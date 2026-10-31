//! Explicit, offline recovery of the nullable collection `search_on_add` policy.
//!
//! The published PR #31 version-82 history converted the main-published nullable
//! `movie_collection_settings.search_on_add` ("setting absent" = NULL, "explicit false" = 0) into an
//! inline `NOT NULL DEFAULT 0` column. A database that completed that conversion no longer holds the
//! distinction. The pre-upgrade backup written by `Database::backup()` still does. This module
//! restores NULL, and only NULL, for collections where the backup proves it and the live row proves
//! that nothing was edited since.
//!
//! Guarantees, each enforced in code below:
//! * The default is a dry run. Nothing in the live database is written unless `apply` is set, and
//!   the dry-run connection is `query_only`.
//! * The backup is never modified or written into. It is hash-verified while being copied to a
//!   private scratch directory, and only the copy is opened.
//! * The backup must be an exact prefix of the published legacy migration ledger that still carries
//!   the nullable column (versions 52 through 72), and the live ledger must be that same lineage
//!   fully upgraded through the current legacy lineage. `schema_version` in the manifest is cross-checked, never trusted.
//! * Collections are matched by BOTH the internal id and the TMDB collection id. Disagreement is
//!   reported as ambiguous and never restored.
//! * A row is restored only when the live settings equal, field for field, what the legacy
//!   conversion plus the 0083 adapter deterministically produce from the backup row (revision,
//!   monitoring, root, profile, availability, local edit, snapshot ownership and tags).
//!   The backup must also have `local_edit = 0`: an already-set flag cannot reveal a later
//!   search-on-add-only edit while the policy was stored inline.
//! * Apply is one `BEGIN IMMEDIATE` transaction that re-plans from live data, updates only the
//!   restorable rows, re-verifies, and rolls back completely on any failure. Foreign keys and the
//!   `collection_settings_revision` trigger stay enabled; the trigger requires each restored row's
//!   `settings_revision` to rise by exactly one, and that is the only side effect besides the
//!   column itself. `schema_migrations` is never written, and its digest is compared before and
//!   after.
//! * Every run writes a JSON audit report (create-new, mode 0600) next to the live database and one
//!   structured line on stderr. They contain collection identities and decisions only.
use super::{Error, LEGACY_MIGRATIONS, hex, integrity, migration_history, scalar};
use libsql::{Connection, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// Collections held in memory per database; larger libraries are refused rather than truncated.
const MAX_COLLECTIONS: usize = 100_000;
/// Tag association rows held in memory per database.
const MAX_TAG_ROWS: usize = 1_000_000;
/// Backup manifests are a few hundred bytes; anything larger is not one.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Upper bound on the backup database copied to scratch space.
const MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// First migration version that created `movie_collection_settings` (the published collections).
const FIRST_NULLABLE_VERSION: usize = 52;
/// The `collections_checkpoint_supersede` migration is version 73; before it the column is nullable.
const LAST_NULLABLE_VERSION: usize = 72;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const REPORT_FORMAT: u32 = 1;
pub const COMMAND: &str = "recover-collection-policy";
const USAGE: &str = "usage: hrrdarr recover-collection-policy --backup <directory> [--apply]\n\
the database path is read from HRRDARR_DATABASE_PATH; without --apply nothing is written to it";
const REVISION_CONSEQUENCE: &str = "each restored collection's settings_revision increases by exactly 1 (required by the collection_settings_revision trigger, which stays enabled); no other column, monitoring or search behavior changes";

/// What was decided for one collection. Serialized names are part of the report contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Backup NULL with local_edit=0, live false, and live equals the backup's conversion.
    Restorable,
    /// Live is already NULL ("absent"); a repeat run lands here.
    AlreadyNull,
    /// Backup recorded an explicit false; it is kept.
    ExplicitFalsePreserved,
    /// Backup recorded an explicit true; it is kept. (Not in the original list; needed so a true
    /// value is not mislabelled "false".)
    ExplicitTruePreserved,
    /// The live row differs from what the conversion yields, so someone edited it since.
    LocalEditSinceBackup,
    /// Identity, nullable-policy validity or edit provenance cannot prove a safe restore.
    Ambiguous,
    /// The backup row has no counterpart collection in the live database.
    MissingInLive,
    /// A live false value for which the backup holds no usable evidence.
    UnrecoverableNoBackupProvenance,
}

