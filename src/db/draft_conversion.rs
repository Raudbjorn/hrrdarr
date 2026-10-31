//! Explicit, offline conversion of an unpublished PR #31 draft database (issue #44).
//!
//! The draft head `1431d97` wrote a 76-row migration ledger. Versions 1 through 49 are shared with
//! every lineage and versions 50 through 62 are byte-identical to the main lineage; the draft first
//! differs from main at version 63 (`notifications`), and from the published PR #31 ledger already
//! at version 50. Startup rejects such a database before running a statement. This module converts
//! it into a new database on the current main lineage (versions 1 through [`TARGET_VERSION`])
//! instead of renaming ledger rows, because the draft and main schemas genuinely differ: collection
//! policy lives inline on `movie_collections` in the draft and in `movie_collection_settings` on
//! main, `notification_deliveries`, `snapshot_imports` and `movie_collection_members` gained
//! columns, and main has five tables the draft never had.
//!
//! Guarantees, each enforced in code below:
//! * The source file is never opened for writing. Its advisory lock is held (stop hrrdarr first),
//!   its bytes (and any `-wal` file) are copied to a private scratch directory while being hashed,
//!   and only the copy is opened. The hash is recomputed at the end; a difference aborts.
//! * The source must be the exact draft ledger (all 76 rows, name and checksum) AND have exactly the
//!   draft schema (fingerprint). Anything else is reported and refused, never guessed at.
//! * The target is built by running the real main migrations 1 through [`TARGET_VERSION`] on an empty
//!   database, so its schema, constraints and ledger are the supported ones by construction.
//! * Data is copied table by table by column name. A column that has no mapping, or a table that is
//!   neither copied nor known to be new, aborts the conversion instead of dropping data. Triggers
//!   are suspended for the copy (they would re-derive history and events) and restored from the very
//!   text they were created from; the schema fingerprint is compared before and after.
//! * Foreign keys, integrity, row counts per table and a few cross-table invariants are verified
//!   before anything is published, and the published file is checked by the application's own
//!   migration-history reader.
//! * Output is created without overwriting (`hard_link`), mode 0600. Without `--to` nothing but the
//!   audit report is written.
//! * The report holds counts, table names and decisions only, never row values.
use super::collection_policy_recovery::{
    Scratch, private_directory, reserve_report, sha256_hex, unix_seconds, write_report,
};
use super::{Error, HISTORY_SQL, MIGRATIONS, checksum, integrity, migration_history, scalar};
use libsql::{Connection, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const COMMAND: &str = "convert-draft-database";
const USAGE: &str = "usage: hrrdarr convert-draft-database --from <draft database> [--to <new database>]\n\
without --to nothing is written except the audit report; the source database is never modified";
const REPORT_FORMAT: u32 = 1;
/// Ledger rows shared by every lineage (`MIGRATIONS[..49]`).
const SHARED_PREFIX: usize = 49;
/// Rows in the draft head's ledger.
pub const DRAFT_VERSION: usize = 76;
/// The main ledger length the converted database has. Later main migrations are applied by the
/// normal startup path, so this converter stays valid when main grows.
pub const TARGET_VERSION: usize = 79;
const TARGET_LAST_NAME: &str = "import_lists";
/// Upper bound on the database file copied to scratch space.
const MAX_DATABASE_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// Tables main has that the draft never had. They start empty, exactly as main's migrations leave
/// them (`movie_collection_settings` is filled from the draft collections below).
const NEW_TABLES: [&str; 5] = [
    "movie_collection_settings",
    "movie_collection_intents",
    "provider_scope_tags",
    "provider_selection_cursors",
    "provider_status",
];
/// Fingerprint of the draft head's complete schema (see [`schema_digest`]). Proven against the
/// migration files of commit `1431d97` by `draft_schema_fingerprint_matches_the_fixture_build`.
pub(super) const DRAFT_SCHEMA_SHA256: &str =
    "ebc0cbf416ea94dbec38e5aca8e8e327dd7fae894fe745ccf56b746e10c49ed4";
/// Ledger rows 50 through 76 of the draft head: `(name, sha256 of the migration SQL)`.
pub(super) const DRAFT_TAIL: [(&str, &str); DRAFT_VERSION - SHARED_PREFIX] = [
    (
        "indexer_backoff",
        "72dc0e9be65f7a3c35002be94c6797f380ee2dc6fc6234bc604b8f83d1a9c05d",
    ),
    (
        "download_client_backoff",
        "f0a5d324526f14b6f8a03717f2e39e357817d8e3a76579508c0042737e100456",
    ),
    (
        "download_client_selection",
        "7ba1b04c7a294494589d2c74ec243a2dc3db0a8319349ab81cea3d55ea3f0fad",
    ),
    (
        "path_diagnostics",
        "b4a860a09559d0d5456ae579d1a365436d76e272abf09da92f617d60e358b76f",
    ),
    (
        "downloaded_scan",
        "239f7de4abe1889093e171956928ff7c7fbf2bb4cf27508be6b821aac1f91157",
    ),
    (
        "series_runtime",
        "42dae37e4c09df32f98197fea07295d9509e8fbfe4997629147a6774862666a9",
    ),
    (
        "import_policy",
        "f74cfc849e286da57a2d2b446d16d355efc76d779d51469819abec3b66dd02b8",
    ),
    (
        "scan_media",
        "6604409bae18e1795787a850c5e8d20e0d4c88af3abfdc75308f14f9b1c73d06",
    ),
    (
        "scan_successor",
        "0ed6e4505db5bd82ce82922f51d05c07c285333b5353d85631e6ac2a3d126c7b",
    ),
    (
        "download_client_registry",
        "5c49d6dffa6eec5e99a8bf5dd44af9a5acedd62ca1f313f3d18e6285fd0582f0",
    ),
    (
        "recycle_bin",
        "96400fc8ee02d30d39b8d5ae3f962d01e7f59d11e198e7418930c0a35eb9cfe5",
    ),
    (
        "upgrade_recycle",
        "a00db0b086c047ab604065083ee0c96dd1d838f0206985d98daa0b578ce30420",
    ),
    (
        "transmission",
        "240da1e9f5e100abe48874225245287d01258b83565ca50d3c1215465ddc54d7",
    ),
    (
        "notifications",
        "03e6f38daffbf608852ed300917a5b873b11e4183ad03609ad028bb3d648366f",
    ),
    (
        "nzbget",
        "78d6250a93b9c11e87f731a7ab4c8c269f82d178725cc996b5893c981fc54e67",
    ),
    (
        "rename_commands",
        "7c8106d4274becaa772aba0909151c5d03202608e6b8114de7865527f7d3ea87",
    ),
    (
        "autotagging_rules",
        "887329ded60ab3760db8be3cfb9849ad031f0a789d863c53b18273656341e4a1",
    ),
    (
        "collections",
        "c089e9ed556dfe7ef68621774082277c649ed2bb4aed1a2d1e88d00316a54fe2",
    ),
    (
        "torrent_rss_indexer",
        "e8f72f94ee984ab11e8e38d1b8f931950888140d3fa68816a202fcb2c715aa32",
    ),
    (
        "collection_snapshot",
        "5a6141514f1211b247b285c3f102c37fe704a9bf8faa9b4095e7bfdaf7d8170a",
    ),
    (
        "deluge",
        "3394cda1821872fa1be679a918b6154c4cb60a97c5bc0abd0b13e52cb425b0ab",
    ),
    (
        "rtorrent",
        "f1da8c1c9f5744610d2f8939b797d79d5eb16ec854addff0a340974ec49c03df",
    ),
    (
        "utorrent",
        "1ddd53efa62f7e91f4e9261f56b6cad32984cd587b23b55d939c24f029f9b525",
    ),
    (
        "aria2",
        "b7d7078d5e42bffbd601ac8006d111431e6c3980c5cc1e357d48aececda6645f",
    ),
    (
        "flood",
        "ab3166b323a3d02c01d578d182603abd62f08f1582c302a1707811a24ad9c6ae",
    ),
    (
        "downloadstation",
        "ab4400d881f2a6f98023ca3f8e35b6182008fbb870700f5141cb934b5f8f2fce",
    ),
    (
        "import_lists",
        "f943e2b45cfc7b8c4bb520a0da331240d3f134e09550a21bc40efe2455b970de",
    ),
];

/// How one table is copied: draft columns with no counterpart on main (`retired`, their data moves
/// elsewhere in this module) and main columns computed from the draft row `d`.
struct Rule {
    table: &'static str,
    retired: &'static [&'static str],
    expressions: &'static [(&'static str, &'static str)],
}

