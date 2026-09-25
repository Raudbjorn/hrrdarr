//! Source-backed management, never a claim that release decisions are enforced.
use super::*;
use std::collections::BTreeSet;
const MAX_ENTRIES: usize = 10_000;
const MAX_EPISODE_LINKS: usize = 100_000;
#[derive(serde::Serialize)]
pub(super) struct Entry {
    id: i64,
    target: i64,
    episodes: Vec<i64>,
    date: String,
    published: Option<String>,
    title: String,
    protocol: Option<i64>,
    size: Option<i64>,
    quality: Option<i64>,
    revision: Option<String>,
    languages: Option<String>,
}
fn issue(list: &mut Vec<Unsupported>, field: &str) {
    if let Some(item) = list
        .iter_mut()
        .find(|u| u.table == "Blocklist" && u.columns == [field])
    {
        item.rows += 1;
    } else {
        list.push(Unsupported {
            table: "Blocklist".into(),
            rows: 1,
            columns: vec![field.into()],
        });
    }
}
fn json(row: &Record, field: &str) -> Result<Option<serde_json::Value>> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Text(s)) => serde_json::from_str(s)
            .map(Some)
            .map_err(|_| ImportError("invalid source Blocklist JSON")),
        _ => Err(ImportError("invalid source Blocklist JSON type")),
    }
}
fn optional_integer(row: &Record, field: &str) -> Result<Option<i64>> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(n)) => Ok(Some(*n)),
        _ => Err(ImportError("invalid source Blocklist integer")),
    }
}
pub(super) fn read(
    source: &Source,
    app: Application,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Vec<Entry>> {
    let Some(table) = source.tables.get("Blocklist") else {
        return Ok(vec![]);
    };
    if table.rows.len() > MAX_ENTRIES {
        return Err(ImportError("source Blocklist row limit exceeded"));
    }
    let tv = matches!(app, Application::Sonarr);
    let target_column = if tv { "SeriesId" } else { "MovieId" };
    for field in ["Id", target_column, "Date", "SourceTitle"] {
        if !table.columns.iter().any(|c| c == field) {
            return Err(ImportError(
                "source Blocklist schema does not match supported layout",
            ));
        }
    }
    if tv && !table.columns.iter().any(|c| c == "EpisodeIds") {
        return Err(ImportError("source Blocklist episode set absent"));
    }
    unsupported.retain(|u| u.table != "Blocklist");
    let public = [
        "Id",
        target_column,
        "Date",
        "SourceTitle",
        "PublishedDate",
        "Size",
        "Protocol",
        "Quality",
        "Languages",
    ];
    let extra: Vec<_> = table
        .columns
        .iter()
        .filter(|c| !public.contains(&c.as_str()) && !(tv && c.as_str() == "EpisodeIds"))
        .cloned()
        .collect();
    if !extra.is_empty() {
        unsupported.push(Unsupported {
            table: "Blocklist".into(),
            rows: table.rows.len(),
            columns: extra,
        });
    }
    let source_targets: BTreeSet<i64> = source
        .tables
        .get(if tv { "Series" } else { "Movies" })
        .into_iter()
        .flat_map(|t| t.rows.iter())
        .map(|r| positive(r, "Id"))
        .collect::<Result<_>>()?;
    let source_episodes: BTreeMap<i64, i64> = if tv {
        source
            .tables
            .get("Episodes")
            .into_iter()
            .flat_map(|t| t.rows.iter())
            .map(|r| Ok((positive(r, "Id")?, positive(r, "SeriesId")?)))
            .collect::<Result<_>>()?
    } else {
        BTreeMap::new()
    };
    let mut seen = BTreeSet::new();
    let mut links = 0usize;
    let mut result = vec![];
    for row in &table.rows {
        let mut supported = true;
        let id = positive(row, "Id")?;
        if id > 9007199254740991 || !seen.insert(id) {
            return Err(ImportError(
                "invalid or duplicate source Blocklist identity",
            ));
        }
        let target = positive(row, target_column)?;
        let title = text(row, "SourceTitle")?.to_owned();
        if title.len() > 1024 || title.chars().any(char::is_control) {
            issue(unsupported, "SourceTitle.unsupported");
            supported = false;
        }
        let date = history::date(text(row, "Date")?)
            .map_err(|_| ImportError("invalid source Blocklist date"))?;
        let published = match row.get("PublishedDate") {
            None | Some(Value::Null) => None,
            Some(Value::Text(s)) => {
                Some(history::date(s).map_err(|_| ImportError("invalid source Blocklist date"))?)
            }
            _ => return Err(ImportError("invalid source Blocklist date")),
        };
        let protocol = optional_integer(row, "Protocol")?;
        if protocol.is_some_and(|n| !(0..=2).contains(&n)) {
            issue(unsupported, "Protocol.unsupported");
            supported = false;
        }
        let size = optional_integer(row, "Size")?;
        if size.is_some_and(|n| !(0..=9007199254740991).contains(&n)) {
            return Err(ImportError("invalid source Blocklist size"));
        }
        let mut episodes = vec![];
        if tv {
            let value = json(row, "EpisodeIds")?
                .ok_or(ImportError("invalid source Blocklist episode set"))?;
            let array = value
                .as_array()
                .ok_or(ImportError("invalid source Blocklist episode set"))?;
            links = links
                .checked_add(array.len())
                .ok_or(ImportError("source Blocklist episode limit exceeded"))?;
            if links > MAX_EPISODE_LINKS || array.len() > 10_000 {
                return Err(ImportError("source Blocklist episode limit exceeded"));
            }
            for value in array {
                episodes.push(
                    value
                        .as_i64()
                        .filter(|n| (1..=9007199254740991).contains(n))
                        .ok_or(ImportError("invalid source Blocklist episode identity"))?,
                );
            }
            episodes.sort_unstable();
            if episodes.windows(2).any(|p| p[0] == p[1]) {
                return Err(ImportError("duplicate source Blocklist episode identity"));
            }
        }
        if !source_targets.contains(&target)
            || episodes
                .iter()
                .any(|id| source_episodes.get(id) != Some(&target))
        {
            issue(unsupported, "target.unresolved");
            supported = false;
        }
        let (quality, revision) = match json(row, "Quality")? {
            None | Some(serde_json::Value::Null) => (None, None),
            Some(value) => {
                let object = value
                    .as_object()
                    .ok_or(ImportError("invalid source Blocklist quality"))?;
                if object
                    .keys()
                    .any(|k| !matches!(k.as_str(), "quality" | "revision"))
                {
                    issue(unsupported, "Quality.unsupported");
                    supported = false;
                }
                let quality = object
                    .get("quality")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or(ImportError("invalid source Blocklist quality"))?;
                let revision = match object.get("revision") {
                    None | Some(serde_json::Value::Null) => serde_json::Value::Null,
                    Some(v) => {
                        let r = v
                            .as_object()
                            .ok_or(ImportError("invalid source Blocklist revision"))?;
                        if r.keys()
                            .any(|k| !matches!(k.as_str(), "version" | "real" | "isRepack"))
                        {
                            issue(unsupported, "Quality.unsupported");
                            supported = false;
                        }
                        serde_json::json!({"version":r.get("version"),"real":r.get("real"),"is_repack":r.get("isRepack")})
                    }
                };
                let (id, revision) = crate::media_files::quality(
                    &serde_json::json!({"quality_id":quality,"revision":revision}),
                )
                .map_err(|_| ImportError("invalid source Blocklist quality"))?;
                (Some(id), revision)
            }
        };
        let languages = match json(row, "Languages")? {
            None => None,
            Some(mut value) => {
                if let Some(a) = value.as_array_mut() {
                    for v in a {
                        if v.is_null() {
                            *v = serde_json::json!(0);
                        }
                    }
                }
                if let Some(array) = value.as_array() {
                    let mut seen = BTreeSet::new();
                    if array.len() > 64 {
                        return Err(ImportError("invalid source Blocklist languages"));
                    }
                    for v in array {
                        let id = v
                            .as_i64()
                            .filter(|n| *n >= 0)
                            .ok_or(ImportError("invalid source Blocklist languages"))?;
                        if !seen.insert(id) {
                            return Err(ImportError("invalid source Blocklist languages"));
                        }
                    }
                }
                if value.as_array().is_some_and(|a| {
                    a.iter()
                        .any(|v| v.as_i64().is_some_and(|n| n > if tv { 52 } else { 57 }))
                }) {
                    issue(unsupported, "Languages.unsupported");
                    supported = false;
                    None
                } else {
                    crate::media_files::languages(&value, if tv { "tv" } else { "movies" })
                        .map_err(|_| ImportError("invalid source Blocklist languages"))?
                }
            }
        };
        if !supported {
            continue;
        }
        result.push(Entry {
            id,
            target,
            episodes,
            date,
            published,
            title,
            protocol,
            size,
            quality,
            revision,
            languages,
        });
    }
    Ok(result)
}
async fn mapping(c: &Connection, r: &Report, table: &str, id: i64) -> Result<Option<i64>> {
    c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table=? AND source_id=?",params![r.application.name(),r.fingerprint.clone(),table,id]).await?.next().await?.map(|r|r.get(0).map_err(ImportError::from)).transpose()
}
pub(super) async fn write(c: &Connection, entries: &[Entry], r: &mut Report) -> Result<()> {
    let app = r.application.name();
    let tv = matches!(r.application, Application::Sonarr);
    let domain = if tv { "tv" } else { "movies" };
    let prior = c
        .query(
            "SELECT blocklist_version FROM snapshot_imports WHERE application=? AND fingerprint=?",
            params![app, r.fingerprint.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("snapshot Blocklist provenance absent"))?
        .get::<i64>(0)?;
    let mut qualities = BTreeSet::new();
    let mut rows = c
        .query(
            "SELECT quality_id FROM quality_definitions WHERE media_type=?",
            [domain],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        qualities.insert(row.get::<i64>(0)?);
    }
    drop(rows);
    const FIELDS: &str = "media_type,series_id,movie_id,occurred_at,published_at,source_title,protocol,size,quality_id,quality_revision_json,languages_json";
    for e in entries {
        if e.quality.is_some_and(|q| !qualities.contains(&q)) {
            issue(&mut r.unsupported, "Quality.unsupported");
            continue;
        }
        let serialized = serde_json::to_vec(e)
            .map_err(|_| ImportError("source Blocklist facts serialization failed"))?;
        let digest = hex(ring::digest::digest(&ring::digest::SHA256, &serialized).as_ref());
        let provenance=c.query("SELECT facts_digest,removed_at FROM snapshot_blocklist WHERE application=? AND fingerprint=? AND source_id=?",params![app,r.fingerprint.clone(),e.id]).await?.next().await?;
        if let Some(p) = &provenance {
            if p.get::<String>(0)? != digest {
                r.conflicts += 1;
                continue;
            }
            if p.get::<Option<String>>(1)?.is_some() {
                r.duplicates += 1;
                continue;
            }
        } else if prior == 1 {
            r.conflicts += 1;
            continue;
        }
        let Some(target) = mapping(c, r, if tv { "series" } else { "movies" }, e.target).await?
        else {
            if provenance.is_some() {
                r.conflicts += 1;
            } else {
                issue(&mut r.unsupported, "target.unresolved");
            }
            continue;
        };
        let mut episodes = Vec::with_capacity(e.episodes.len());
        let mut unresolved = false;
        for source in &e.episodes {
            let Some(id) = mapping(c, r, "episodes", *source).await? else {
                unresolved = true;
                break;
            };
            if c.query(
                "SELECT 1 FROM episodes WHERE id=? AND series_id=?",
                params![id, target],
            )
            .await?
            .next()
            .await?
            .is_none()
            {
                unresolved = true;
                break;
            }
            episodes.push(id);
        }
        if unresolved {
            if provenance.is_some() {
                r.conflicts += 1;
            } else {
                issue(&mut r.unsupported, "target.unresolved");
            }
            continue;
        }
        episodes.sort_unstable();
        let values: Vec<Value> = vec![
            domain.into(),
            if tv { target.into() } else { Value::Null },
            if tv { Value::Null } else { target.into() },
            e.date.clone().into(),
            e.published.clone().into(),
            e.title.clone().into(),
            e.protocol.into(),
            e.size.into(),
            e.quality.into(),
            e.revision.clone().into(),
            e.languages.clone().into(),
        ];
        let old=c.query(&format!("SELECT {FIELDS} FROM blocklist_entries WHERE application=? AND fingerprint=? AND source_id=?"),params![app,r.fingerprint.clone(),e.id]).await?.next().await?;
        if let Some(old) = old {
            let mut links=c.query("SELECT episode_id FROM blocklist_episodes WHERE application=? AND fingerprint=? AND source_id=? ORDER BY episode_id LIMIT 10001",params![app,r.fingerprint.clone(),e.id]).await?;
            let mut actual = vec![];
            while let Some(row) = links.next().await? {
                actual.push(row.get::<i64>(0)?);
            }
            if provenance.is_some()
                && actual == episodes
                && values
                    .iter()
                    .enumerate()
                    .all(|(i, v)| old.get_value(i as i32).is_ok_and(|a| a == *v))
            {
                r.duplicates += 1;
            } else {
                r.conflicts += 1;
            }
        } else if provenance.is_some() {
            r.conflicts += 1;
        } else {
            c.execute("INSERT INTO snapshot_blocklist(application,fingerprint,source_id,facts_digest) VALUES(?,?,?,?)",params![app,r.fingerprint.clone(),e.id,digest]).await?;
            let mut params: Vec<Value> =
                vec![app.into(), r.fingerprint.clone().into(), e.id.into()];
            params.extend(values);
            c.execute(&format!("INSERT INTO blocklist_entries(application,fingerprint,source_id,{FIELDS}) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)"),params).await?;
            for episode in episodes {
                c.execute("INSERT INTO blocklist_episodes(application,fingerprint,source_id,series_id,episode_id) VALUES(?,?,?,?,?)",params![app,r.fingerprint.clone(),e.id,target,episode]).await?;
            }
            r.mapped += 1;
        }
    }
    c.execute(
        "UPDATE snapshot_imports SET blocklist_version=1 WHERE application=? AND fingerprint=?",
        params![app, r.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