impl Decision {
    pub const ALL: [Decision; 8] = [
        Decision::Restorable,
        Decision::AlreadyNull,
        Decision::ExplicitFalsePreserved,
        Decision::ExplicitTruePreserved,
        Decision::LocalEditSinceBackup,
        Decision::Ambiguous,
        Decision::MissingInLive,
        Decision::UnrecoverableNoBackupProvenance,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Decision::Restorable => "restorable",
            Decision::AlreadyNull => "already_null",
            Decision::ExplicitFalsePreserved => "explicit_false_preserved",
            Decision::ExplicitTruePreserved => "explicit_true_preserved",
            Decision::LocalEditSinceBackup => "local_edit_since_backup",
            Decision::Ambiguous => "ambiguous",
            Decision::MissingInLive => "missing_in_live",
            Decision::UnrecoverableNoBackupProvenance => "unrecoverable_no_backup_provenance",
        }
    }
}

/// The policy fields that the conversion carries and a local edit would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFacts {
    pub settings_revision: i64,
    pub monitored: i64,
    pub root_folder_id: Option<i64>,
    pub quality_profile_id: Option<i64>,
    pub minimum_availability: Option<String>,
    pub search_on_add: Option<i64>,
    pub local_edit: i64,
    pub snapshot_owned: i64,
    pub tags: BTreeSet<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionFacts {
    pub tmdb_id: Option<i64>,
    pub settings: Option<PolicyFacts>,
}

/// Collections keyed by internal id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub collections: BTreeMap<i64, CollectionFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub collection_id: i64,
    pub tmdb_id: Option<i64>,
    pub decision: Decision,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveat: Option<&'static str>,
    /// The live revision the restore is conditioned on.
    #[serde(skip)]
    pub live_revision: Option<i64>,
}

fn entry(
    collection_id: i64,
    tmdb_id: Option<i64>,
    decision: Decision,
    reason: &'static str,
) -> Entry {
    Entry {
        collection_id,
        tmdb_id,
        decision,
        reason,
        caveat: None,
        live_revision: None,
    }
}

/// What the legacy-82 conversion (`collections_checkpoint_supersede` + carry) followed by the 0083
/// adapter yields for a backup settings row: NULL becomes false, a monitored flag survives only
/// with a complete policy, and everything else is carried verbatim.
fn converted(backup: &PolicyFacts) -> PolicyFacts {
    let complete = backup.root_folder_id.is_some()
        && backup.quality_profile_id.is_some()
        && backup.minimum_availability.is_some();
    PolicyFacts {
        monitored: if complete { backup.monitored } else { 0 },
        search_on_add: Some(backup.search_on_add.unwrap_or(0)),
        ..backup.clone()
    }
}

fn first_difference(expected: &PolicyFacts, live: &PolicyFacts) -> &'static str {
    if expected.settings_revision != live.settings_revision {
        "settings_revision_differs"
    } else if expected.monitored != live.monitored {
        "monitored_differs"
    } else if expected.root_folder_id != live.root_folder_id {
        "root_folder_differs"
    } else if expected.quality_profile_id != live.quality_profile_id {
        "quality_profile_differs"
    } else if expected.minimum_availability != live.minimum_availability {
        "minimum_availability_differs"
    } else if expected.local_edit != live.local_edit {
        "local_edit_differs"
    } else if expected.snapshot_owned != live.snapshot_owned {
        "snapshot_owned_differs"
    } else if expected.tags != live.tags {
        "tags_differ"
    } else {
        "search_on_add_differs"
    }
}