/// Mirrors migration 0077 (`collection_authority`): collection policy moves to
/// `movie_collection_settings`, the inline columns are retired to their defaults, and members are
/// stamped with their collection's metadata revision (the `collection_members_*` triggers require
/// equality).
const RULES: [Rule; 3] = [
    Rule {
        table: "movie_collections",
        retired: &["local_edit", "snapshot_owned"],
        expressions: &[
            ("monitored", "0"),
            ("search_on_add", "0"),
            ("root_folder_id", "NULL"),
            ("quality_profile_id", "NULL"),
            ("minimum_availability", "NULL"),
        ],
    },
    Rule {
        table: "movie_collection_members",
        retired: &[],
        expressions: &[(
            "metadata_revision",
            "(SELECT c.metadata_revision FROM draft.movie_collections c WHERE c.id = d.collection_id)",
        )],
    },
    // Mirrors migration 0083: a delivery renders for the scope that matched at fan-out; a system
    // event uses the scope of the notification, TV first. No acknowledged parts, no claim.
    Rule {
        table: "notification_deliveries",
        retired: &[],
        expressions: &[
            (
                "media_type",
                "CASE WHEN (SELECT e.scope FROM draft.notification_events e WHERE e.sequence = d.event_sequence) IN ('tv','movies') \
                 THEN (SELECT e.scope FROM draft.notification_events e WHERE e.sequence = d.event_sequence) \
                 WHEN EXISTS(SELECT 1 FROM draft.notification_scopes s WHERE s.notification_id = d.notification_id AND s.media_type = 'tv') THEN 'tv' \
                 ELSE 'movies' END",
            ),
            ("sent_parts", "'[]'"),
            ("lease_owner", "NULL"),
        ],
    },
];

