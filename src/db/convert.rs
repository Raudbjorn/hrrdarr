//! Explicit offline conversion of databases created by pre-merge PR #32 (#47).
//!
//! Pre-merge PR #32 (`eb08c2ba9adf0df14dd7be2fb6d933f26ffb3889`) shipped its own migration history:
//! `notifications` (63), `rename_commands` (65), the collection snapshot (69), the label clients
//! (70-75) and `indexer_breadth` (76) differ from the selected main history, so the history
//! validator refuses such a database at startup and must keep doing so. Nothing here relaxes it and
//! no history row is rewritten.
//!
//! The conversion never opens the source for writing. It copies the (cleanly stopped) database,
//! proves the copy's ledger is byte-for-byte the pinned pre-merge ledger, builds a new database
//! through the normal migration runner, moves the data over inside one transaction and renames the
//! result into place only after the integrity checks and a reopen pass. A failure leaves no
//! destination and an untouched source.
use super::{Database, Error, MIGRATIONS, checksum};
use libsql::{Connection, TransactionBehavior};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// First ledger entry at which the pre-merge history differs from main's.
const FIRST_DIVERGENT_VERSION: usize = 63;

/// Pre-merge entries that are not main's. Every other ordinal 1..=76 is main's entry at the same
/// ordinal, which the ledger comparison still checks byte for byte.
const PREMERGE_ENTRIES: [(usize, &str, &str); 10] = [
    (
        63,
        "notifications",
        include_str!("premerge_pr32/63_notifications.sql"),
    ),
    (
        65,
        "rename_commands",
        include_str!("premerge_pr32/65_rename_commands.sql"),
    ),
    (
        69,
        "collection_snapshot",
        include_str!("premerge_pr32/69_collection_snapshot.sql"),
    ),
    (70, "deluge", include_str!("premerge_pr32/70_deluge.sql")),
    (
        71,
        "rtorrent",
        include_str!("premerge_pr32/71_rtorrent.sql"),
    ),
    (
        72,
        "utorrent",
        include_str!("premerge_pr32/72_utorrent.sql"),
    ),
    (73, "aria2", include_str!("premerge_pr32/73_aria2.sql")),
    (74, "flood", include_str!("premerge_pr32/74_flood.sql")),
    (
        75,
        "downloadstation",
        include_str!("premerge_pr32/75_downloadstation.sql"),
    ),
    (
        76,
        "indexer_breadth",
        include_str!("premerge_pr32/76_indexer_breadth.sql"),
    ),
];
const PREMERGE_LEN: usize = 76;

/// Tables whose shape differs between the lineages; each has an explicit mapping below.
const MAPPED_TABLES: [&str; 5] = [
    "movie_collections",
    "movie_collection_members",
    "movie_collection_tags",
    "notification_deliveries",
    "snapshot_imports",
];

/// The pinned pre-merge ledger: `(name, sql)` per ordinal, for the conversion's tests and tooling.
pub fn premerge_ledger() -> Vec<(&'static str, &'static str)> {
    (1..=PREMERGE_LEN)
        .map(|ordinal| {
            PREMERGE_ENTRIES
                .iter()
                .find(|(n, _, _)| *n == ordinal)
                .map_or_else(|| MIGRATIONS[ordinal - 1], |&(_, name, sql)| (name, sql))
        })
        .collect()
}

/// What a finished conversion moved. Counts are rows in the converted database.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConversionReport {
    /// Last ledger version of the source (63..=76).
    pub source_version: usize,
    pub destination_version: usize,
    pub tables: BTreeMap<String, u64>,
    pub collection_settings: u64,
    /// Monitored collections whose policy was incomplete; they are converted unmonitored, exactly as
    /// main's own collection-authority migration treats the same state.
    pub collections_unmonitored: u64,
    /// Deliveries not yet resolved; they stay pending and are never marked delivered.
    pub pending_deliveries: u64,
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn scalar(conn: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(super::scalar(conn, sql).await?)
}

async fn names(conn: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    let mut rows = conn.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

async fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>, Error> {
    let mut rows = conn
        .query(&format!("PRAGMA {schema}.table_info({})", quote(table)), ())
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(1)?);
    }
    out.sort();
    Ok(out)
}