/// Pure planning step: no I/O, deterministic order (ascending collection id).
pub fn plan(backup: &Snapshot, live: &Snapshot) -> Vec<Entry> {
    let live_by_tmdb: BTreeMap<i64, i64> = live
        .collections
        .iter()
        .filter_map(|(id, c)| c.tmdb_id.map(|t| (t, *id)))
        .collect();
    let mut entries = Vec::new();
    for (&id, b) in &backup.collections {
        let Some(bs) = &b.settings else { continue };
        let Some(tmdb) = b.tmdb_id else {
            entries.push(entry(
                id,
                None,
                Decision::Ambiguous,
                "backup_collection_has_no_tmdb_identity",
            ));
            continue;
        };
        let live_row = live.collections.get(&id);
        let identity_ok = match (live_row, live_by_tmdb.get(&tmdb)) {
            (None, None) => {
                entries.push(entry(
                    id,
                    Some(tmdb),
                    Decision::MissingInLive,
                    "no_live_collection_with_this_id_or_tmdb_id",
                ));
                continue;
            }
            (Some(l), Some(&by_tmdb)) => by_tmdb == id && l.tmdb_id == Some(tmdb),
            _ => false,
        };
        if !identity_ok {
            entries.push(entry(
                id,
                Some(tmdb),
                Decision::Ambiguous,
                "internal_id_and_tmdb_id_disagree",
            ));
            continue;
        }
        match bs.search_on_add {
            Some(0) => {
                entries.push(entry(
                    id,
                    Some(tmdb),
                    Decision::ExplicitFalsePreserved,
                    "backup_recorded_explicit_false",
                ));
                continue;
            }
            Some(_) => {
                entries.push(entry(
                    id,
                    Some(tmdb),
                    Decision::ExplicitTruePreserved,
                    "backup_recorded_explicit_true",
                ));
                continue;
            }
            None => {}
        }
        let Some(ls) = live_row.and_then(|l| l.settings.as_ref()) else {
            entries.push(entry(
                id,
                Some(tmdb),
                Decision::LocalEditSinceBackup,
                "live_settings_row_absent",
            ));
            continue;
        };
        if ls.search_on_add.is_none() {
            entries.push(entry(
                id,
                Some(tmdb),
                Decision::AlreadyNull,
                "live_already_absent",
            ));
            continue;
        }
        let expected = converted(bs);
        let e = if ls.search_on_add != Some(0) || expected != *ls {
            entry(
                id,
                Some(tmdb),
                Decision::LocalEditSinceBackup,
                first_difference(&expected, ls),
            )
        } else if expected.monitored != 0 {
            // NULL is only legal for unmonitored collections; a monitored row cannot be restored.
            entry(
                id,
                Some(tmdb),
                Decision::Ambiguous,
                "monitored_collection_cannot_hold_absent_policy",
            )
        } else if bs.local_edit != 0 {
            // The inline policy had no revision counter for subsequent policy edits. Once
            // local_edit was already set, choosing false later leaves the same facts as the
            // lossy conversion. Matching fields therefore do not prove permission to restore.
            entry(
                id,
                Some(tmdb),
                Decision::Ambiguous,
                "backup_row_already_had_local_edit_so_a_later_search_on_add_only_edit_is_undetectable",
            )
        } else {
            let mut restorable = entry(
                id,
                Some(tmdb),
                Decision::Restorable,
                "live_equals_conversion_of_backup_row",
            );
            restorable.live_revision = Some(ls.settings_revision);
            restorable
        };
        entries.push(e);
    }
    // Live false values with no usable backup row cannot be proven either way.
    for (&id, l) in &live.collections {
        let at_risk = l
            .settings
            .as_ref()
            .is_some_and(|s| s.search_on_add == Some(0));
        let backup_has_row = backup
            .collections
            .get(&id)
            .is_some_and(|b| b.settings.is_some());
        if at_risk && !backup_has_row {
            entries.push(entry(
                id,
                l.tmdb_id,
                Decision::UnrecoverableNoBackupProvenance,
                "backup_has_no_settings_row_for_this_collection",
            ));
        }
    }
    entries.sort_by_key(|e| e.collection_id);
    entries
}

/// With no usable backup every live false is unprovable.
pub fn plan_without_backup(live: &Snapshot, reason: &'static str) -> Vec<Entry> {
    live.collections
        .iter()
        .filter(|(_, l)| {
            l.settings
                .as_ref()
                .is_some_and(|s| s.search_on_add == Some(0))
        })
        .map(|(&id, l)| {
            entry(
                id,
                l.tmdb_id,
                Decision::UnrecoverableNoBackupProvenance,
                reason,
            )
        })
        .collect()
}

fn counts(entries: &[Entry]) -> BTreeMap<&'static str, usize> {
    let mut map: BTreeMap<&'static str, usize> =
        Decision::ALL.iter().map(|d| (d.name(), 0)).collect();
    for e in entries {
        *map.entry(e.decision.name()).or_default() += 1;
    }
    map
}