/// Policy rows exactly as migration 0077 derives them, except that the draft's own provenance
/// flags are kept. Unknown provenance (neither flag set) is treated as a local edit, the same
/// conservative choice 0077 makes, so a later backup import never overwrites it.
const SETTINGS_SQL: &str = "INSERT INTO main.movie_collection_settings(collection_id, monitored, root_folder_id, quality_profile_id, minimum_availability, search_on_add, local_edit, snapshot_owned)
SELECT d.id,
 CASE WHEN d.root_folder_id IS NOT NULL AND d.quality_profile_id IS NOT NULL AND d.minimum_availability IS NOT NULL THEN d.monitored ELSE 0 END,
 d.root_folder_id, d.quality_profile_id, d.minimum_availability, d.search_on_add,
 CASE WHEN d.snapshot_owned = 1 AND d.local_edit = 0 THEN 0 ELSE 1 END,
 d.snapshot_owned
FROM draft.movie_collections d
WHERE d.monitored != 0 OR d.search_on_add != 0 OR d.root_folder_id IS NOT NULL OR d.quality_profile_id IS NOT NULL
 OR d.minimum_availability IS NOT NULL OR d.local_edit != 0 OR d.snapshot_owned != 0
 OR EXISTS(SELECT 1 FROM draft.movie_collection_tags t WHERE t.collection_id = d.id)";

/// Why a source database was not converted. Serialized names are part of the report contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    NoMigrationHistory,
    /// Startup already accepts this database; there is nothing to convert.
    AlreadySupported,
    NotTheDraftLedger,
    DraftLedgerIncomplete,
    DraftSchemaDiffers,
}

impl Rejection {
    pub fn explanation(self) -> &'static str {
        match self {
            Rejection::NoMigrationHistory => "the database has no migration history",
            Rejection::AlreadySupported => {
                "the database is already on a supported migration lineage; start hrrdarr to upgrade it"
            }
            Rejection::NotTheDraftLedger => {
                "the migration history is not the unpublished PR #31 draft ledger (draft head 1431d97)"
            }
            Rejection::DraftLedgerIncomplete => {
                "the draft ledger is incomplete (fewer than 76 rows); run the draft build once to finish its own migrations, then convert"
            }
            Rejection::DraftSchemaDiffers => {
                "the migration history is the draft ledger but the schema is not the draft schema; it was changed outside the migrations"
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Converted and verified in scratch space; nothing published (no `--to`).
    DryRun,
    Converted,
    NotConvertible(Rejection),
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::DryRun => "dry_run",
            Outcome::Converted => "converted",
            Outcome::NotConvertible(_) => "not_convertible",
        }
    }
}

#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    pub report_path: PathBuf,
    pub json: serde_json::Value,
}

/// Digest of the complete schema: every table, index, trigger and view definition by type and name.
pub async fn schema_digest(conn: &Connection) -> Result<String, Error> {
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name, coalesce(sql, '') FROM main.sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            (),
        )
        .await?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    while let Some(row) = rows.next().await? {
        for column in 0..4 {
            let text = row.get::<String>(column)?;
            digest.update(&(text.len() as u64).to_le_bytes());
            digest.update(text.as_bytes());
        }
    }
    Ok(sha256_hex(digest))
}

