//! Snapshot facts are inactive until references resolve; native intent always wins.
use super::*;
use std::collections::BTreeSet;
const INVALID: ImportError = ImportError("invalid source collection facts");
const MAX_ID: i64 = 9_007_199_254_740_991;
const FIELDS: &[&str] = &[
    "Id",
    "TmdbId",
    "Title",
    "SortTitle",
    "Overview",
    "Images",
    "Monitored",
    "QualityProfileId",
    "RootFolderPath",
    "MinimumAvailability",
    "SearchOnAdd",
    "LastInfoSync",
    "Added",
    "Tags",
];

/// Validate recognized text before libsql's C-string TEXT extraction can lose bytes.
pub(super) async fn preflight(c: &Connection, table: &str, bytes: &mut usize) -> Result<()> {
    if !matches!(
        table,
        "Collections" | "Movies" | "MovieMetadata" | "ImportExclusions"
    ) {
        return Ok(());
    }
    let version: i64 = c
        .query("SELECT max(Version) FROM VersionInfo", ())
        .await?
        .next()
        .await?
        .ok_or(INVALID)?
        .get(0)?;
    if (version == 206 && matches!(table, "Collections" | "MovieMetadata"))
        || (version == 242 && table == "Movies")
    {
        return Ok(());
    }
    let recognized: &[&str] = match table {
        "Collections" => FIELDS,
        "Movies" => &["Collection"],
        "MovieMetadata" => &["CollectionTmdbId", "CollectionTitle"],
        "ImportExclusions" => &["Id", "TmdbId", "MovieTitle", "MovieYear"],
        _ => return Ok(()),
    };
    let mut columns = c
        .query(&format!("PRAGMA table_xinfo({})", identifier(table)?), ())
        .await?;
    let mut selected = Vec::new();
    while let Some(r) = columns.next().await? {
        let name: String = r.get(1)?;
        if recognized.contains(&name.as_str()) {
            selected.push(name);
        }
    }
    drop(columns);
    if selected.is_empty() {
        return Ok(());
    }
    let encoding: String = c
        .query("PRAGMA encoding", ())
        .await?
        .next()
        .await?
        .ok_or(INVALID)?
        .get(0)?;
    let projection = selected.iter().map(|name| {
        let q = identifier(name)?;
        Ok(format!("typeof({q}),length(CAST({q} AS BLOB)),CASE WHEN typeof({q})='text' THEN substr(CAST({q} AS BLOB),1,1048577) ELSE NULL END"))
    }).collect::<Result<Vec<_>>>()?.join(",");
    let mut rows = c
        .query(
            &format!(
                "SELECT {projection} FROM {} NOT INDEXED LIMIT {}",
                identifier(table)?,
                MAX_ROWS + 1
            ),
            (),
        )
        .await?;
    let mut count = 0;
    while let Some(r) = rows.next().await? {
        count += 1;
        if count > MAX_ROWS {
            return Err(INVALID);
        }
        for i in 0..selected.len() {
            let col = (i * 3) as i32;
            if r.get::<String>(col)? != "text" {
                continue;
            }
            let len: i64 = r.get(col + 1)?;
            if !(0..=1048576).contains(&len) {
                return Err(INVALID);
            }
            *bytes += len as usize;
            if *bytes > MAX_ARCHIVE_BYTES {
                return Err(INVALID);
            }
            let raw: Vec<u8> = r.get(col + 2)?;
            if raw.len() != len as usize {
                return Err(INVALID);
            }
            let decoded = match encoding.as_str() {
                "UTF-8" => String::from_utf8(raw).map_err(|_| INVALID)?,
                "UTF-16le" | "UTF-16be" => {
                    if raw.len() % 2 != 0 {
                        return Err(INVALID);
                    }
                    let units = raw
                        .chunks_exact(2)
                        .map(|v| {
                            if encoding == "UTF-16le" {
                                u16::from_le_bytes([v[0], v[1]])
                            } else {
                                u16::from_be_bytes([v[0], v[1]])
                            }
                        })
                        .collect::<Vec<_>>();
                    String::from_utf16(&units).map_err(|_| INVALID)?
                }
                _ => return Err(INVALID),
            };
            if decoded.contains('\0') || decoded.len() > 1048576 {
                return Err(INVALID);
            }
        }
    }
    Ok(())
}
#[derive(Default)]
pub(super) struct Collections {
    facts: BTreeMap<i64, Fact>,
    members: Vec<(i64, i64)>,
    exclusions: Vec<(i64, Option<String>, Option<i64>)>,
}
#[derive(Default)]
struct Fact {
    source_id: Option<i64>,
    title: String,
    sort: Option<String>,
    overview: Option<String>,
    images: Option<String>,
    added: Option<String>,
    synced: Option<String>,
    monitored: bool,
    profile: Option<i64>,
    root: Option<String>,
    availability: Option<String>,
    search: Option<bool>,
    tags: Vec<i64>,
    supported: bool,
}
fn optional_text(r: &Record, key: &str, max: usize) -> Result<Option<String>> {
    match r.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Text(s)) if s.len() <= max && !s.contains('\0') => Ok(Some(s.clone())),
        _ => Err(INVALID),
    }
}
fn id(r: &Record, key: &str) -> Result<i64> {
    let n = positive(r, key)?;
    if n > MAX_ID { Err(INVALID) } else { Ok(n) }
}
fn optional_number(r: &Record, key: &str) -> Result<Option<i64>> {
    match r.get(key) {
        None | Some(Value::Null) | Some(Value::Integer(0)) => Ok(None),
        Some(Value::Integer(n)) if (1..=MAX_ID).contains(n) => Ok(Some(*n)),
        _ => Err(INVALID),
    }
}
fn optional_bool(r: &Record, key: &str) -> Result<Option<bool>> {
    match r.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(n)) if *n == 0 || *n == 1 => Ok(Some(*n == 1)),
        _ => Err(INVALID),
    }
}
fn timestamp(r: &Record, key: &str) -> Result<Option<String>> {
    let Some(s) = optional_text(r, key, 128)? else {
        return Ok(None);
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(&s)
        .map(|v| v.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f").map(|v| v.and_utc())
        })
        .map_err(|_| INVALID)?;
    use chrono::Datelike;
    if !(1..=9999).contains(&parsed.year()) {
        return Err(INVALID);
    }
    Ok(Some(parsed.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
}
fn issue(report: &mut Vec<Unsupported>, table: &str, reason: &str) {
    profiles::issue(report, table, reason);
}
fn consume(u: &mut Vec<Unsupported>, table: &str, fields: &[&str]) {
    for item in u.iter_mut().filter(|v| v.table == table) {
        item.columns.retain(|c| !fields.contains(&c.as_str()));
    }
    u.retain(|v| !v.columns.is_empty());
}
fn images(r: &Record, u: &mut Vec<Unsupported>) -> Result<Option<String>> {
    let Some(raw) = optional_text(r, "Images", 65536)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|_| INVALID)?;
    let items = value.as_array().ok_or(INVALID)?;
    if items.len() > 32 {
        return Err(INVALID);
    }
    let mut output = Vec::new();
    for item in items {
        let obj = item.as_object().ok_or(INVALID)?;
        let cover = obj.get("coverType").and_then(|v| v.as_str());
        let url = obj.get("remoteUrl").and_then(|v| v.as_str());
        let (Some(cover), Some(url)) = (cover, url) else {
            issue(
                u,
                "Collections",
                "Images: unsupported source cover facts retained privately",
            );
            return Ok(None);
        };
        if cover.trim().is_empty()
            || cover.len() > 64
            || cover.chars().any(char::is_control)
            || url.trim().is_empty()
            || url.len() > 2048
            || url.chars().any(char::is_control)
        {
            return Err(INVALID);
        }
        if obj
            .keys()
            .any(|k| !matches!(k.as_str(), "coverType" | "remoteUrl" | "url"))
        {
            issue(
                u,
                "Collections",
                "Images: unknown fields retained privately",
            );
        }
        output.push(serde_json::json!({"cover_type":cover,"source_url":url}));
    }
    let encoded = serde_json::to_string(&output).map_err(|_| INVALID)?;
    if encoded.len() > 65536 {
        return Err(INVALID);
    }
    Ok(Some(encoded))
}
pub(super) fn read(
    source: &Source,
    app: Application,
    u: &mut Vec<Unsupported>,
) -> Result<Collections> {
    let mut plan = Collections::default();
    if !matches!(app, Application::Radarr) {
        return Ok(plan);
    }
    if source.version == 242 {
        if let Some(table) = source.tables.get("Collections") {
            if table.rows.len() > 1024 {
                return Err(INVALID);
            }
            let mut ids = BTreeSet::new();
            for r in &table.rows {
                let remote = id(r, "TmdbId")?;
                let source_id = id(r, "Id")?;
                if !ids.insert(source_id) {
                    return Err(INVALID);
                }
                let title = text(r, "Title")?.to_string();
                if title.len() > 1024 {
                    return Err(INVALID);
                }
                let mut supported = true;
                let availability = match r.get("MinimumAvailability") {
                    None | Some(Value::Null) => None,
                    Some(Value::Text(v)) => match v.as_str() {
                        "tba" => Some("tba"),
                        "announced" => Some("announced"),
                        "inCinemas" => Some("in_cinemas"),
                        "released" => Some("released"),
                        _ => {
                            supported = false;
                            None
                        }
                    },
                    Some(Value::Integer(n)) => match n {
                        0 => Some("tba"),
                        1 => Some("announced"),
                        2 => Some("in_cinemas"),
                        3 => Some("released"),
                        _ => {
                            supported = false;
                            None
                        }
                    },
                    _ => return Err(INVALID),
                }
                .map(str::to_owned);
                let tags = match optional_text(r, "Tags", 65536)? {
                    None => Vec::new(),
                    Some(raw) => {
                        let values: Vec<i64> = serde_json::from_str(&raw).map_err(|_| INVALID)?;
                        if values.len() > 200
                            || values.iter().any(|v| !(1..=MAX_ID).contains(v))
                            || values.iter().collect::<BTreeSet<_>>().len() != values.len()
                        {
                            return Err(INVALID);
                        }
                        values
                    }
                };
                if !supported {
                    issue(
                        u,
                        "Collections",
                        "unsupported availability; policy inactive",
                    );
                }
                let fact = Fact {
                    source_id: Some(source_id),
                    title,
                    sort: optional_text(r, "SortTitle", 1024)?,
                    overview: optional_text(r, "Overview", 65536)?,
                    images: images(r, u)?,
                    added: timestamp(r, "Added")?,
                    synced: timestamp(r, "LastInfoSync")?,
                    monitored: optional_bool(r, "Monitored")?.unwrap_or(false),
                    profile: optional_number(r, "QualityProfileId")?,
                    root: optional_text(r, "RootFolderPath", 4096)?,
                    availability,
                    search: optional_bool(r, "SearchOnAdd")?,
                    tags,
                    supported,
                };
                if plan.facts.insert(remote, fact).is_some() {
                    return Err(INVALID);
                }
            }
            consume(u, "Collections", FIELDS);
        }
        if let Some(table) = source.tables.get("MovieMetadata") {
            for r in &table.rows {
                let Some(remote) = optional_number(r, "CollectionTmdbId")? else {
                    continue;
                };
                if !plan.facts.contains_key(&remote) {
                    let title =
                        optional_text(r, "CollectionTitle", 1024)?.filter(|s| !s.trim().is_empty());
                    let Some(title) = title else {
                        issue(
                            u,
                            "MovieMetadata",
                            "collection title unresolved; association inactive",
                        );
                        continue;
                    };
                    plan.facts.insert(
                        remote,
                        Fact {
                            title,
                            ..Default::default()
                        },
                    );
                    issue(
                        u,
                        "Collections",
                        "missing collection policy; defaults unknown",
                    );
                }
                plan.members.push((remote, id(r, "Id")?));
            }
            consume(u, "MovieMetadata", &["CollectionTmdbId", "CollectionTitle"]);
        }
    } else if let Some(table) = source.tables.get("Movies") {
        for r in &table.rows {
            let Some(raw) = optional_text(r, "Collection", 65536)? else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|_| INVALID)?;
            if value.is_null() {
                continue;
            }
            let obj = value.as_object().ok_or(INVALID)?;
            let remote = obj.get("tmdbId").and_then(|v| v.as_i64()).ok_or(INVALID)?;
            // Source206 uses zero as absent collection, not a native collection identity.
            if remote == 0 {
                continue;
            }
            if !(1..=MAX_ID).contains(&remote) {
                return Err(INVALID);
            }
            let Some(title) = obj
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            else {
                issue(
                    u,
                    "Movies",
                    "Collection: missing name; association inactive",
                );
                continue;
            };
            if title.len() > 1024 || title.contains('\0') {
                return Err(INVALID);
            }
            if obj.keys().any(|k| !matches!(k.as_str(), "tmdbId" | "name")) {
                issue(u, "Movies", "Collection: unknown fields retained privately");
            }
            if let Some(prior) = plan.facts.get(&remote) {
                if prior.title != title {
                    return Err(INVALID);
                }
            } else {
                plan.facts.insert(
                    remote,
                    Fact {
                        title: title.into(),
                        ..Default::default()
                    },
                );
            }
            plan.members.push((remote, id(r, "Id")?));
        }
        consume(u, "Movies", &["Collection"]);
    }
    if let Some(table) = source.tables.get("ImportExclusions") {
        let mut seen = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        for r in &table.rows {
            let remote = id(r, "TmdbId")?;
            if !seen.insert(remote) || !source_ids.insert(id(r, "Id")?) {
                return Err(INVALID);
            }
            let year = optional_number(r, "MovieYear")?;
            if year.is_some_and(|y| y > 9999) {
                return Err(INVALID);
            }
            let title = optional_text(r, "MovieTitle", 1024)?.filter(|s| !s.trim().is_empty());
            plan.exclusions.push((remote, title, year));
        }
        consume(
            u,
            "ImportExclusions",
            &["Id", "TmdbId", "MovieTitle", "MovieYear"],
        );
    }
    if plan.facts.len() > 1024 || plan.members.len() > 100000 {
        return Err(INVALID);
    }
    let mut counts = BTreeMap::new();
    for (remote, _) in &plan.members {
        let n = counts.entry(remote).or_insert(0);
        *n += 1;
        if *n > 1000 {
            return Err(INVALID);
        }
    }
    Ok(plan)
}
async fn mapped(c: &Connection, report: &Report, table: &str, source: i64) -> Result<Option<i64>> {
    Ok(c.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table=? AND source_id=?",params![report.application.name(),report.fingerprint.clone(),table,source]).await?.next().await?.map(|r|r.get(0)).transpose()?)
}
pub(super) async fn write(c: &Connection, plan: &Collections, report: &mut Report) -> Result<()> {
    if !matches!(report.application, Application::Radarr) {
        return Ok(());
    }
    if c.is_autocommit() {
        return Err(INVALID);
    }
    let activated=c.query("SELECT collection_version FROM snapshot_imports WHERE application='radarr' AND fingerprint=?",[report.fingerprint.clone()]).await?.next().await?.ok_or(INVALID)?.get::<i64>(0)?==1;
    // An already-applied snapshot never reactivates deleted policy, associations or exclusions.
    if activated {
        return Ok(());
    }
    let mut owners = BTreeMap::new();
    for (remote, fact) in &plan.facts {
        let intent = c
            .query(
                "SELECT local_edit,removed FROM movie_collection_intents WHERE tmdb_id=?",
                [*remote],
            )
            .await?
            .next()
            .await?;
        if let Some(r) = intent {
            if r.get::<i64>(0)? != 0 || r.get::<i64>(1)? != 0 {
                issue(
                    &mut report.unsupported,
                    "Collections",
                    "native collection intent retained; source policy inactive",
                );
                continue;
            }
        }
        let existing=c.query("SELECT c.id,c.metadata_revision,c.title,coalesce(s.local_edit,0) FROM movie_collections c LEFT JOIN movie_collection_settings s ON s.collection_id=c.id WHERE c.tmdb_id=?",[*remote]).await?.next().await?;
        let (collection, created) = if let Some(r) = existing {
            let id: i64 = r.get(0)?;
            if r.get::<i64>(1)? > 0 || r.get::<i64>(3)? != 0 {
                issue(
                    &mut report.unsupported,
                    "Collections",
                    "newer native collection retained; source policy inactive",
                );
                continue;
            }
            if r.get::<String>(2)? != fact.title {
                report.conflicts += 1;
                continue;
            }
            report.duplicates += 1;
            (id, false)
        } else {
            c.execute("INSERT INTO movie_collections(tmdb_id,title,sort_title,overview,images_json,added,last_info_sync)VALUES(?,?,?,?,?,?,?)",params![*remote,fact.title.clone(),fact.sort.clone(),fact.overview.clone(),fact.images.clone(),fact.added.clone(),fact.synced.clone()]).await?;
            report.mapped += 1;
            (c.last_insert_rowid(), true)
        };
        c.execute(
            "INSERT INTO movie_collection_intents(tmdb_id)VALUES(?) ON CONFLICT DO NOTHING",
            [*remote],
        )
        .await?;
        if let Some(source) = fact.source_id {
            if mapped(c, report, "movie_collections", source)
                .await?
                .is_some_and(|id| id != collection)
            {
                report.conflicts += 1;
                continue;
            }
            c.execute("INSERT INTO snapshot_mappings(application,fingerprint,destination_table,source_id,destination_id)VALUES('radarr',?,'movie_collections',?,?) ON CONFLICT DO NOTHING",params![report.fingerprint.clone(),source,collection]).await?;
        }
        if created
            || c.query(
                "SELECT 1 FROM movie_collection_settings WHERE collection_id=?",
                [collection],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            let profile = if let Some(source) = fact.profile {
                mapped(c, report, "quality_profiles", source).await?
            } else {
                None
            };
            let root = if let Some(path) = &fact.root {
                if let Some(path) = crate::library::normalized_path(path) {
                    c.query("SELECT r.id FROM root_folders r JOIN snapshot_mappings m ON m.destination_id=r.id AND m.destination_table='root_folders' WHERE m.application='radarr' AND m.fingerprint=? AND r.media_type='movies' AND r.path=?",params![report.fingerprint.clone(),path]).await?.next().await?.map(|r|r.get::<i64>(0)).transpose()?
                } else {
                    None
                }
            } else {
                None
            };
            let mut tags = Vec::new();
            let mut resolved = fact.supported;
            for source in &fact.tags {
                if let Some(tag) = mapped(c, report, "tags", *source).await? {
                    tags.push(tag)
                } else {
                    resolved = false
                }
            }
            if fact.profile.is_some() && profile.is_none() || fact.root.is_some() && root.is_none()
            {
                resolved = false
            }
            let complete = resolved
                && root.is_some()
                && profile.is_some()
                && fact.availability.is_some()
                && fact.search.is_some();
            if !complete {
                issue(
                    &mut report.unsupported,
                    "Collections",
                    "unresolved or absent defaults; collection policy inactive",
                );
            }
            c.execute("INSERT INTO movie_collection_settings(collection_id,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add)VALUES(?,?,?,?,?,?)",params![collection,i64::from(fact.monitored&&complete),root,profile,fact.availability.clone(),fact.search.map(i64::from)]).await?;
            // A partly mapped tag set must never broaden automatic matching/defaults.
            if resolved {
                for tag in tags {
                    c.execute(
                        "INSERT INTO movie_collection_tags(collection_id,tag_id)VALUES(?,?)",
                        params![collection, tag],
                    )
                    .await?;
                }
            }
        } else {
            issue(
                &mut report.unsupported,
                "Collections",
                "existing collection defaults preserved",
            );
        }
        owners.insert(*remote, collection);
    }
    for (remote, source) in &plan.members {
        let Some(collection) = owners.get(remote) else {
            continue;
        };
        let Some(metadata) = mapped(c, report, "movie_metadata", *source).await? else {
            issue(
                &mut report.unsupported,
                "MovieMetadata",
                "unresolved catalog collection owner",
            );
            continue;
        };
        let prior = c
            .query(
                "SELECT collection_id,origin FROM movie_collection_members WHERE metadata_id=?",
                [metadata],
            )
            .await?;
        let mut prior = prior;
        let mut same = false;
        let mut native = false;
        while let Some(r) = prior.next().await? {
            if r.get::<i64>(0)? == *collection {
                same = true
            } else {
                native = true
            }
            if r.get::<String>(1)? != "snapshot" {
                native = true
            }
        }
        drop(prior);
        if same {
            report.duplicates += 1;
            continue;
        }
        if native {
            issue(
                &mut report.unsupported,
                "MovieMetadata",
                "existing collection association retained",
            );
            continue;
        }
        c.execute("INSERT INTO movie_collection_members(collection_id,metadata_id,origin,metadata_revision)SELECT ?,?,'snapshot',metadata_revision FROM movie_collections WHERE id=?",params![*collection,metadata,*collection]).await?;
        report.mapped += 1;
    }
    for (remote, title, year) in &plan.exclusions {
        if let Some(r) = c
            .query(
                "SELECT title,year,local_edit FROM movie_import_exclusions WHERE tmdb_id=?",
                [*remote],
            )
            .await?
            .next()
            .await?
        {
            if r.get::<i64>(2)? != 0 {
                issue(
                    &mut report.unsupported,
                    "ImportExclusions",
                    "native exclusion intent retained",
                );
            } else if r.get::<Option<String>>(0)? != *title || r.get::<Option<i64>>(1)? != *year {
                issue(
                    &mut report.unsupported,
                    "ImportExclusions",
                    "existing exclusion facts retained",
                );
            }
            report.duplicates += 1;
            continue;
        }
        c.execute(
            "INSERT INTO movie_import_exclusions(tmdb_id,title,year)VALUES(?,?,?)",
            params![*remote, title.clone(), *year],
        )
        .await?;
        report.mapped += 1;
    }
    c.execute("UPDATE snapshot_imports SET collection_version=1 WHERE application='radarr' AND fingerprint=?",[report.fingerprint.clone()]).await?;
    Ok(())
}
