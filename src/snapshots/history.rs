//! Source event attestations only. Raw dictionaries stay in the private snapshot archive.
use super::*;
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::BTreeSet;

pub(super) struct Event {
    id: i64,
    target: i64,
    series: Option<i64>,
    date: String,
    kind: &'static str,
    source_kind: i64,
    title: Option<String>,
    download: Option<String>,
    quality: Option<i64>,
    revision: Option<String>,
    languages: Option<String>,
}
fn issue(unsupported: &mut Vec<Unsupported>, column: &str) {
    if let Some(entry) = unsupported
        .iter_mut()
        .find(|u| u.table == "History" && u.columns == [column])
    {
        entry.rows += 1;
    } else {
        unsupported.push(Unsupported {
            table: "History".into(),
            rows: 1,
            columns: vec![column.into()],
        });
    }
}
fn optional_text(
    row: &Record,
    field: &str,
    limit: usize,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Option<String>> {
    match row.get(field) {
        None => {
            issue(unsupported, &format!("{field}.missing"));
            Ok(None)
        }
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value))
            if value.len() <= limit && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.clone()))
        }
        Some(Value::Text(_)) => {
            issue(unsupported, field);
            Ok(None)
        }
        _ => Err(ImportError("invalid source History text field")),
    }
}
fn json(
    row: &Record,
    field: &str,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Option<serde_json::Value>> {
    match row.get(field) {
        None => {
            issue(unsupported, &format!("{field}.missing"));
            Ok(None)
        }
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => serde_json::from_str(value)
            .map(Some)
            .map_err(|_| ImportError("invalid source History JSON")),
        _ => Err(ImportError("invalid source History JSON type")),
    }
}
fn date(value: &str) -> Result<String> {
    if value.len() > 40
        || value
            .split_once('.')
            .is_some_and(|(_, s)| s.bytes().take_while(u8::is_ascii_digit).count() > 9)
    {
        return Err(ImportError("invalid source History date precision"));
    }
    // Both pinned UtcConverter contracts interpret stored SQLite DateTime values as UTC.
    let parsed = DateTime::parse_from_rfc3339(value)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
                .iter()
                .find_map(|f| {
                    NaiveDateTime::parse_from_str(value, f)
                        .ok()
                        .map(|d| d.and_utc())
                })
        });
    parsed
        .and_then(|d| crate::history::canonical_timestamp(&d))
        .ok_or(ImportError("invalid source History date"))
}
fn event(app: Application, code: i64) -> Option<&'static str> {
    match (app, code) {
        (_, 1) => Some("grabbed"),
        (_, 3) => Some("download_folder_imported"),
        (_, 4) => Some("download_failed"),
        (Application::Sonarr, 2) => Some("series_folder_imported"),
        (Application::Sonarr, 5) | (Application::Radarr, 6) => Some("file_deleted"),
        (Application::Sonarr, 6) | (Application::Radarr, 8) => Some("file_renamed"),
        (Application::Sonarr, 7) | (Application::Radarr, 9) => Some("download_ignored"),
        (Application::Radarr, 7) => Some("movie_folder_imported"),
        _ => None,
    }
}
pub(super) fn read(
    source: &Source,
    app: Application,
    unsupported: &mut Vec<Unsupported>,
) -> Result<Vec<Event>> {
    let Some(table) = source.tables.get("History") else {
        return Ok(vec![]);
    };
    let tv = matches!(app, Application::Sonarr);
    for column in if tv {
        &["Id", "EpisodeId", "SeriesId", "Date", "EventType"][..]
    } else {
        &["Id", "MovieId", "Date", "EventType"][..]
    } {
        if !table.columns.iter().any(|c| c == column) {
            return Err(ImportError(
                "source History schema does not match supported layout",
            ));
        }
    }
    unsupported.retain(|u| u.table != "History");
    let known = [
        "Id",
        "EpisodeId",
        "SeriesId",
        "MovieId",
        "Date",
        "EventType",
        "SourceTitle",
        "DownloadId",
        "Quality",
        "Languages",
    ];
    let extra: Vec<_> = table
        .columns
        .iter()
        .filter(|c| {
            !known.contains(&c.as_str())
                || (tv && c.as_str() == "MovieId")
                || (!tv && matches!(c.as_str(), "EpisodeId" | "SeriesId"))
        })
        .cloned()
        .collect();
    if !extra.is_empty() {
        unsupported.push(Unsupported {
            table: "History".into(),
            rows: table.rows.len(),
            columns: extra,
        });
    }
    let mut targets = BTreeMap::new();
    for row in &source
        .tables
        .get(if tv { "Episodes" } else { "Movies" })
        .ok_or(ImportError("source History targets absent"))?
        .rows
    {
        targets.insert(
            positive(row, "Id")?,
            if tv {
                Some(positive(row, "SeriesId")?)
            } else {
                None
            },
        );
    }
    let mut ids = BTreeSet::new();
    let mut events = Vec::new();
    for row in &table.rows {
        let id = positive(row, "Id")?;
        if id > 9007199254740991 || !ids.insert(id) {
            return Err(ImportError("invalid or duplicate source History identity"));
        }
        let code = integer(row, "EventType")?;
        let Some(kind) = event(app, code) else {
            issue(unsupported, "EventType.unsupported");
            continue;
        };
        let target = integer(row, if tv { "EpisodeId" } else { "MovieId" })?;
        let series = if tv {
            Some(integer(row, "SeriesId")?)
        } else {
            None
        };
        if target <= 0 || series.is_some_and(|id| id <= 0) {
            issue(unsupported, "target.unresolved");
            continue;
        }
        let Some(actual_series) = targets.get(&target) else {
            issue(unsupported, "target.unresolved");
            continue;
        };
        if *actual_series != series {
            return Err(ImportError(
                "source History series does not own its episode",
            ));
        }
        let timestamp = date(text(row, "Date")?)?;
        let title = optional_text(row, "SourceTitle", 1024, unsupported)?;
        let mut download = optional_text(row, "DownloadId", 256, unsupported)?;
        if download.as_ref().is_some_and(|v| {
            v.is_empty()
                || !v
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
        }) {
            issue(unsupported, "DownloadId");
            download = None;
        }
        let (quality, revision) = match json(row, "Quality", unsupported)? {
            None | Some(serde_json::Value::Null) => (None, None),
            Some(value) => {
                let object = value
                    .as_object()
                    .ok_or(ImportError("invalid source History quality object"))?;
                if object
                    .keys()
                    .any(|k| !matches!(k.as_str(), "quality" | "revision"))
                {
                    issue(unsupported, "Quality.extra_fields");
                }
                let quality = object
                    .get("quality")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or(ImportError("invalid source History quality identity"))?;
                let revision = match object.get("revision") {
                    None | Some(serde_json::Value::Null) => serde_json::Value::Null,
                    Some(value) => {
                        let r = value
                            .as_object()
                            .ok_or(ImportError("invalid source History quality revision"))?;
                        if r.keys()
                            .any(|k| !matches!(k.as_str(), "version" | "real" | "isRepack"))
                        {
                            issue(unsupported, "Quality.revision.extra_fields");
                        }
                        serde_json::json!({"version":r.get("version"),"real":r.get("real"),"is_repack":r.get("isRepack")})
                    }
                };
                let (id, revision) = crate::media_files::quality(
                    &serde_json::json!({"quality_id":quality,"revision":revision}),
                )
                .map_err(|_| ImportError("invalid source History quality"))?;
                (Some(id), revision)
            }
        };
        let languages = match json(row, "Languages", unsupported)? {
            None => None,
            Some(mut value) => {
                if let Some(array) = value.as_array_mut() {
                    for entry in array {
                        if entry.is_null() {
                            *entry = serde_json::json!(0);
                        }
                    }
                }
                let max = if tv { 52 } else { 57 };
                if value
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v.as_i64().is_some_and(|id| id > max)))
                {
                    issue(unsupported, "Languages.unsupported");
                    None
                } else {
                    crate::media_files::languages(&value, if tv { "tv" } else { "movies" })
                        .map_err(|_| ImportError("invalid source History languages"))?
                }
            }
        };
        events.push(Event {
            id,
            target,
            series,
            date: timestamp,
            kind,
            source_kind: code,
            title,
            download,
            quality,
            revision,
            languages,
        });
    }
    Ok(events)
}
async fn mapping(conn: &Connection, report: &Report, table: &str, id: i64) -> Result<Option<i64>> {
    conn.query("SELECT destination_id FROM snapshot_mappings WHERE application=? AND fingerprint=? AND destination_table=? AND source_id=?",params![report.application.name(),report.fingerprint.clone(),table,id]).await?.next().await?.map(|r|r.get(0).map_err(ImportError::from)).transpose()
}
pub(super) async fn write(conn: &Connection, events: &[Event], report: &mut Report) -> Result<()> {
    let app = report.application.name();
    let prior = conn
        .query(
            "SELECT history_version FROM snapshot_imports WHERE application=? AND fingerprint=?",
            params![app, report.fingerprint.clone()],
        )
        .await?
        .next()
        .await?
        .ok_or(ImportError("snapshot history provenance absent"))?
        .get::<i64>(0)?;
    let tv = matches!(report.application, Application::Sonarr);
    const FIELDS: &str = "media_type,episode_id,movie_id,occurred_at,event_type,source_event_type,source_title,download_id,quality_id,quality_revision_json,languages_json";
    for event in events {
        let Some(target) = mapping(
            conn,
            report,
            if tv { "episodes" } else { "movies" },
            event.target,
        )
        .await?
        else {
            report.conflicts += 1;
            continue;
        };
        if let Some(series) = event.series {
            let series = mapping(conn, report, "series", series).await?;
            if series.is_none()
                || conn
                    .query(
                        "SELECT 1 FROM episodes WHERE id=? AND series_id=?",
                        params![target, series],
                    )
                    .await?
                    .next()
                    .await?
                    .is_none()
            {
                report.conflicts += 1;
                continue;
            }
        }
        let mut quality = event.quality;
        let mut revision = event.revision.clone();
        if let Some(id) = quality {
            if conn
                .query(
                    "SELECT 1 FROM quality_definitions WHERE media_type=? AND quality_id=?",
                    params![if tv { "tv" } else { "movies" }, id],
                )
                .await?
                .next()
                .await?
                .is_none()
            {
                issue(&mut report.unsupported, "Quality.unsupported");
                quality = None;
                revision = None;
            }
        }
        let values: Vec<Value> = vec![
            Value::Text(if tv { "episode" } else { "movie" }.into()),
            if tv { target.into() } else { Value::Null },
            if tv { Value::Null } else { target.into() },
            event.date.clone().into(),
            event.kind.into(),
            event.source_kind.into(),
            event.title.clone().into(),
            event.download.clone().into(),
            quality.into(),
            revision.into(),
            event.languages.clone().into(),
        ];
        let row=conn.query(&format!("SELECT {FIELDS} FROM snapshot_history_events WHERE application=? AND fingerprint=? AND source_id=?"),params![app,report.fingerprint.clone(),event.id]).await?.next().await?;
        if let Some(row) = row {
            if values
                .iter()
                .enumerate()
                .all(|(i, v)| row.get_value(i as i32).is_ok_and(|actual| actual == *v))
            {
                report.duplicates += 1;
            } else {
                report.conflicts += 1;
            }
        } else if prior == 1 {
            report.conflicts += 1;
        } else {
            let mut parameters: Vec<Value> = vec![
                app.into(),
                report.fingerprint.clone().into(),
                event.id.into(),
            ];
            parameters.extend(values);
            conn.execute(&format!("INSERT INTO snapshot_history_events(application,fingerprint,source_id,{FIELDS}) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)"),parameters).await?;
            report.mapped += 1;
        }
    }
    conn.execute(
        "UPDATE snapshot_imports SET history_version=1 WHERE application=? AND fingerprint=?",
        params![app, report.fingerprint.clone()],
    )
    .await?;
    Ok(())
}
