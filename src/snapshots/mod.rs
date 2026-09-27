//! Import self-contained SQLite backup uploads; never open a source path or touch media.
//! Supported source contracts: Sonarr 233; Radarr 206 (inline metadata), 242 (split).
//! Raw records (including credentials) are retained privately, never activated or returned.
mod blocklist;
mod custom_formats;
mod history;
mod profiles;
mod providers;
mod readers;
mod tags;

use crate::db::Database;
use libsql::{Connection, Value, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

pub const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 100_000;
const MAX_TABLES: usize = 128;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
// ponytail: one importer per process; per-database permits if concurrent library imports are needed.
static IMPORT_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename = "SnapshotApplication")]
pub enum Application {
    Sonarr,
    Radarr,
}
impl Application {
    fn name(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
        }
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "SnapshotReport")]
pub struct Report {
    pub application: Application,
    pub fingerprint: String,
    pub schema_version: i64,
    pub dry_run: bool,
    pub applied: bool,
    pub mapped: usize,
    pub duplicates: usize,
    pub metadata_backfilled: usize,
    pub conflicts: usize,
    pub missing_file_records: usize,
    pub unsupported: Vec<Unsupported>,
    pub policy: &'static str,
}
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "SnapshotUnsupported")]
pub struct Unsupported {
    pub table: String,
    pub rows: usize,
    pub columns: Vec<String>,
}

// Deliberately static: SQL/parser errors may include source values or credentials.
#[derive(Debug)]
pub struct ImportError(pub &'static str);
impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ImportError {}
impl From<libsql::Error> for ImportError {
    fn from(_: libsql::Error) -> Self {
        Self("snapshot query or destination write failed; transaction not committed")
    }
}
type Result<T> = std::result::Result<T, ImportError>;
type Record = BTreeMap<String, Value>;
struct Table {
    columns: Vec<String>,
    rows: Vec<Record>,
}
struct Source {
    tables: BTreeMap<String, Table>,
    version: i64,
}
#[derive(Clone)]
enum Field {
    Value(Value),
    Reference(&'static str, i64),
}
struct Entity {
    table: &'static str,
    source_id: i64,
    fields: Vec<(&'static str, Field)>,
    // A match on any key demands equality of every imported field; never overwrite.
    keys: Vec<Vec<&'static str>>,
}
struct Plan {
    entities: Vec<Entity>,
    seasons: Vec<(i64, i64, i64)>,
    missing: usize,
    unsupported: Vec<Unsupported>,
}
struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            eprintln!(
                "{{\"level\":\"ERROR\",\"event\":\"snapshot_staging_cleanup_failed\",\"message\":\"Private snapshot staging may remain; inspect owner-only temporary directories\"}}"
            );
        }
    }
}
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        IMPORT_ACTIVE.store(false, Ordering::Release);
    }
}

pub async fn import(
    db: &Database,
    app: Application,
    bytes: Vec<u8>,
    dry_run: bool,
) -> Result<Report> {
    import_inner(db, app, bytes, dry_run, false, None).await
}