async fn load_snapshot(conn: &Connection) -> Result<Snapshot, Error> {
    let mut snapshot = Snapshot::default();
    let mut rows = conn
        .query(
            "SELECT id, tmdb_id FROM movie_collections ORDER BY id LIMIT ?1",
            [MAX_COLLECTIONS as i64 + 1],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        if snapshot.collections.len() >= MAX_COLLECTIONS {
            return Err(
                format!("more than {MAX_COLLECTIONS} collections; refusing to load").into(),
            );
        }
        snapshot.collections.insert(
            row.get::<i64>(0)?,
            CollectionFacts {
                tmdb_id: row.get::<Option<i64>>(1)?,
                settings: None,
            },
        );
    }
    let mut rows = conn
        .query(
            "SELECT collection_id, settings_revision, monitored, root_folder_id, quality_profile_id,
                    minimum_availability, search_on_add, local_edit, snapshot_owned
             FROM movie_collection_settings ORDER BY collection_id LIMIT ?1",
            [MAX_COLLECTIONS as i64 + 1],
        )
        .await?;
    let mut settings_rows = 0usize;
    while let Some(row) = rows.next().await? {
        settings_rows += 1;
        if settings_rows > MAX_COLLECTIONS {
            return Err(
                format!("more than {MAX_COLLECTIONS} settings rows; refusing to load").into(),
            );
        }
        let id = row.get::<i64>(0)?;
        let facts = PolicyFacts {
            settings_revision: row.get(1)?,
            monitored: row.get(2)?,
            root_folder_id: row.get(3)?,
            quality_profile_id: row.get(4)?,
            minimum_availability: row.get(5)?,
            search_on_add: row.get(6)?,
            local_edit: row.get(7)?,
            snapshot_owned: row.get(8)?,
            tags: BTreeSet::new(),
        };
        // The settings table references the collection table, so the parent exists.
        snapshot
            .collections
            .get_mut(&id)
            .ok_or("settings row without a collection")?
            .settings = Some(facts);
    }
    let mut rows = conn
        .query(
            "SELECT collection_id, tag_id FROM movie_collection_tags LIMIT ?1",
            [MAX_TAG_ROWS as i64 + 1],
        )
        .await?;
    let mut tag_rows = 0usize;
    while let Some(row) = rows.next().await? {
        tag_rows += 1;
        if tag_rows > MAX_TAG_ROWS {
            return Err(
                format!("more than {MAX_TAG_ROWS} collection tags; refusing to load").into(),
            );
        }
        let id = row.get::<i64>(0)?;
        let tag = row.get::<i64>(1)?;
        if let Some(settings) = snapshot
            .collections
            .get_mut(&id)
            .and_then(|c| c.settings.as_mut())
        {
            settings.tags.insert(tag);
        }
    }
    Ok(snapshot)
}

/// Digest of every ledger column, so "the ledger is untouched" is a byte-level claim.
pub async fn ledger_digest(conn: &Connection) -> Result<String, Error> {
    let mut rows = conn
        .query(
            "SELECT version, name, checksum, sql, applied_at FROM schema_migrations ORDER BY version",
            (),
        )
        .await?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    while let Some(row) = rows.next().await? {
        digest.update(&row.get::<i64>(0)?.to_le_bytes());
        for column in 1..5 {
            let text = row.get::<String>(column)?;
            digest.update(&(text.len() as u64).to_le_bytes());
            digest.update(text.as_bytes());
        }
    }
    Ok(hex(digest.finish().as_ref()))
}

/// Verified facts about the backup that the report records.
#[derive(Debug, Clone)]
pub struct BackupEvidence {
    pub database_sha256: String,
    pub manifest_file_sha256: String,
    pub schema_version: usize,
    pub snapshot: Snapshot,
}

/// Removes the scratch directory on every exit path.
pub(super) struct Scratch(pub(super) PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"WARN","event":"maintenance_scratch_not_removed","error_kind":format!("{:?}", error.kind())})
                );
            }
        }
    }
}

pub(super) fn private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub(super) fn sha256_hex(digest: ring::digest::Context) -> String {
    hex(digest.finish().as_ref())
}

struct ManifestFacts {
    file_sha256: String,
    database_sha256: String,
    schema_version: u64,
}

fn read_manifest(directory: &Path) -> Result<ManifestFacts, &'static str> {
    let path = directory.join("manifest.json");
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "backup_manifest_missing")?;
    if !meta.is_file() {
        return Err("backup_manifest_not_a_regular_file");
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("backup_manifest_too_large");
    }
    let bytes = std::fs::read(&path).map_err(|_| "backup_manifest_unreadable")?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("backup_manifest_too_large");
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "backup_manifest_invalid")?;
    if value["format"] != 1
        || value["purpose"] != "pre-migration recovery"
        || value["database"] != "database.db"
    {
        return Err("backup_manifest_not_a_pre_migration_backup");
    }
    let sha = value["sha256"].as_str().ok_or("backup_manifest_invalid")?;
    if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("backup_manifest_invalid");
    }
    let schema_version = value["schema_version"]
        .as_u64()
        .ok_or("backup_manifest_invalid")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(&bytes);
    Ok(ManifestFacts {
        file_sha256: sha256_hex(digest),
        database_sha256: sha.to_owned(),
        schema_version,
    })
}