async fn tables(conn: &Connection, schema: &str) -> Result<Vec<String>, Error> {
    names(
        conn,
        &format!(
            "SELECT name FROM {schema}.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name!='schema_migrations' ORDER BY name"
        ),
    )
    .await
}

/// The source ledger must be exactly a prefix (63..=76 entries) of the pinned pre-merge ledger.
async fn verify_source_ledger(conn: &Connection, schema: &str) -> Result<usize, Error> {
    if scalar(
        conn,
        &format!("SELECT count(*) FROM {schema}.sqlite_schema WHERE type='table' AND name='schema_migrations'"),
    )
    .await?
        == 0
    {
        return Err("source has no migration history; it is not a pre-merge PR #32 database".into());
    }
    let pinned = premerge_ledger();
    let mut rows = conn
        .query(
            &format!("SELECT version, name, checksum, sql FROM {schema}.schema_migrations ORDER BY version"),
            (),
        )
        .await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        let version: i64 = row.get(0)?;
        let Some(&(name, sql)) = pinned.get(count) else {
            return Err(format!(
                "source history has version {version} beyond the pinned pre-merge ledger of {PREMERGE_LEN}; use the matching application version"
            )
            .into());
        };
        if version != (count + 1) as i64
            || row.get::<String>(1)? != name
            || row.get::<String>(2)? != checksum(sql)
            || row.get::<String>(3)? != sql
        {
            return Err(format!(
                "source history differs from the pinned pre-merge PR #32 ledger at version {version}; refusing to convert an unrecognized or modified history"
            )
            .into());
        }
        count += 1;
    }
    if count < FIRST_DIVERGENT_VERSION {
        return Err(format!(
            "source history ends at version {count}, before the lineages diverge at {FIRST_DIVERGENT_VERSION}; open it with the current application instead"
        )
        .into());
    }
    Ok(count)
}

/// A shorter pre-merge ledger is first brought to the pre-merge head the way the pre-merge runner
/// would have: its own pinned entries, applied in order to the copy. One conversion path then covers
/// every supported prefix, and no pre-merge data transformation is skipped or re-derived.
async fn replay_to_premerge_head(copy: &Path) -> Result<usize, Error> {
    let raw = libsql::Builder::new_local(copy).build().await?;
    let conn = raw.connect()?;
    check(&conn, "main").await?;
    let version = verify_source_ledger(&conn, "main").await?;
    if version < PREMERGE_LEN {
        conn.execute("PRAGMA foreign_keys = OFF", ()).await?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        for (index, (name, sql)) in premerge_ledger().into_iter().enumerate().skip(version) {
            tx.execute_batch(sql).await.map_err(|e| {
                format!(
                    "replaying pre-merge migration {} ({name}) failed: {e}",
                    index + 1
                )
            })?;
            tx.execute(
                "INSERT INTO schema_migrations (version, name, checksum, sql) VALUES (?1, ?2, ?3, ?4)",
                libsql::params![(index + 1) as i64, name, checksum(sql), sql],
            )
            .await?;
        }
        if (67..69).contains(&version) {
            // Entry 69 introduces the ownership markers at 0. A policy written before they existed has
            // unknown provenance, so it is protected as a local edit (main's 0077 does the same) rather
            // than left replaceable by a later snapshot import.
            tx.execute(
                "UPDATE movie_collections SET local_edit=1 WHERE monitored!=0 OR search_on_add!=0 OR root_folder_id IS NOT NULL OR quality_profile_id IS NOT NULL OR minimum_availability IS NOT NULL OR EXISTS(SELECT 1 FROM movie_collection_tags t WHERE t.collection_id=movie_collections.id)",
                (),
            )
            .await?;
        }
        check(&tx, "main").await?;
        tx.commit().await?;
    }
    Ok(version)
}