/// Explicitly reconstruct supported provider configuration, disabled and untested.
pub async fn import_with_providers(
    db: &Database,
    app: Application,
    bytes: Vec<u8>,
    dry_run: bool,
    key: Option<&crate::providers::CredentialKey>,
) -> Result<Report> {
    import_inner(db, app, bytes, dry_run, true, key).await
}
async fn import_inner(
    db: &Database,
    app: Application,
    bytes: Vec<u8>,
    dry_run: bool,
    reconstruct_providers: bool,
    key: Option<&crate::providers::CredentialKey>,
) -> Result<Report> {
    if !db.permits_private_snapshots() {
        return Err(ImportError(
            "snapshot retention requires a private local database (owner-only permissions)",
        ));
    }
    if IMPORT_ACTIVE
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(ImportError(
            "another snapshot import is active; retry after it completes",
        ));
    }
    let _active = Active;
    if bytes.len() < 100 || bytes.len() > MAX_SNAPSHOT_BYTES || &bytes[..16] != b"SQLite format 3\0"
    {
        return Err(ImportError(
            "upload must be a self-contained SQLite backup of at most 32 MiB",
        ));
    }
    // WAL headers cannot establish completeness without their WAL. Require a backup
    // exported in rollback-journal format, not a copied live main database.
    if bytes[18] != 1 || bytes[19] != 1 {
        return Err(ImportError(
            "WAL-mode uploads are not self-contained; export a consistent SQLite backup in DELETE journal mode",
        ));
    }
    let fingerprint = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
    let (source, _active) = tokio::task::spawn_blocking(move || {
        let result = tokio::runtime::Handle::current().block_on(read_upload(bytes));
        result.map(|source| (source, _active))
    })
    .await
    .map_err(|_| ImportError("snapshot reader failed"))??;
    let mut plan = match app {
        Application::Sonarr => readers::sonarr(&source)?,
        Application::Radarr => readers::radarr(&source)?,
    };
    let tag_plan = tags::read(&source, app, &mut plan.unsupported)?;
    let profile_plan = profiles::read(&source, app, &mut plan.unsupported).await?;
    let blocklist_plan = blocklist::read(&source, app, &mut plan.unsupported)?;
    let history_plan = history::read(&source, app, &mut plan.unsupported)?;
    let provider_plan = if reconstruct_providers {
        providers::read(&source, app, &mut plan.unsupported)?
    } else {
        Vec::new()
    };
    let mut report = Report {
        application: app,
        fingerprint,
        schema_version: source.version,
        dry_run,
        applied: false,
        mapped: 0,
        duplicates: 0,
        metadata_backfilled: 0,
        conflicts: 0,
        missing_file_records: plan.missing,
        unsupported: plan.unsupported,
        policy: "Core library and tags/assignments, supported custom formats, whole profiles and assignments, and supported source History and managed Blocklist facts only. Unsupported records/fields including credentials are retained privately and remain inactive. No clients, jobs or sessions are resumed. Media existence, permissions, mounts and path mappings are unverified; no media was accessed. Upload must be an exported consistent backup, not a live database copy.",
    };
    if reconstruct_providers {
        report.policy = "Supported tags/assignments, custom formats and whole profiles/assignments, source History/Blocklist facts and provider configurations are reconstructed; providers remain disabled and untested, and credentials require the configured encryption key. Unsupported fields and all raw source rows remain private archives. No clients, jobs or sessions are resumed. No network or media access occurs.";
    }
    let conn = db.connect().await?;
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let result = async {
        let prepared =
            profiles::prepare(&tx, &profile_plan, &mut plan.entities, &mut report).await?;
        write(&tx, &source, &plan.entities, &plan.seasons, &mut report).await?;
        profiles::finish(&tx, prepared, &mut report).await?;
        tags::write(&tx, &tag_plan, &mut report).await?;
        history::write(&tx, &history_plan, &mut report).await?;
        blocklist::write(&tx, &blocklist_plan, &mut report).await?;
        providers::write(&tx, &provider_plan, key, &mut report).await
    }
    .await;
    match result {
        Ok(()) if !dry_run && report.conflicts == 0 => {
            tx.commit().await?;
            report.applied = true;
        }
        Ok(()) => {
            tx.rollback().await?;
        }
        Err(error) => {
            tx.rollback().await.map_err(|_| {
                ImportError("snapshot rollback failed; inspect database integrity before retrying")
            })?;
            return Err(error);
        }
    }
    Ok(report)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn identifier(s: &str) -> Result<String> {
    if s.is_empty() || s.len() > 128 || !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(ImportError("unsupported source identifier"));
    }
    Ok(format!("\"{s}\""))
}
async fn read_upload(bytes: Vec<u8>) -> Result<Source> {
    use std::io::Write;
    let directory = std::env::temp_dir().join(format!("hrrdarr-snapshot-{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&directory)
        .map_err(|_| ImportError("cannot create private snapshot staging"))?;
    let _stage = Stage(directory.clone());
    let path = directory.join("snapshot.db");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|_| ImportError("cannot stage snapshot"))?;
    file.write_all(&bytes)
        .map_err(|_| ImportError("cannot stage snapshot"))?;
    drop(file);
    let database = libsql::Builder::new_local(&path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await?;
    let conn = database.connect()?;
    conn.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA ignore_check_constraints=ON;",
    )
    .await?;
    let mut tables = BTreeMap::new();
    let mut objects = conn.query("SELECT name, type, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name", ()).await?;
    while let Some(row) = objects.next().await? {
        let kind: String = row.get(1)?;
        if kind == "index" {
            continue;
        }
        if kind != "table"
            || row
                .get::<String>(2)?
                .to_ascii_uppercase()
                .contains("VIRTUAL TABLE")
        {
            return Err(ImportError(
                "snapshot views, triggers and virtual tables are unsupported",
            ));
        }
        if tables.len() >= MAX_TABLES {
            return Err(ImportError("snapshot exceeds table limit"));
        }
        let name: String = row.get(0)?;
        identifier(&name)?;
        tables.insert(
            name,
            Table {
                columns: Vec::new(),
                rows: Vec::new(),
            },
        );
    }
    // Reject computed columns; expression indexes are valid in Sonarr233 and are never used below.
    for name in tables.keys() {
        let mut columns = conn
            .query(&format!("PRAGMA table_xinfo({})", identifier(name)?), ())
            .await?;
        while let Some(row) = columns.next().await? {
            if row.get::<i64>(6)? != 0 {
                return Err(ImportError(
                    "generated or hidden snapshot columns are unsupported",
                ));
            }
        }
    }
    // quick_check omits index-content verification; ignore_check_constraints prevents
    // execution of source CHECK expressions. Readers/destination validate imported fields.
    let mut integrity = conn.query("PRAGMA quick_check", ()).await?;
    if integrity
        .next()
        .await?
        .ok_or(ImportError("missing integrity result"))?
        .get::<String>(0)?
        != "ok"
        || integrity.next().await?.is_some()
    {
        return Err(ImportError("snapshot integrity check failed"));
    }
    let mut row_count = 0;
    let mut archive_bytes = 0;
    for (name, table) in &mut tables {
        let mut rows = conn
            .query(
                &format!(
                    "SELECT * FROM {} NOT INDEXED LIMIT {}",
                    identifier(name)?,
                    MAX_ROWS + 1
                ),
                (),
            )
            .await?;
        if rows.column_count() > 256 {
            return Err(ImportError("snapshot exceeds column limit"));
        }
        table.columns = (0..rows.column_count())
            .map(|i| rows.column_name(i).unwrap_or("").to_owned())
            .collect();
        for col in &table.columns {
            identifier(col)?;
        }
        while let Some(row) = rows.next().await? {
            row_count += 1;
            if row_count > MAX_ROWS {
                return Err(ImportError("snapshot exceeds row limit"));
            }
            let mut record = BTreeMap::new();
            for (i, col) in table.columns.iter().enumerate() {
                let value = row.get_value(i as i32)?;
                if matches!(&value, Value::Text(s) if s.len() > 1024*1024)
                    || matches!(&value, Value::Blob(b) if b.len() > 1024*1024)
                {
                    return Err(ImportError("snapshot exceeds cell limit"));
                }
                record.insert(col.clone(), value);
            }
            archive_bytes += archive(&record)?.len();
            if archive_bytes > MAX_ARCHIVE_BYTES {
                return Err(ImportError("snapshot exceeds archive limit"));
            }
            table.rows.push(record);
        }
    }
    let versions = tables
        .get("VersionInfo")
        .ok_or(ImportError("snapshot is missing VersionInfo"))?;
    let version = versions
        .rows
        .iter()
        .map(|r| integer(r, "Version"))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .ok_or(ImportError("snapshot has no schema version"))?;
    Ok(Source { tables, version })
}

fn archive(record: &Record) -> Result<String> {
    // Tagged values retain blobs and SQL nulls without guessing a JSON/string contract.
    let mut object = serde_json::Map::new();
    for (key, value) in record {
        let encoded = match value {
            Value::Null => serde_json::json!(["null"]),
            Value::Integer(n) => serde_json::json!(["integer", n]),
            Value::Real(n) if n.is_finite() => serde_json::json!(["real", n]),
            Value::Text(s) => serde_json::json!(["text", s]),
            Value::Blob(b) => serde_json::json!(["blob_hex", hex(b)]),
            _ => return Err(ImportError("unsupported non-finite source value")),
        };
        object.insert(key.clone(), encoded);
    }
    serde_json::to_string(&object).map_err(|_| ImportError("cannot archive source row"))
}
fn integer(row: &Record, col: &str) -> Result<i64> {
    match row.get(col) {
        Some(Value::Integer(n)) => Ok(*n),
        _ => Err(ImportError("required source integer is absent or invalid")),
    }
}
fn text<'a>(row: &'a Record, col: &str) -> Result<&'a str> {
    match row.get(col) {
        Some(Value::Text(s)) if !s.trim().is_empty() && !s.contains('\0') => Ok(s),
        _ => Err(ImportError("required source text is absent or invalid")),
    }
}
fn positive(row: &Record, col: &str) -> Result<i64> {
    let n = integer(row, col)?;
    if n <= 0 {
        return Err(ImportError("source identity must be positive"));
    }
    Ok(n)
}
fn boolean(row: &Record, col: &str) -> Result<i64> {
    let n = integer(row, col)?;
    if !(0..=1).contains(&n) {
        return Err(ImportError("source monitoring value must be boolean"));
    }
    Ok(n)
}
fn optional_id(row: &Record, col: &str) -> Result<Option<i64>> {
    match row.get(col) {
        Some(Value::Null) | Some(Value::Integer(0)) => Ok(None),
        Some(Value::Integer(n)) if *n > 0 => Ok(Some(*n)),
        _ => Err(ImportError("invalid source file identity")),
    }
}