/// Copies `database.db` into scratch while hashing exactly the bytes copied, so the verified
/// bytes are the opened bytes.
fn copy_and_hash(directory: &Path, copy: &Path) -> Result<String, &'static str> {
    let source_path = directory.join("database.db");
    let meta = std::fs::symlink_metadata(&source_path).map_err(|_| "backup_database_missing")?;
    if !meta.is_file() {
        return Err("backup_database_not_a_regular_file");
    }
    if meta.len() > MAX_BACKUP_BYTES {
        return Err("backup_database_too_large");
    }
    let mut source = File::open(&source_path).map_err(|_| "backup_database_unreadable")?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(copy)
        .map_err(|_| "backup_scratch_unwritable")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        let count = source
            .read(&mut buffer)
            .map_err(|_| "backup_database_unreadable")?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_BACKUP_BYTES {
            return Err("backup_database_too_large");
        }
        digest.update(&buffer[..count]);
        target
            .write_all(&buffer[..count])
            .map_err(|_| "backup_scratch_unwritable")?;
    }
    target.sync_all().map_err(|_| "backup_scratch_unwritable")?;
    Ok(sha256_hex(digest))
}

/// By value, never by address: `LEGACY_MIGRATIONS` is a `const`, so two uses need not share storage.
fn is_legacy(history: &[(&str, &str)]) -> bool {
    history == LEGACY_MIGRATIONS
}

/// Why a backup cannot serve as provenance. Never an I/O error text: codes only.
pub type Rejection = &'static str;

/// Validates the backup directory and loads its collection facts. Backup problems are values
/// (`Err(Rejection)`), not process errors.
async fn load_backup(directory: &Path, scratch: &Path) -> Result<BackupEvidence, Rejection> {
    let dir = directory.to_path_buf();
    let copy = scratch.join("backup.db");
    let copy_for_blocking = copy.clone();
    let (manifest, actual_sha) = tokio::task::spawn_blocking(move || {
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| "backup_directory_missing")?;
        if !meta.is_dir() {
            return Err("backup_directory_not_a_directory");
        }
        let manifest = read_manifest(&dir)?;
        let actual = copy_and_hash(&dir, &copy_for_blocking)?;
        Ok((manifest, actual))
    })
    .await
    .map_err(|_| "backup_validation_aborted")??;
    if actual_sha != manifest.database_sha256 {
        return Err("backup_database_sha256_mismatch");
    }
    let inspected = inspect_copy(&copy, manifest.schema_version).await;
    let (schema_version, snapshot) = inspected?;
    Ok(BackupEvidence {
        database_sha256: manifest.database_sha256,
        manifest_file_sha256: manifest.file_sha256,
        schema_version,
        snapshot,
    })
}

async fn inspect_copy(copy: &Path, manifest_version: u64) -> Result<(usize, Snapshot), Rejection> {
    let database = libsql::Builder::new_local(copy)
        .build()
        .await
        .map_err(|_| "backup_database_unopenable")?;
    let conn = database
        .connect()
        .map_err(|_| "backup_database_unopenable")?;
    integrity(&conn)
        .await
        .map_err(|_| "backup_integrity_check_failed")?;
    let (version, history) = migration_history(&conn)
        .await
        .map_err(|_| "backup_ledger_not_a_published_history")?;
    if !is_legacy(history) {
        return Err("backup_ledger_not_the_published_pr31_lineage");
    }
    if version as u64 != manifest_version {
        return Err("backup_manifest_schema_version_disagrees_with_ledger");
    }
    if version < FIRST_NULLABLE_VERSION {
        return Err("backup_predates_collection_settings");
    }
    if version > LAST_NULLABLE_VERSION {
        return Err("backup_taken_after_the_policy_conversion");
    }
    // The ledger names the schema; confirm the column really is nullable.
    let mut rows = conn
        .query("PRAGMA table_info(movie_collection_settings)", ())
        .await
        .map_err(|_| "backup_database_unreadable")?;
    let mut nullable = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| "backup_database_unreadable")?
    {
        let name: String = row.get(1).map_err(|_| "backup_database_unreadable")?;
        let not_null: i64 = row.get(3).map_err(|_| "backup_database_unreadable")?;
        if name == "search_on_add" && not_null == 0 {
            nullable = true;
        }
    }
    if !nullable {
        return Err("backup_search_on_add_not_nullable");
    }
    let snapshot = load_snapshot(&conn)
        .await
        .map_err(|_| "backup_collections_unreadable")?;
    Ok((version, snapshot))
}