/// Verify source/destination schemas line up table by table so nothing is silently dropped.
async fn plan(conn: &Connection) -> Result<Vec<String>, Error> {
    let source = tables(conn, "old").await?;
    let target = tables(conn, "main").await?;
    let mut straight = Vec::new();
    for table in &source {
        if !target.contains(table) {
            return Err(
                format!("source table {table} has no destination; refusing to drop data").into(),
            );
        }
        if MAPPED_TABLES.contains(&table.as_str()) {
            continue;
        }
        let (old, new) = (
            columns(conn, "old", table).await?,
            columns(conn, "main", table).await?,
        );
        // Main 0079 adds facts the historical runner never recorded; preserve NULL defaults.
        let appended = match table.as_str() {
            "episodes" | "movies" => Some("native_last_search_at"),
            "search_commands" => Some("dispatch_completed_at"),
            _ => None,
        };
        let mut expected = old
            .iter()
            .map(String::as_str)
            .chain(appended)
            .collect::<Vec<_>>();
        expected.sort_unstable();
        if new.iter().map(String::as_str).collect::<Vec<_>>() != expected {
            return Err(format!(
                "table {table} differs between the lineages and has no mapping; refusing to guess"
            )
            .into());
        }
        straight.push(table.clone());
    }
    Ok(straight)
}

fn column_list(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| quote(c))
        .collect::<Vec<_>>()
        .join(",")
}

async fn copy_straight(conn: &Connection, table: &str) -> Result<(), Error> {
    let list = column_list(&columns(conn, "old", table).await?);
    // REPLACE keeps the seeded rows of migrations a shorter source never ran, and lets the source's
    // edits to rows both lineages seed (settings, definitions) win.
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO main.{t}({list}) SELECT {list} FROM old.{t}",
            t = quote(table)
        ),
        (),
    )
    .await?;
    Ok(())
}

struct Mapped {
    settings: u64,
    unmonitored: u64,
}

/// Inline collection policy (pre-merge 0066/0069) becomes the settings/tag authority of main's 0077.
async fn copy_collections(conn: &Connection) -> Result<Mapped, Error> {
    let have = columns(conn, "old", "movie_collections").await?;
    let has = |c: &str| have.iter().any(|h| h == c);
    let pick = |c: &str, otherwise: &str| {
        if has(c) {
            format!("c.{c}")
        } else {
            otherwise.to_owned()
        }
    };
    // Catalog facts move as-is; the retired policy columns keep their (empty) defaults.
    conn.execute(
        &format!(
            "INSERT INTO main.movie_collections(id,tmdb_id,title,overview,last_info_sync,sort_title,images_json,added,metadata_revision) \
             SELECT c.id,c.tmdb_id,c.title,{},{},{},{},{},{} FROM old.movie_collections c",
            pick("overview", "NULL"),
            pick("last_info_sync", "NULL"),
            pick("sort_title", "NULL"),
            pick("images_json", "NULL"),
            pick("added", "NULL"),
            pick("metadata_revision", "0"),
        ),
        (),
    )
    .await?;
    let mut mapped = Mapped {
        settings: 0,
        unmonitored: 0,
    };
    if has("monitored") {
        let (local_edit, snapshot_owned) = ("c.local_edit", "c.snapshot_owned");
        let marked = "c.local_edit=1 OR c.snapshot_owned=1 OR ";
        let complete = "c.root_folder_id IS NOT NULL AND c.quality_profile_id IS NOT NULL AND c.minimum_availability IS NOT NULL";
        mapped.unmonitored = scalar(
            conn,
            &format!("SELECT count(*) FROM old.movie_collections c WHERE c.monitored=1 AND NOT ({complete})"),
        )
        .await? as u64;
        conn.execute(
            &format!(
                "INSERT INTO main.movie_collection_settings(collection_id,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned) \
                 SELECT c.id,CASE WHEN {complete} THEN c.monitored ELSE 0 END,c.root_folder_id,c.quality_profile_id,c.minimum_availability,c.search_on_add,{local_edit},{snapshot_owned} \
                 FROM old.movie_collections c \
                 WHERE {marked}c.monitored!=0 OR c.search_on_add!=0 OR c.root_folder_id IS NOT NULL OR c.quality_profile_id IS NOT NULL \
                 OR c.minimum_availability IS NOT NULL OR EXISTS(SELECT 1 FROM old.movie_collection_tags t WHERE t.collection_id=c.id)"
            ),
            (),
        )
        .await?;
        mapped.settings =
            scalar(conn, "SELECT count(*) FROM main.movie_collection_settings").await? as u64;
    }
    if tables(conn, "old")
        .await?
        .contains(&"movie_collection_tags".to_owned())
    {
        conn.execute(
            "INSERT INTO main.movie_collection_tags(collection_id,tag_id,media_type) \
             SELECT collection_id,tag_id,media_type FROM old.movie_collection_tags",
            (),
        )
        .await?;
    }
    if tables(conn, "old")
        .await?
        .contains(&"movie_collection_members".to_owned())
    {
        let origin = if columns(conn, "old", "movie_collection_members")
            .await?
            .iter()
            .any(|c| c == "origin")
        {
            "m.origin"
        } else {
            "'legacy'"
        };
        // A member's revision must equal its collection's, which main enforces on every later write.
        conn.execute(
            &format!(
                "INSERT INTO main.movie_collection_members(collection_id,metadata_id,origin,metadata_revision) \
                 SELECT m.collection_id,m.metadata_id,{origin},(SELECT c.metadata_revision FROM main.movie_collections c WHERE c.id=m.collection_id) \
                 FROM old.movie_collection_members m"
            ),
            (),
        )
        .await?;
    }
    Ok(mapped)
}