/// Value-returning classification of the source: `None` means it is the complete draft database.
async fn classify_source(conn: &Connection) -> Result<Option<Rejection>, Error> {
    if scalar(
        conn,
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migrations'",
    )
    .await?
        == 0
    {
        return Ok(Some(Rejection::NoMigrationHistory));
    }
    if migration_history(conn).await.is_ok() {
        return Ok(Some(Rejection::AlreadySupported));
    }
    let mut rows = conn
        .query(
            "SELECT version, name, checksum, sql FROM schema_migrations ORDER BY version",
            (),
        )
        .await?;
    let mut count = 0usize;
    while let Some(row) = rows.next().await? {
        let version: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let recorded: String = row.get(2)?;
        let sql: String = row.get(3)?;
        let expected: Option<(&str, String)> = if count < SHARED_PREFIX {
            MIGRATIONS.get(count).map(|&(n, s)| (n, checksum(s)))
        } else {
            DRAFT_TAIL
                .get(count - SHARED_PREFIX)
                .map(|&(n, h)| (n, h.to_owned()))
        };
        let Some((expected_name, expected_sum)) = expected else {
            return Ok(Some(Rejection::NotTheDraftLedger));
        };
        if version != count as i64 + 1
            || name != expected_name
            || recorded != expected_sum
            || checksum(&sql) != expected_sum
        {
            return Ok(Some(Rejection::NotTheDraftLedger));
        }
        count += 1;
    }
    if count == 0 {
        return Ok(Some(Rejection::NoMigrationHistory));
    }
    if count < DRAFT_VERSION {
        return Ok(Some(Rejection::DraftLedgerIncomplete));
    }
    if schema_digest(conn).await? != DRAFT_SCHEMA_SHA256 {
        return Ok(Some(Rejection::DraftSchemaDiffers));
    }
    Ok(None)
}

fn wal_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// Feed a file or stream into an existing digest without allocating for its total length.
pub(super) fn update_digest(
    mut source: impl Read,
    digest: &mut ring::digest::Context,
) -> std::io::Result<()> {
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        digest.update(&buffer[..count]);
    }
}

/// Digest of the database file followed by its write-ahead log, if any.
fn source_digest(path: &Path) -> Result<String, Error> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for file in [path.to_path_buf(), wal_path(path)] {
        let source = match File::open(&file) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && file != path => continue,
            Err(error) => return Err(format!("cannot read the source database: {error}").into()),
        };
        update_digest(source, &mut digest)?;
        digest.update(b"|");
    }
    Ok(sha256_hex(digest))
}