#[derive(Debug, Clone)]
pub struct Options {
    pub database: PathBuf,
    pub backup: PathBuf,
    pub apply: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    DryRun,
    Applied,
    /// The backup cannot prove anything; nothing was applied.
    Unrecoverable,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::DryRun => "dry_run",
            Outcome::Applied => "applied",
            Outcome::Unrecoverable => "unrecoverable",
        }
    }
}

#[derive(Debug)]
pub struct RunReport {
    pub outcome: Outcome,
    pub report_path: PathBuf,
    pub restored: usize,
    pub counts: BTreeMap<&'static str, usize>,
    pub entries: Vec<Entry>,
    pub json: serde_json::Value,
}

pub(super) fn unix_seconds() -> Result<u64, Error> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

/// Reserves the report name before the database is touched so an unwritable location fails early.
pub(super) fn reserve_report(
    directory: &Path,
    stem: &str,
    mode: &str,
) -> Result<(File, PathBuf), Error> {
    let path = directory.join(format!(
        "hrrdarr-{stem}-{mode}-{}-{}.json",
        unix_seconds()?,
        uuid::Uuid::new_v4()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|e| format!("cannot create the audit report next to the database: {e}"))?;
    Ok((file, path))
}

pub(super) fn write_report(mut file: File, json: &serde_json::Value) -> Result<(), Error> {
    file.write_all(serde_json::to_string_pretty(json)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

async fn open_live(
    path: &Path,
    query_only: bool,
) -> Result<(File, libsql::Database, Connection), Error> {
    let lock_path = path.to_path_buf();
    // No create: a missing database is an error, not a new empty database.
    let owner = tokio::task::spawn_blocking(move || -> Result<File, Error> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("cannot open the database file: {e}"))?;
        file.try_lock().map_err(|e| {
            format!("database already owned or cannot be locked (stop hrrdarr first): {e}")
        })?;
        Ok(file)
    })
    .await??;
    let database = libsql::Builder::new_local(path).build().await?;
    let conn = database.connect()?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute("PRAGMA foreign_keys = ON", ()).await?;
    if scalar(&conn, "PRAGMA foreign_keys").await? != 1 {
        return Err("foreign key enforcement unavailable".into());
    }
    if query_only {
        conn.execute("PRAGMA query_only = ON", ()).await?;
    }
    Ok((owner, database, conn))
}

/// Runs the recovery. `Err` is reserved for problems with the live database or the environment;
/// an unusable backup is an `Ok` report with [`Outcome::Unrecoverable`].
pub async fn run(options: &Options) -> Result<RunReport, Error> {
    let live_dir = options
        .database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mode = if options.apply { "apply" } else { "dry-run" };
    if let (Ok(backup), Ok(dir)) = (
        std::fs::canonicalize(&options.backup),
        std::fs::canonicalize(&live_dir),
    ) {
        if backup == dir {
            return Err("the backup directory must not be the database directory".into());
        }
    }
    // The report is reserved first and never placed inside the backup.
    let (report_file, report_path) = reserve_report(&live_dir, "collection-policy-recovery", mode)?;
    let result = run_inner(options, &live_dir).await;
    match result {
        Ok(mut report) => {
            report.report_path = report_path;
            report.json["report_file"] = serde_json::Value::String(
                report
                    .report_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
            write_report(report_file, &report.json)?;
            emit_summary(&report);
            Ok(report)
        }
        Err((error, partial)) => {
            let mut json = partial.unwrap_or_else(|| {
                serde_json::json!({
                    "format": REPORT_FORMAT,
                    "tool": format!("hrrdarr {COMMAND}"),
                    "mode": mode,
                    "applied": false,
                })
            });
            json["outcome"] = "failed".into();
            json["error_class"] = classify(&error).into();
            if let Err(write_error) = write_report(report_file, &json) {
                return Err(format!(
                    "{error}; additionally the report could not be written: {write_error}"
                )
                .into());
            }
            Err(error)
        }
    }
}

fn classify(error: &Error) -> &'static str {
    if error.to_string().contains("rolled back") {
        "apply_failed_rolled_back"
    } else {
        "failed"
    }
}

fn emit_summary(report: &RunReport) {
    eprintln!(
        "{}",
        serde_json::json!({
            "level": "INFO",
            "event": "collection_policy_recovery",
            "outcome": report.outcome.name(),
            "applied": report.json["applied"],
            "restored": report.restored,
            "counts": report.counts,
            "report_file": report.json["report_file"],
        })
    );
}

type Failure = (Error, Option<serde_json::Value>);

fn fail(error: impl Into<Error>) -> Failure {
    (error.into(), None)
}

async fn run_inner(options: &Options, live_dir: &Path) -> Result<RunReport, Failure> {
    let (owner, database, conn) = open_live(&options.database, !options.apply)
        .await
        .map_err(fail)?;
    // Both stay alive for the whole run: the lock excludes a running service.
    let _owner = owner;
    let _database = database;
    let (version, history) = migration_history(&conn).await.map_err(fail)?;
    if !is_legacy(history) {
        return Err(fail(
            "the live database is on the main migration lineage, which never ran the PR #31 conversion; nothing to recover",
        ));
    }
    if version != LEGACY_MIGRATIONS.len() {
        return Err(fail(format!(
            "the live database is at schema version {version}; start hrrdarr once to complete the upgrade to {} before recovering",
            LEGACY_MIGRATIONS.len()
        )));
    }
    integrity(&conn).await.map_err(fail)?;
    let ledger_before = ledger_digest(&conn).await.map_err(fail)?;
    let live = load_snapshot(&conn).await.map_err(fail)?;

    let scratch_path = live_dir.join(format!(".hrrdarr-recovery-{}", uuid::Uuid::new_v4()));
    private_directory(&scratch_path)
        .map_err(|e| fail(format!("cannot create scratch space: {e}")))?;
    let scratch = Scratch(scratch_path);
    let backup = load_backup(&options.backup, &scratch.0).await;

    let mut json = serde_json::json!({
        "format": REPORT_FORMAT,
        "tool": format!("hrrdarr {COMMAND}"),
        "tool_version": env!("CARGO_PKG_VERSION"),
        "mode": if options.apply { "apply" } else { "dry_run" },
        "created_at_unix_seconds": unix_seconds().map_err(fail)?,
        "revision_consequence": REVISION_CONSEQUENCE,
        "live": { "schema_version": version, "ledger_sha256_before": ledger_before },
    });
    let evidence = match backup {
        Ok(evidence) => evidence,
        Err(reason) => {
            let entries = plan_without_backup(&live, reason);
            json["applied"] = false.into();
            json["outcome"] = Outcome::Unrecoverable.name().into();
            json["backup"] = serde_json::json!({ "valid": false, "rejection": reason });
            return Ok(finish(json, entries, 0, Outcome::Unrecoverable, &conn).await);
        }
    };
    json["backup"] = serde_json::json!({
        "valid": true,
        "manifest_file_sha256": evidence.manifest_file_sha256,
        "database_sha256": evidence.database_sha256,
        "database_sha256_verified": true,
        "schema_version": evidence.schema_version,
        "ledger_is_exact_published_pr31_prefix": true,
    });
    let entries = plan(&evidence.snapshot, &live);
    if !options.apply {
        json["applied"] = false.into();
        return Ok(finish(json, entries, 0, Outcome::DryRun, &conn).await);
    }
    match apply(&conn, &evidence.snapshot, &ledger_before).await {
        Ok((entries, restored)) => {
            json["applied"] = true.into();
            Ok(finish(json, entries, restored, Outcome::Applied, &conn).await)
        }
        Err(error) => {
            json["applied"] = false.into();
            json["backup"]["valid"] = true.into();
            json["counts"] = serde_json::to_value(counts(&entries)).unwrap_or_default();
            Err((error, Some(json)))
        }
    }
}

async fn finish(
    mut json: serde_json::Value,
    entries: Vec<Entry>,
    restored: usize,
    outcome: Outcome,
    conn: &Connection,
) -> RunReport {
    let counts = counts(&entries);
    json["outcome"] = outcome.name().into();
    json["restored"] = restored.into();
    json["counts"] = serde_json::to_value(&counts).unwrap_or_default();
    json["collections"] = serde_json::to_value(&entries).unwrap_or_default();
    // Recorded for the audit trail; a failure here cannot undo the finished run.
    json["live"]["ledger_sha256_after"] = match ledger_digest(conn).await {
        Ok(digest) => digest.into(),
        Err(_) => serde_json::Value::Null,
    };
    RunReport {
        outcome,
        report_path: PathBuf::new(),
        restored,
        counts,
        entries,
        json,
    }
}

/// One immediate transaction: re-plan from locked live data, restore, verify, commit.
async fn apply(
    conn: &Connection,
    backup: &Snapshot,
    ledger_before: &str,
) -> Result<(Vec<Entry>, usize), Error> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result: Result<(Vec<Entry>, usize), Error> = async {
        if ledger_digest(&tx).await? != ledger_before {
            return Err("the migration ledger changed after planning".into());
        }
        let live = load_snapshot(&tx).await?;
        let entries = plan(backup, &live);
        let mut restored = 0usize;
        for e in entries.iter().filter(|e| e.decision == Decision::Restorable) {
            let revision = e.live_revision.ok_or("restorable entry without a revision")?;
            // Conditioned on every fact the plan relied on; the trigger demands revision + 1.
            let changed = tx
                .execute(
                    "UPDATE movie_collection_settings
                     SET search_on_add = NULL, settings_revision = settings_revision + 1
                     WHERE collection_id = ?1 AND search_on_add = 0 AND monitored = 0 AND settings_revision = ?2",
                    params![e.collection_id, revision],
                )
                .await?;
            if changed != 1 {
                return Err(format!("collection {} changed during apply", e.collection_id).into());
            }
            restored += 1;
        }
        // Verify the result before commit: every restored row is now absent, nothing else moved.
        let after = load_snapshot(&tx).await?;
        let replanned = plan(backup, &after);
        let expect_null = counts(&entries)["restorable"] + counts(&entries)["already_null"];
        let got = counts(&replanned);
        if got["restorable"] != 0 || got["already_null"] != expect_null {
            return Err("post-apply verification disagrees with the plan".into());
        }
        for (id, before) in &live.collections {
            let now = after.collections.get(id).ok_or("collection vanished")?;
            let restored_row = entries
                .iter()
                .any(|e| e.collection_id == *id && e.decision == Decision::Restorable);
            let same = match (&before.settings, &now.settings) {
                (Some(b), Some(n)) if restored_row => {
                    PolicyFacts {
                        search_on_add: None,
                        settings_revision: b.settings_revision + 1,
                        ..b.clone()
                    } == *n
                }
                (b, n) => b == n,
            } && before.tmdb_id == now.tmdb_id;
            if !same {
                return Err(format!("collection {id} changed beyond the intended restore").into());
            }
        }
        if after.collections.len() != live.collections.len() {
            return Err("collection set changed during apply".into());
        }
        if ledger_digest(&tx).await? != ledger_before {
            return Err("the migration ledger changed during apply".into());
        }
        integrity(&tx).await?;
        Ok((entries, restored))
    }
    .await;
    match result {
        Ok(done) => {
            tx.commit().await?;
            Ok(done)
        }
        Err(error) => {
            tx.rollback().await.map_err(|rollback| {
                format!("apply failed ({error}); rollback also failed ({rollback})")
            })?;
            Err(format!("apply failed and was rolled back; database unchanged: {error}").into())
        }
    }
}