/// Domain events retain their recorded scope. Terminal system deliveries use current scopes only
/// as a historical display projection; pending system deliveries must be refused below.
const DELIVERY_DOMAIN: &str = "CASE WHEN e.scope IN ('tv','movies') THEN e.scope ELSE COALESCE(\
    (SELECT s.media_type FROM old.notification_scopes s WHERE s.notification_id=d.notification_id \
      AND CASE e.kind WHEN 'health_issue' THEN s.on_health_issue WHEN 'health_restored' THEN s.on_health_restored \
        WHEN 'application_update' THEN s.on_application_update ELSE 0 END=1 ORDER BY s.media_type DESC LIMIT 1),\
    (SELECT s.media_type FROM old.notification_scopes s WHERE s.notification_id=d.notification_id ORDER BY s.media_type DESC LIMIT 1)) END";

async fn copy_deliveries(conn: &Connection) -> Result<u64, Error> {
    // The legacy row did not record its fan-out domain. Even one current scope cannot prove
    // which scope existed when it was queued, so never resume a guessed system delivery.
    let ambiguous = scalar(conn, "SELECT count(*) FROM old.notification_deliveries d JOIN old.notification_events e ON e.sequence=d.event_sequence WHERE d.state='pending' AND e.scope='system'").await?;
    if ambiguous > 0 {
        return Err(format!("{ambiguous} pending system notification deliveries have no recorded fan-out domain; conversion cannot safely resume them; no data was changed").into());
    }
    let unassignable = scalar(
        conn,
        &format!(
            "SELECT count(*) FROM old.notification_deliveries d JOIN old.notification_events e ON e.sequence=d.event_sequence WHERE {DELIVERY_DOMAIN} IS NULL"
        ),
    )
    .await?;
    if unassignable > 0 {
        return Err(format!(
            "{unassignable} notification deliveries belong to notifications with no scope, so their domain cannot be determined; no data was changed"
        )
        .into());
    }
    // Pre-merge deliveries record no acknowledged request parts and no claim token. Neither is
    // invented: parts stay empty, and a claim left by the stopped instance becomes claimable.
    conn.execute(
        &format!(
            "INSERT INTO main.notification_deliveries(id,event_sequence,notification_id,media_type,state,attempts,next_attempt_at,lease_until,last_error,sent_parts,created_at,completed_at,lease_owner) \
             SELECT d.id,d.event_sequence,d.notification_id,{DELIVERY_DOMAIN},d.state,d.attempts,d.next_attempt_at,\
             CASE WHEN d.state='pending' THEN 0 ELSE d.lease_until END,d.last_error,'[]',d.created_at,d.completed_at,NULL \
             FROM old.notification_deliveries d JOIN old.notification_events e ON e.sequence=d.event_sequence"
        ),
        (),
    )
    .await?;
    Ok(scalar(
        conn,
        "SELECT count(*) FROM main.notification_deliveries WHERE state='pending'",
    )
    .await? as u64)
}