/// Copies the database file and its write-ahead log into scratch space so only the copy is opened.
fn copy_source(source: &Path, copy: &Path) -> Result<(), Error> {
    for (from, to) in [
        (source.to_path_buf(), copy.to_path_buf()),
        (wal_path(source), wal_path(copy)),
    ] {
        let mut input = match File::open(&from) {
            Ok(input) => input,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && from != source => {
                continue;
            }
            Err(error) => return Err(format!("cannot read the source database: {error}").into()),
        };
        if !input.metadata()?.is_file() {
            return Err("the source database is not a regular file".into());
        }
        let mut output = create_private_file(&to)?;
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
        let mut total = 0u64;
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > MAX_DATABASE_BYTES {
                return Err("the source database is too large to convert".into());
            }
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[derive(Debug, Clone)]
pub(super) struct Column {
    pub(super) name: String,
    not_null: bool,
    has_default: bool,
    primary_key: bool,
    declared_type: String,
}

#[derive(Debug, Clone)]
pub(super) struct Table {
    pub(super) columns: Vec<Column>,
    /// False for `WITHOUT ROWID` tables.
    has_rowid: bool,
    /// True when a single `INTEGER PRIMARY KEY` column is the rowid.
    rowid_alias: bool,
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Ordinary tables of one schema with their insertable (non-generated) columns.
pub(super) async fn inventory(
    conn: &Connection,
    schema: &str,
) -> Result<BTreeMap<String, Table>, Error> {
    let mut names = Vec::new();
    let mut rows = conn
        .query(
            &format!("SELECT name, sql FROM {schema}.sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"),
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let sql: String = row.get(1)?;
        if sql
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("CREATE VIRTUAL")
        {
            return Err(format!(
                "{schema} table {name} is a virtual table, which cannot be converted"
            )
            .into());
        }
        names.push((name, sql.to_ascii_uppercase().contains("WITHOUT ROWID")));
    }
    let mut tables = BTreeMap::new();
    for (name, without_rowid) in names {
        let mut columns = Vec::new();
        let mut primary_keys = 0;
        let mut rows = conn
            .query(
                &format!("PRAGMA {schema}.table_xinfo({})", quote(&name)),
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            // 0 = ordinary column; 2 and 3 are generated columns, which cannot be written.
            if row.get::<i64>(6)? != 0 {
                continue;
            }
            let primary_key = row.get::<i64>(5)? > 0;
            primary_keys += usize::from(primary_key);
            columns.push(Column {
                name: row.get(1)?,
                declared_type: row.get::<String>(2)?.to_ascii_uppercase(),
                not_null: row.get::<i64>(3)? != 0,
                has_default: !matches!(row.get_value(4)?, libsql::Value::Null),
                primary_key,
            });
        }
        let rowid_alias = !without_rowid
            && primary_keys == 1
            && columns
                .iter()
                .any(|c| c.primary_key && c.declared_type == "INTEGER");
        tables.insert(
            name,
            Table {
                columns,
                has_rowid: !without_rowid,
                rowid_alias,
            },
        );
    }
    Ok(tables)
}

/// One statement copying a table by column name, or an error when a column would be lost or left
/// without a value.
fn copy_statement(name: &str, draft: &Table, main: &Table) -> Result<String, Error> {
    let rule = RULES.iter().find(|r| r.table == name);
    let retired: &[&str] = rule.map_or(&[], |r| r.retired);
    let expressions: &[(&str, &str)] = rule.map_or(&[], |r| r.expressions);
    let draft_names: BTreeSet<&str> = draft.columns.iter().map(|c| c.name.as_str()).collect();
    let main_names: BTreeSet<&str> = main.columns.iter().map(|c| c.name.as_str()).collect();
    for column in &draft_names {
        if !main_names.contains(column) && !retired.contains(column) {
            return Err(format!(
                "table {name}: draft column {column} has no mapping; refusing to drop data"
            )
            .into());
        }
    }
    for column in retired {
        if !draft_names.contains(column) {
            return Err(format!("table {name}: expected draft column {column} is missing").into());
        }
    }
    let mut targets = Vec::new();
    let mut sources = Vec::new();
    if draft.has_rowid && main.has_rowid && !draft.rowid_alias && !main.rowid_alias {
        targets.push("rowid".to_owned());
        sources.push("d.rowid".to_owned());
    }
    for column in &main.columns {
        if let Some((_, expression)) = expressions.iter().find(|(c, _)| *c == column.name) {
            targets.push(quote(&column.name));
            sources.push((*expression).to_owned());
        } else if draft_names.contains(column.name.as_str()) {
            targets.push(quote(&column.name));
            sources.push(format!("d.{}", quote(&column.name)));
        } else if column.has_default || !column.not_null || column.primary_key {
            continue;
        } else {
            return Err(format!(
                "table {name}: main column {} has no draft source and no default",
                column.name
            )
            .into());
        }
    }
    Ok(format!(
        "INSERT INTO main.{table}({targets}) SELECT {sources} FROM draft.{table} AS d",
        table = quote(name),
        targets = targets.join(", "),
        sources = sources.join(", "),
    ))
}

/// Applies main migrations `1..=TARGET_VERSION` to an empty database, writing the same ledger rows
/// the application writes.
async fn build_target(conn: &Connection) -> Result<(), Error> {
    if MIGRATIONS.len() < TARGET_VERSION || MIGRATIONS[TARGET_VERSION - 1].0 != TARGET_LAST_NAME {
        return Err("the main migration history no longer contains the conversion target".into());
    }
    conn.execute("PRAGMA foreign_keys = OFF", ()).await?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result: Result<(), Error> = async {
        tx.execute(HISTORY_SQL, ()).await?;
        for (index, &(name, sql)) in MIGRATIONS.iter().enumerate().take(TARGET_VERSION) {
            tx.execute_batch(sql)
                .await
                .map_err(|e| format!("target migration {} ({name}) failed: {e}", index + 1))?;
            tx.execute(
                "INSERT INTO schema_migrations (version, name, checksum, sql) VALUES (?1, ?2, ?3, ?4)",
                params![(index + 1) as i64, name, checksum(sql), sql],
            )
            .await?;
        }
        integrity(&tx).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(tx.commit().await?),
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Row counts per copied table, `(draft, converted)`.
type Counts = BTreeMap<String, (i64, i64)>;

#[derive(Debug, Default)]
struct Transfer {
    counts: Counts,
    settings_created: i64,
}

/// Copies every draft row into the target inside one transaction. `draft` must be attached.
async fn transfer(conn: &Connection) -> Result<Transfer, Error> {
    let draft_tables = inventory(conn, "draft").await?;
    let main_tables = inventory(conn, "main").await?;
    for name in draft_tables.keys() {
        if !main_tables.contains_key(name) {
            return Err(format!(
                "draft table {name} does not exist on main; refusing to drop data"
            )
            .into());
        }
    }
    for name in main_tables.keys() {
        if !draft_tables.contains_key(name) && !NEW_TABLES.contains(&name.as_str()) {
            return Err(format!(
                "main table {name} has no draft source and is not a known new table"
            )
            .into());
        }
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result: Result<Transfer, Error> = async {
        let before = schema_digest(&tx).await?;
        let mut triggers = Vec::new();
        let mut rows = tx
            .query(
                "SELECT name, sql FROM main.sqlite_schema WHERE type = 'trigger' ORDER BY rowid",
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
        let mut done = Transfer::default();
        for (name, draft) in &draft_tables {
            if name == "schema_migrations" {
                continue;
            }
            let statement = copy_statement(name, draft, &main_tables[name])?;
            // The target's migrations seeded default rows (identical to the draft's own seeds, see
            // `seed_rows_match_between_draft_and_target`); the draft's rows replace them wholesale,
            // so edits and deletions of defaults survive.
            tx.execute(&format!("DELETE FROM main.{}", quote(name)), ())
                .await?;
            tx.execute(&statement, ())
                .await
                .map_err(|e| format!("copying table {name} failed: {e}"))?;
            let from = scalar(&tx, &format!("SELECT count(*) FROM draft.{}", quote(name))).await?;
            let to = scalar(&tx, &format!("SELECT count(*) FROM main.{}", quote(name))).await?;
            if from != to {
                return Err(format!("table {name}: {from} draft rows but {to} converted rows").into());
            }
            done.counts.insert(name.clone(), (from, to));
        }
        tx.execute(SETTINGS_SQL, ())
            .await
            .map_err(|e| format!("deriving collection settings failed: {e}"))?;
        done.settings_created = scalar(&tx, "SELECT count(*) FROM main.movie_collection_settings").await?;
        // Autoincrement counters never move backwards, so ids freed by deletes are not reissued.
        tx.execute(
            "UPDATE main.sqlite_sequence SET seq = max(seq, coalesce((SELECT d.seq FROM draft.sqlite_sequence d WHERE d.name = main.sqlite_sequence.name), 0))",
            (),
        )
        .await?;
        tx.execute(
            "INSERT INTO main.sqlite_sequence(name, seq) SELECT d.name, d.seq FROM draft.sqlite_sequence d \
             WHERE EXISTS(SELECT 1 FROM main.sqlite_schema s WHERE s.type = 'table' AND s.name = d.name) \
             AND NOT EXISTS(SELECT 1 FROM main.sqlite_sequence m WHERE m.name = d.name)",
            (),
        )
        .await?;
        for (name, sql) in &triggers {
            tx.execute_batch(sql)
                .await
                .map_err(|e| format!("restoring trigger {name} failed: {e}"))?;
        }
        if schema_digest(&tx).await? != before {
            return Err("the converted schema differs from the target schema".into());
        }
        invariants(&tx).await?;
        Ok(done)
    }
    .await;
    match result {
        Ok(done) => {
            tx.commit().await?;
            Ok(done)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Cross-table facts that main's triggers only enforce on write, so existing rows must be checked.
async fn invariants(conn: &Connection) -> Result<(), Error> {
    let mut violations = Vec::new();
    let mut rows = conn.query("PRAGMA main.foreign_key_check", ()).await?;
    let mut tables = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        tables.insert(row.get::<String>(0)?);
    }
    drop(rows);
    if !tables.is_empty() {
        violations.push(format!(
            "foreign key violations in: {}",
            tables.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    for (message, sql) in [
        (
            "collection settings reference a root folder outside the movies domain",
            "SELECT count(*) FROM main.movie_collection_settings s WHERE s.root_folder_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM main.root_folders r WHERE r.id = s.root_folder_id AND r.media_type = 'movies')",
        ),
        (
            "collection members disagree with their collection's metadata revision",
            "SELECT count(*) FROM main.movie_collection_members m WHERE m.metadata_revision != (SELECT c.metadata_revision FROM main.movie_collections c WHERE c.id = m.collection_id)",
        ),
        (
            "a collection has more than 1000 members",
            "SELECT count(*) FROM (SELECT 1 FROM main.movie_collection_members GROUP BY collection_id HAVING count(*) > 1000)",
        ),
        (
            "a collection has more than 200 tags",
            "SELECT count(*) FROM (SELECT 1 FROM main.movie_collection_tags GROUP BY collection_id HAVING count(*) > 200)",
        ),
    ] {
        let count = scalar(conn, sql).await?;
        if count > 0 {
            violations.push(format!("{message} ({count})"));
        }
    }
    // Migration 0076 forces this check on existing rows; the new trigger rejects the value.
    conn.execute(
        "UPDATE main.provider_scopes SET category = category WHERE implementation IN ('aria2', 'downloadstation') AND category IN ('.', '..')",
        (),
    )
    .await
    .map_err(|_| "a download client category is '.' or '..', which main rejects; correct it in the draft database first")?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the draft data does not satisfy main: {}",
            violations.join("; ")
        )
        .into())
    }
}

/// The published file is checked the way startup would read it.
async fn verify_output(path: &Path) -> Result<(), Error> {
    let database = libsql::Builder::new_local(path).build().await?;
    let conn = database.connect()?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    integrity(&conn).await?;
    let (version, history) = migration_history(&conn).await?;
    if version != TARGET_VERSION || history != MIGRATIONS {
        return Err("the converted database does not read back as the main lineage".into());
    }
    if scalar(&conn, "SELECT count(*) FROM pragma_foreign_key_check").await? != 0 {
        return Err("the converted database has foreign key violations".into());
    }
    Ok(())
}

fn directory_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

fn lock_source(path: &Path) -> Result<File, Error> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open the source database: {e}"))?;
    file.try_lock().map_err(|e| {
        format!("database already owned or cannot be locked (stop hrrdarr first): {e}")
    })?;
    Ok(file)
}

fn require_unused_output(to: &Path) -> std::io::Result<()> {
    // SQLite can consume stale journals when the new database is first opened. Treat every
    // sidecar entry, including a dangling symlink, as occupied without opening or deleting it.
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = to.as_os_str().to_os_string();
        name.push(suffix);
        match std::fs::symlink_metadata(PathBuf::from(name)) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "the output path or a SQLite sidecar already exists; neither is overwritten",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Publish complete bytes atomically without replacing an existing path; unsupported links fail closed.
pub(super) fn publish(from: &Path, to: &Path) -> std::io::Result<()> {
    // Repeat preflight after conversion, just before publication. Offline use still requires
    // excluding external writers: sidecar checks and the hard link are separate filesystem calls.
    require_unused_output(to)?;
    std::fs::hard_link(from, to)
}

/// Runs the conversion. `Err` is reserved for environment problems and data that cannot be
/// converted; a source that is not a draft database is an `Ok` report with
/// [`Outcome::NotConvertible`].
pub async fn run(options: &Options) -> Result<Report, Error> {
    let source = std::fs::canonicalize(&options.from)
        .map_err(|e| format!("cannot resolve the source database: {e}"))?;
    if let Some(to) = &options.to {
        require_unused_output(to)?;
        let parent = directory_of(to);
        let parent = std::fs::canonicalize(&parent)
            .map_err(|e| format!("the output directory is not usable: {e}"))?;
        if parent.join(to.file_name().ok_or("the output path has no file name")?) == source {
            return Err("the output must not be the source database".into());
        }
    }
    let work_dir = options
        .to
        .as_deref()
        .map_or_else(|| directory_of(&source), directory_of);
    let mode = if options.to.is_some() {
        "convert"
    } else {
        "dry-run"
    };
    let (report_file, report_path) = reserve_report(&work_dir, "draft-conversion", mode)?;
    let result = run_inner(options, &source, &work_dir).await;
    let (mut json, outcome) = match result {
        Ok(done) => done,
        Err(error) => {
            let json = serde_json::json!({
                "format": REPORT_FORMAT,
                "tool": format!("hrrdarr {COMMAND}"),
                "mode": mode,
                "outcome": "failed",
                "published": false,
                "error": error.to_string(),
            });
            if let Err(write_error) = write_report(report_file, &json) {
                return Err(format!(
                    "{error}; additionally the report could not be written: {write_error}"
                )
                .into());
            }
            return Err(error);
        }
    };
    json["report_file"] = report_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into();
    write_report(report_file, &json)?;
    eprintln!(
        "{}",
        serde_json::json!({
            "level": "INFO",
            "event": "draft_database_conversion",
            "outcome": outcome.name(),
            "published": json["published"],
            "report_file": json["report_file"],
        })
    );
    Ok(Report {
        outcome,
        report_path,
        json,
    })
}

async fn run_inner(
    options: &Options,
    source: &Path,
    work_dir: &Path,
) -> Result<(serde_json::Value, Outcome), Error> {
    // Held until the function returns: it excludes a running service for the whole run.
    let _lock = lock_source(source)?;
    let before = source_digest(source)?;
    let scratch_path = work_dir.join(format!(".hrrdarr-convert-{}", uuid::Uuid::new_v4()));
    private_directory(&scratch_path).map_err(|e| format!("cannot create scratch space: {e}"))?;
    let scratch = Scratch(scratch_path);
    let draft_copy = scratch.0.join("draft.db");
    copy_source(source, &draft_copy)?;
    let mut json = serde_json::json!({
        "format": REPORT_FORMAT,
        "tool": format!("hrrdarr {COMMAND}"),
        "tool_version": env!("CARGO_PKG_VERSION"),
        "mode": if options.to.is_some() { "convert" } else { "dry_run" },
        "created_at_unix_seconds": unix_seconds()?,
        "source": { "sha256_before": before, "draft_version": DRAFT_VERSION },
        "target": { "version": TARGET_VERSION },
    });

    {
        let draft = libsql::Builder::new_local(&draft_copy).build().await?;
        let conn = draft.connect()?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        integrity(&conn).await?;
        if let Some(rejection) = classify_source(&conn).await? {
            json["published"] = false.into();
            json["outcome"] = Outcome::NotConvertible(rejection).name().into();
            json["rejection"] = serde_json::to_value(rejection)?;
            json["explanation"] = rejection.explanation().into();
            return Ok((json, Outcome::NotConvertible(rejection)));
        }
    }

    let target_path = scratch.0.join("converted.db");
    drop(create_private_file(&target_path)?);
    let transferred = {
        let target = libsql::Builder::new_local(&target_path).build().await?;
        let conn = target.connect()?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        build_target(&conn).await?;
        let draft_literal = draft_copy.to_str().ok_or("the scratch path is not UTF-8")?;
        conn.execute("ATTACH DATABASE ?1 AS draft", params![draft_literal])
            .await?;
        let transferred = transfer(&conn).await?;
        conn.execute("DETACH DATABASE draft", ()).await?;
        // Fold the journal into the file so exactly one file is published.
        conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()).await?;
        transferred
    };
    verify_output(&target_path).await?;
    if source_digest(source)? != before {
        return Err(
            "the source database changed during the conversion; nothing was published".into(),
        );
    }
    File::open(&target_path)?.sync_all()?;
    let output_sha = sha256_hex({
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        update_digest(File::open(&target_path)?, &mut digest)?;
        digest
    });

    let tables: BTreeMap<&String, serde_json::Value> = transferred
        .counts
        .iter()
        .map(|(name, (from, to))| {
            (
                name,
                serde_json::json!({ "draft_rows": from, "converted_rows": to }),
            )
        })
        .collect();
    json["tables"] = serde_json::to_value(tables)?;
    json["collection_settings_rows_created"] = transferred.settings_created.into();
    json["source"]["sha256_after"] = source_digest(source)?.into();
    json["source"]["modified"] = false.into();
    json["target"]["sha256"] = output_sha.into();
    json["notes"] = serde_json::json!([
        "the migration ledger of the converted database is the main ledger, regenerated; applied_at values are the conversion time",
        "triggers, indexes and constraints are the main definitions; no draft trigger or event was replayed",
        "collection search-on-add values are copied as stored; the draft never kept the absent/explicit-false distinction",
        "new main tables (provider tags, provider status, selection cursors, collection intents) start empty"
    ]);

    match &options.to {
        None => {
            json["published"] = false.into();
            json["outcome"] = Outcome::DryRun.name().into();
            Ok((json, Outcome::DryRun))
        }
        Some(to) => {
            publish(&target_path, to)
                .map_err(|e| format!("cannot publish the converted database: {e}"))?;
            File::open(directory_of(to))?.sync_all()?;
            json["published"] = true.into();
            json["outcome"] = Outcome::Converted.name().into();
            Ok((json, Outcome::Converted))
        }
    }
}

/// Parsed command line, minimal and dependency-free.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut from: Option<PathBuf> = None;
    let mut to: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let slot = match flag {
            "--from" => &mut from,
            "--to" => &mut to,
            other => return Err(format!("unrecognized argument: {other}")),
        };
        if slot.is_some() {
            return Err(format!("{flag} given twice"));
        }
        let value = match inline {
            Some(value) => value,
            None => iter.next().cloned().ok_or(format!("{flag} needs a path"))?,
        };
        if value.is_empty() {
            return Err(format!("{flag} needs a path"));
        }
        *slot = Some(PathBuf::from(value));
    }
    Ok(Options {
        from: from.ok_or("--from <draft database> is required")?,
        to,
    })
}

/// Entry point for `hrrdarr convert-draft-database`. Exit codes: 0 done, 1 error or bad usage,
/// 3 the source is not a convertible draft database (nothing written but the report).
pub async fn run_cli(args: &[String]) -> std::process::ExitCode {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return std::process::ExitCode::from(1);
        }
    };
    match run(&options).await {
        Ok(Report {
            outcome: Outcome::NotConvertible(rejection),
            ..
        }) => {
            eprintln!("{}", rejection.explanation());
            std::process::ExitCode::from(3)
        }
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","event":"draft_database_conversion_failed","error":error.to_string()})
            );
            std::process::ExitCode::from(1)
        }
    }
}