/// Parsed command line, minimal and dependency-free.
pub fn parse_args(args: &[String]) -> Result<(PathBuf, bool), String> {
    let mut backup: Option<PathBuf> = None;
    let mut apply = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value_of = |v: &str| -> Result<PathBuf, String> {
            if v.is_empty() {
                Err("--backup needs a directory".to_owned())
            } else {
                Ok(PathBuf::from(v))
            }
        };
        match arg.as_str() {
            "--apply" if !apply => apply = true,
            "--apply" => return Err("--apply given twice".into()),
            "--backup" if backup.is_none() => {
                let v = iter.next().ok_or("--backup needs a directory")?;
                backup = Some(value_of(v)?);
            }
            other if other.starts_with("--backup=") && backup.is_none() => {
                backup = Some(value_of(&other["--backup=".len()..])?);
            }
            "--backup" => return Err("--backup given twice".into()),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok((backup.ok_or("--backup <directory> is required")?, apply))
}

/// Entry point for `hrrdarr recover-collection-policy`. Exit codes: 0 done, 1 error or bad usage,
/// 3 the backup cannot prove anything (nothing applied).
pub async fn run_cli(args: &[String]) -> std::process::ExitCode {
    let (backup, apply) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return std::process::ExitCode::from(1);
        }
    };
    if std::env::var_os("TURSO_DATABASE_URL").is_some()
        && std::env::var_os("TURSO_AUTH_TOKEN").is_some()
    {
        eprintln!(
            "remote storage is configured; this recovery works on the local database file only (unset TURSO_DATABASE_URL to use it)"
        );
        return std::process::ExitCode::from(1);
    }
    let database = std::env::var_os("HRRDARR_DATABASE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("hrrdarr.db"));
    match run(&Options {
        database,
        backup,
        apply,
    })
    .await
    {
        Ok(report) if report.outcome == Outcome::Unrecoverable => std::process::ExitCode::from(3),
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::json!({"level":"ERROR","event":"collection_policy_recovery_failed","error":error.to_string()})
            );
            std::process::ExitCode::from(1)
        }
    }
}