/// Source columns are a subset; main's additions (`provider_authority_version`) take their defaults,
/// which records that no provider authority was ever imported from a snapshot.
async fn copy_snapshot_imports(conn: &Connection) -> Result<(), Error> {
    let (old, new) = (
        columns(conn, "old", "snapshot_imports").await?,
        columns(conn, "main", "snapshot_imports").await?,
    );
    if let Some(extra) = old.iter().find(|c| !new.contains(c)) {
        return Err(format!("snapshot_imports.{extra} has no destination column").into());
    }
    let list = column_list(&old);
    conn.execute(
        &format!(
            "INSERT INTO main.snapshot_imports({list}) SELECT {list} FROM old.snapshot_imports"
        ),
        (),
    )
    .await?;
    Ok(())
}

/// AUTOINCREMENT counters must not move backwards past ids the source already issued.
async fn copy_sequences(conn: &Connection) -> Result<(), Error> {
    if scalar(
        conn,
        "SELECT count(*) FROM old.sqlite_schema WHERE name='sqlite_sequence'",
    )
    .await?
        == 0
    {
        return Ok(());
    }
    conn.execute(
        "UPDATE main.sqlite_sequence SET seq=max(seq,COALESCE((SELECT o.seq FROM old.sqlite_sequence o WHERE o.name=main.sqlite_sequence.name),0))",
        (),
    )
    .await?;
    conn.execute(
        "INSERT INTO main.sqlite_sequence(name,seq) SELECT o.name,o.seq FROM old.sqlite_sequence o WHERE o.name NOT IN (SELECT name FROM main.sqlite_sequence)",
        (),
    )
    .await?;
    Ok(())
}

async fn check(conn: &Connection, schema: &str) -> Result<(), Error> {
    let mut rows = conn
        .query(&format!("PRAGMA {schema}.integrity_check"), ())
        .await?;
    let first = rows
        .next()
        .await?
        .ok_or("integrity check returned no result")?;
    if first.get::<String>(0)? != "ok" || rows.next().await?.is_some() {
        return Err(format!("{schema} database integrity check failed").into());
    }
    if conn
        .query(&format!("PRAGMA {schema}.foreign_key_check"), ())
        .await?
        .next()
        .await?
        .is_some()
    {
        return Err(format!("{schema} database foreign key check failed; an orphaned relationship cannot be converted").into());
    }
    Ok(())
}

/// Use the same advisory ownership lock as Database::open_local, without opening SQLite or
/// recovering journals on the source. The caller retains this descriptor through publication.
pub(super) fn lock_source(source: &Path) -> Result<std::fs::File, Error> {
    let file = std::fs::File::from(rustix::fs::open(
        source,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err("source must be a regular database file".into());
    }
    file.try_lock()
        .map_err(|e| format!("source database already owned or cannot be locked: {e}"))?;
    for (suffix, label) in [
        ("-wal", "write-ahead log"),
        ("-journal", "rollback journal"),
    ] {
        let mut path = source.as_os_str().to_owned();
        path.push(suffix);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() == 0 => {}
            Ok(_) => return Err(format!("source has nonempty or unsafe {label} data; stop the pre-merge instance cleanly and retry").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot inspect source {label}: {error}").into()),
        }
    }
    Ok(file)
}

/// The early destination check is only a diagnostic; publication itself must exclude races.
pub(super) fn publish(built: &Path, destination: &Path) -> Result<(), Error> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        built,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    Ok(())
}