async fn write(
    conn: &Connection,
    source: &Source,
    entities: &[Entity],
    seasons: &[(i64, i64, i64)],
    report: &mut Report,
) -> Result<()> {
    let app = report.application.name();
    conn.execute("INSERT INTO snapshot_imports (application,fingerprint,schema_version,episode_metadata_version) VALUES (?1,?2,?3,1) ON CONFLICT DO NOTHING", params![app, report.fingerprint.clone(), source.version]).await?;
    let old_metadata=conn.query("SELECT episode_metadata_version FROM snapshot_imports WHERE application=?1 AND fingerprint=?2",params![app,report.fingerprint.clone()]).await?.next().await?.ok_or(ImportError("snapshot mapping disappeared"))?.get::<i64>(0)?==0;
    let mut ids = BTreeMap::new();
    let mut seasons_done = false;
    // Predecessor-schema fixture writers also use this core before retirement exists.
    let has_retirement = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='rss_candidate_imports'",
            (),
        )
        .await?
        .next()
        .await?
        .is_some();
    for entity in entities {
        if entity.table == "episodes" && !seasons_done {
            write_seasons(conn, seasons, &ids, report).await?;
            seasons_done = true;
        }
        let mut fields = Vec::new();
        let mut unresolved = false;
        for (col, field) in &entity.fields {
            let value = match field {
                Field::Value(v) => v.clone(),
                Field::Reference(table, id) => match ids.get(&(*table, *id)) {
                    Some(id) => Value::Integer(*id),
                    None => {
                        unresolved = true;
                        break;
                    }
                },
            };
            fields.push((*col, value));
        }
        if unresolved {
            report.conflicts += 1;
            continue;
        }
        let columns = fields.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(",");
        let values = fields.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
        let predicates = entity
            .keys
            .iter()
            .map(|key| {
                key.iter()
                    .map(|col| format!("t.{col} = i.{col}"))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            })
            .map(|s| format!("({s})"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut matching = conn.query(&format!("WITH incoming({columns}) AS (VALUES ({})) SELECT t.id,{} FROM {} t, incoming i WHERE {}", (1..=values.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(","), fields.iter().map(|(c,_)| format!("t.{c}")).collect::<Vec<_>>().join(","), entity.table, predicates), values.clone()).await?;
        let mut matches = Vec::new();
        while let Some(row) = matching.next().await? {
            let equal = values.iter().enumerate().all(|(i, v)| {
                row.get_value((i + 1) as i32)
                    .is_ok_and(|actual| actual == *v)
            });
            let can_backfill = old_metadata
                && entity.table == "episodes"
                && fields.iter().enumerate().all(|(i, (col, value))| {
                    row.get_value((i + 1) as i32).is_ok_and(|actual| {
                        if crate::episodes::METADATA_COLUMNS.contains(col) {
                            actual == Value::Null
                        } else {
                            actual == *value
                        }
                    })
                });
            matches.push((row.get::<i64>(0)?, equal, can_backfill));
        }
        let mapped = conn.query("SELECT destination_id FROM snapshot_mappings WHERE application=?1 AND fingerprint=?2 AND destination_table=?3 AND source_id=?4", params![app, report.fingerprint.clone(), entity.table, entity.source_id]).await?.next().await?;
        // A replacement archive is not an active snapshot file candidate, even when
        // an old provenance mapping survives or the source path now matches its archive.
        if has_retirement && entity.table == "episode_files" {
            let mut ids = matches.iter().map(|(id, _, _)| *id).collect::<Vec<_>>();
            if let Some(row) = &mapped {
                ids.push(row.get::<i64>(0)?);
            }
            let mut retired = false;
            for id in ids {
                if conn.query("SELECT 1 FROM rss_candidate_imports WHERE old_episode_file_id=? AND retirement_state='quarantined' LIMIT 1", [id]).await?.next().await?.is_some() {
                    retired = true;
                    break;
                }
            }
            if retired {
                report.conflicts += 1;
                continue;
            }
        }
        let id = match matches.as_slice() {
            [(id, true, _)]
                if mapped
                    .as_ref()
                    .is_none_or(|r| r.get::<i64>(0).ok() == Some(*id)) =>
            {
                report.duplicates += 1;
                *id
            }
            [(id, false, true)]
                if mapped
                    .as_ref()
                    .is_some_and(|r| r.get::<i64>(0).ok() == Some(*id)) =>
            {
                let metadata = fields
                    .iter()
                    .filter(|(c, _)| crate::episodes::METADATA_COLUMNS.contains(c))
                    .collect::<Vec<_>>();
                let mut values = metadata.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
                values.push(Value::Integer(*id));
                conn.execute(
                    &format!(
                        "UPDATE episodes SET {} WHERE id=?",
                        metadata
                            .iter()
                            .map(|(c, _)| format!("{c}=?"))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    values,
                )
                .await?;
                report.metadata_backfilled += 1;
                *id
            }
            [] if mapped.is_none() => {
                conn.execute(
                    &format!(
                        "INSERT INTO {} ({columns}) VALUES ({})",
                        entity.table,
                        (1..=values.len())
                            .map(|i| format!("?{i}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    values,
                )
                .await?;
                report.mapped += 1;
                conn.last_insert_rowid()
            }
            _ => {
                report.conflicts += 1;
                continue;
            }
        };
        ids.insert((entity.table, entity.source_id), id);
        conn.execute("INSERT INTO snapshot_mappings (application,fingerprint,destination_table,source_id,destination_id) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING", params![app,report.fingerprint.clone(),entity.table,entity.source_id,id]).await?;
    }
    if !seasons_done {
        write_seasons(conn, seasons, &ids, report).await?;
    }
    readers::verify_remote_order(conn, entities, &ids, report).await?;
    for (table, data) in &source.tables {
        for (i, row) in data.rows.iter().enumerate() {
            conn.execute("INSERT INTO snapshot_records (application,fingerprint,source_table,ordinal,record_json) VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",params![app,report.fingerprint.clone(),table.clone(),i as i64,archive(row)?]).await?;
        }
    }
    conn.execute("UPDATE snapshot_imports SET episode_metadata_version=1 WHERE application=?1 AND fingerprint=?2",params![app,report.fingerprint.clone()]).await?;
    Ok(())
}
async fn write_seasons(
    conn: &Connection,
    seasons: &[(i64, i64, i64)],
    ids: &BTreeMap<(&str, i64), i64>,
    report: &mut Report,
) -> Result<()> {
    for &(series, number, monitored) in seasons {
        let Some(&series_id) = ids.get(&("series", series)) else {
            report.conflicts += 1;
            continue;
        };
        let existing = conn
            .query(
                "SELECT monitored FROM seasons WHERE series_id=?1 AND number=?2",
                params![series_id, number],
            )
            .await?
            .next()
            .await?;
        match existing {
            Some(row) if row.get::<i64>(0)? == monitored => report.duplicates += 1,
            Some(_) => report.conflicts += 1,
            None => {
                conn.execute(
                    "INSERT INTO seasons(series_id,number,monitored) VALUES (?1,?2,?3)",
                    params![series_id, number, monitored],
                )
                .await?;
                report.mapped += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
// Construct prior core+archive state only; fixtures independently choose omitted ancillary tables.
pub(crate) async fn write_core_snapshot_fixture(
    conn: &Connection,
    app: Application,
    bytes: Vec<u8>,
) -> Result<()> {
    let fingerprint = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
    let source = read_upload(bytes).await?;
    let plan = match app {
        Application::Sonarr => readers::sonarr(&source)?,
        Application::Radarr => readers::radarr(&source)?,
    };
    let mut report = Report {
        application: app,
        fingerprint,
        schema_version: source.version,
        dry_run: false,
        applied: false,
        mapped: 0,
        duplicates: 0,
        metadata_backfilled: 0,
        conflicts: 0,
        missing_file_records: plan.missing,
        unsupported: plan.unsupported,
        policy: "test predecessor",
    };
    write(conn, &source, &plan.entities, &plan.seasons, &mut report).await?;
    assert_eq!(report.conflicts, 0);
    Ok(())
}

#[cfg(test)]
pub(crate) static IMPORT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