async fn convert_in(
    source: &std::fs::File,
    work: &Path,
) -> Result<(PathBuf, ConversionReport), Error> {
    let copy = work.join("source.db");
    let target = work.join("converted.db");
    // Clone the locked descriptor, not its pathname: copying cannot switch to an unlocked inode.
    // A cancelled caller also leaves the ownership lock held until this blocking copy completes.
    let (mut from, to) = (source.try_clone()?, copy.clone());
    tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        std::io::copy(&mut from, &mut to)
    })
    .await??;
    let source_version = replay_to_premerge_head(&copy).await?;
    let db = Database::open_local(&target).await?;
    let conn = db.connect().await?;
    conn.execute("PRAGMA foreign_keys = OFF", ()).await?;
    conn.execute(
        "ATTACH DATABASE ?1 AS old",
        [copy
            .to_str()
            .ok_or("conversion work directory must have a UTF-8 path")?],
    )
    .await?;
    check(&conn, "old").await?;
    verify_source_ledger(&conn, "old").await?;
    let straight = plan(&conn).await?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    // Main's triggers guard new writes (revisions, event capture, identity); stricter than the data
    // they now meet, and event capture would invent notifications for rows that already existed. The
    // exact definitions are restored below and compared before commit.
    let mut triggers = Vec::new();
    let mut rows = tx
        .query(
            "SELECT name, sql FROM main.sqlite_schema WHERE type='trigger' ORDER BY rowid",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        triggers.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(rows);
    for (name, _) in &triggers {
        tx.execute(&format!("DROP TRIGGER main.{}", quote(name)), ())
            .await?;
    }
    for table in &straight {
        copy_straight(&tx, table).await?;
    }
    let mapped = copy_collections(&tx).await?;
    let pending = copy_deliveries(&tx).await?;
    copy_snapshot_imports(&tx).await?;
    copy_sequences(&tx).await?;
    for (name, sql) in &triggers {
        tx.execute(sql, ())
            .await
            .map_err(|e| format!("restoring trigger {name} failed: {e}"))?;
    }
    let restored = scalar(
        &tx,
        "SELECT count(*) FROM main.sqlite_schema WHERE type='trigger'",
    )
    .await?;
    if restored != triggers.len() as i64 {
        return Err("trigger set changed during conversion".into());
    }
    let mut counts = BTreeMap::new();
    for table in tables(&tx, "main").await? {
        let n = scalar(&tx, &format!("SELECT count(*) FROM main.{}", quote(&table))).await? as u64;
        counts.insert(table, n);
    }
    for table in tables(&tx, "old").await? {
        let old = scalar(&tx, &format!("SELECT count(*) FROM old.{}", quote(&table))).await? as u64;
        let new = counts.get(&table).copied().unwrap_or(0);
        // Seeded rows only exist in main, so the converted table is never smaller than the source.
        if new < old {
            return Err(
                format!("table {table} lost rows during conversion ({old} -> {new})").into(),
            );
        }
    }
    check(&tx, "main").await?;
    tx.commit().await?;
    conn.execute("DETACH DATABASE old", ()).await?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .ok();
    drop(conn);
    drop(db);
    Ok((
        target,
        ConversionReport {
            source_version,
            destination_version: MIGRATIONS.len(),
            tables: counts,
            collection_settings: mapped.settings,
            collections_unmonitored: mapped.unmonitored,
            pending_deliveries: pending,
        },
    ))
}

/// Convert the cleanly stopped pre-merge PR #32 database at `source` into a new database at
/// `destination`. Never overwrites, never modifies `source`, and never publishes a partial destination.
pub async fn convert_premerge_pr32(
    source: &Path,
    destination: &Path,
) -> Result<ConversionReport, Error> {
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err("destination already exists; conversion never overwrites a database".into());
    }
    let source_owner = lock_source(source)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let work = parent.join(format!(".hrrdarr-convert-{}", uuid::Uuid::new_v4()));
    let create = work.clone();
    tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(create)
    })
    .await??;
    let result = async {
        let (built, report) = convert_in(&source_owner, &work).await?;
        let mut wal = built.as_os_str().to_owned();
        wal.push("-wal");
        if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            return Err("converted database left write-ahead data behind; it was discarded".into());
        }
        // A converted database must open under the normal runner, with the current history intact.
        drop(Database::open_local(&built).await?);
        publish(&built, destination)?;
        Ok::<_, Error>(report)
    }
    .await;
    if let Err(error) = std::fs::remove_dir_all(&work) {
        eprintln!(
            "{}",
            serde_json::json!({"level":"WARN","event":"conversion_workdir_cleanup_failed","error":error.to_string()})
        );
    }
    result
}
