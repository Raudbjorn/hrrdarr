//! Populated fixtures for the collection policy recovery: a published main v53 database with real
//! nullable policy rows is taken through the real legacy history to version 82 (the published PR #31
//! conversion, without the capture hook) and then through the current runner, including the version-83 adapter.
use super::collection_policy_recovery::*;
use super::published_prefix_tests::{Scratch, published, seed_lineage};
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const POPULATE: &str = "
    INSERT INTO root_folders(id,media_type,path) VALUES(1,'movies','/movies');
    INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'movies','Any');
    INSERT INTO tags(id,media_type,label) VALUES(1,'movies','kept'),(2,'movies','also');
    INSERT INTO movie_collections(id,tmdb_id,title) VALUES
        (1,101,'Null tagged'),(2,102,'Explicit false'),(3,103,'Explicit true'),
        (4,104,'Edited later'),(5,105,'Monitored'),(6,106,'Null with policy'),
        (7,107,'Null plain'),(8,108,'Catalog only');
    INSERT INTO movie_collection_settings(collection_id,settings_revision,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned) VALUES
        (1,3,0,NULL,NULL,NULL,NULL,0,0),
        (2,2,0,NULL,NULL,NULL,0,1,0),
        (3,2,0,NULL,NULL,NULL,1,0,1),
        (4,4,0,NULL,NULL,NULL,NULL,0,0),
        (5,5,1,1,1,'released',1,1,0),
        (6,6,0,1,1,'in_cinemas',NULL,1,1),
        (7,1,0,NULL,NULL,NULL,NULL,0,0);
    INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES (1,1),(6,1),(6,2),(5,2);
    INSERT INTO movie_collection_intents(tmdb_id,revision,local_edit,removed,removal_reason) VALUES (900,7,1,1,'user');";

struct Fixture {
    files: Scratch,
    db: PathBuf,
    /// The validated pre-upgrade backup taken at v53.
    backup: PathBuf,
    /// The backup the current runner took at v82, after the NULL was already lost.
    late_backup: Option<PathBuf>,
}

fn backups_in(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".hrrdarr-migration-"))
        })
        .collect();
    found.sort();
    found
}

async fn advance(c: &Connection, from: usize, to: usize) -> Result<(), Error> {
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    for (i, (name, sql)) in LEGACY_MIGRATIONS.iter().enumerate().take(to).skip(from) {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    integrity(c).await?;
    Ok(())
}

/// How far the fixture is taken.
#[derive(Clone, Copy, PartialEq)]
enum Upgrade {
    /// Published PR #31 runner only (version 82).
    PublishedOnly,
    /// Published runner, then the current runner: the case this recovery serves.
    ThroughPublished,
}

async fn fixture(upgrade: Upgrade) -> Result<Fixture, Error> {
    let files = Scratch::new();
    let db = files.0.join("db");
    let (raw, c) = published(&db, 53).await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute_batch(POPULATE).await?;
    // Exactly what Database::backup() writes before any pending migration runs.
    let backup = super::backup(&c, &db).await?;
    advance(&c, 53, 82).await?;
    // A local edit made after the backup, through the PR #31 inline policy columns.
    c.execute(
        "UPDATE movie_collections SET local_edit=1, root_folder_id=1, minimum_availability='released' WHERE id=4",
        (),
    )
    .await?;
    drop(c);
    drop(raw);
    let mut late_backup = None;
    if upgrade == Upgrade::ThroughPublished {
        let opened = Database::open_local(&db).await?;
        assert!(opened.migration_backup().is_some());
        drop(opened);
        late_backup = backups_in(&files.0).into_iter().find(|p| *p != backup);
        assert!(late_backup.is_some());
    }
    Ok(Fixture {
        files,
        db,
        backup,
        late_backup,
    })
}

fn file_sha(path: &Path) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, &std::fs::read(path).unwrap()).as_ref())
}

/// Settings, tags, collections, intents and the ledger, as text.
async fn dump(path: &Path) -> Result<String, Error> {
    let db = Database::open_local(path).await?;
    assert!(
        db.migration_backup().is_none(),
        "inspection must not migrate"
    );
    let c = db.connect().await?;
    let mut out = String::new();
    for sql in [
        "SELECT collection_id,settings_revision,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned FROM movie_collection_settings ORDER BY 1",
        "SELECT collection_id,tag_id FROM movie_collection_tags ORDER BY 1,2",
        "SELECT id,tmdb_id,title,metadata_revision FROM movie_collections ORDER BY 1",
        "SELECT tmdb_id,revision,local_edit,removed,removal_reason FROM movie_collection_intents ORDER BY 1",
        "SELECT version,name,checksum,sql,applied_at FROM schema_migrations ORDER BY 1",
    ] {
        let mut rows = c.query(sql, ()).await?;
        while let Some(row) = rows.next().await? {
            for i in 0..row.column_count() {
                out.push_str(&format!("{:?}|", row.get_value(i)?));
            }
            out.push('\n');
        }
    }
    Ok(out)
}

async fn settings_row(path: &Path, id: i64) -> Result<(Option<i64>, i64), Error> {
    let db = Database::open_local(path).await?;
    let c = db.connect().await?;
    let row = c
        .query(
            "SELECT search_on_add, settings_revision FROM movie_collection_settings WHERE collection_id=?",
            [id],
        )
        .await?
        .next()
        .await?
        .ok_or("no settings row")?;
    Ok((row.get(0)?, row.get(1)?))
}

fn options(f: &Fixture, backup: &Path, apply: bool) -> Options {
    Options {
        database: f.db.clone(),
        backup: backup.to_path_buf(),
        apply,
    }
}

fn decisions(report: &RunReport) -> BTreeMap<i64, &'static str> {
    report
        .entries
        .iter()
        .map(|e| (e.collection_id, e.decision.name()))
        .collect()
}

async fn rewrite_backup(directory: &Path, sql: &str) -> Result<(), Error> {
    let database = directory.join("database.db");
    {
        let raw = libsql::Builder::new_local(&database).build().await?;
        let c = raw.connect()?;
        c.execute("PRAGMA foreign_keys=OFF", ()).await?;
        c.execute_batch(sql).await?;
    }
    reseal(directory, |_| {});
    Ok(())
}

/// Recomputes the manifest hash so only the intended defect remains.
fn reseal(directory: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let manifest_path = directory.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["sha256"] = file_sha(&directory.join("database.db")).into();
    edit(&mut manifest);
    std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

#[tokio::test]
async fn published_conversion_loses_the_distinction_but_the_forward_hook_keeps_it()
-> Result<(), Error> {
    let lost = fixture(Upgrade::ThroughPublished).await?;
    let db = Database::open_local(&lost.db).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, LEGACY_MIGRATIONS.len());
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE search_on_add IS NULL"
        )
        .await?,
        0,
        "after the published version-82 conversion no row is absent any more"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE search_on_add=0"
        )
        .await?,
        5,
        "the absent rows 1,4,6,7 collapsed into false, next to the explicit false row 2"
    );
    drop(c);
    drop(db);

    // Control: the same v53 data through the current runner keeps NULL (the hook).
    let files = Scratch::new();
    let path = files.0.join("db");
    let (raw, c) = published(&path, 53).await?;
    c.execute_batch(POPULATE).await?;
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE search_on_add IS NULL"
        )
        .await?,
        4
    );
    Ok(())
}

#[tokio::test]
async fn dry_run_reports_and_never_writes_the_database() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    let before_file = file_sha(&f.db);
    let before = dump(&f.db).await?;
    assert_eq!(
        file_sha(&f.db),
        before_file,
        "inspection alone is read-only"
    );

    let report = run(&options(&f, &f.backup, false)).await?;
    assert_eq!(report.outcome, Outcome::DryRun);
    assert_eq!(report.restored, 0);
    assert_eq!(
        file_sha(&f.db),
        before_file,
        "database file checksum unchanged"
    );
    assert_eq!(dump(&f.db).await?, before);
    assert_eq!(
        decisions(&report),
        BTreeMap::from([
            (1, "restorable"),
            (2, "explicit_false_preserved"),
            (3, "explicit_true_preserved"),
            (4, "local_edit_since_backup"),
            (5, "explicit_true_preserved"),
            (6, "ambiguous"),
            (7, "restorable"),
        ])
    );
    // Row 6 already had local_edit=1: an exact match cannot exclude a later search-only edit.
    assert_eq!(report.counts["restorable"], 2);
    assert_eq!(
        report.json["applied"], false,
        "a dry run says so in the audit file"
    );

    // Audit file: next to the database, 0600, never in the backup, identities and decisions only.
    assert_eq!(report.report_path.parent(), f.db.parent());
    assert!(!report.report_path.starts_with(&f.backup));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&report.report_path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    let text = std::fs::read_to_string(&report.report_path)?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(json["mode"], "dry_run");
    assert_eq!(json["backup"]["schema_version"], 53);
    assert_eq!(json["backup"]["database_sha256_verified"], true);
    assert_eq!(
        json["backup"]["database_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(
        json["backup"]["manifest_file_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        json["live"]["ledger_sha256_before"],
        json["live"]["ledger_sha256_after"]
    );
    assert!(
        json["revision_consequence"]
            .as_str()
            .unwrap()
            .contains("exactly 1")
    );
    for forbidden in [
        "/movies",
        "/tmp",
        "credential",
        "token",
        "password",
        "api_key",
    ] {
        assert!(!text.contains(forbidden), "report leaks {forbidden}");
    }
    assert!(!text.contains(f.files.0.to_str().unwrap()));
    // Reports are never overwritten: a second run creates a different file.
    let again = run(&options(&f, &f.backup, false)).await?;
    assert_ne!(again.report_path, report.report_path);
    Ok(())
}

#[tokio::test]
async fn apply_restores_only_proven_null_and_preserves_everything_else() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    let ledger_dump_before = dump(&f.db).await?;
    let mut revisions = BTreeMap::new();
    for id in 1..=7 {
        revisions.insert(id, settings_row(&f.db, id).await?);
    }

    let report = run(&options(&f, &f.backup, true)).await?;
    assert_eq!(report.outcome, Outcome::Applied);
    assert_eq!(report.restored, 2);
    assert_eq!(report.json["applied"], true);

    for (id, restored) in [
        (1, true),
        (2, false),
        (3, false),
        (4, false),
        (5, false),
        (6, false),
        (7, true),
    ] {
        let (soa, revision) = settings_row(&f.db, id).await?;
        let (soa_before, revision_before) = revisions[&id];
        if restored {
            assert_eq!(soa, None, "collection {id} restored to absent");
            assert_eq!(
                revision,
                revision_before + 1,
                "collection {id} revision +1 exactly"
            );
        } else {
            assert_eq!(
                (soa, revision),
                (soa_before, revision_before),
                "collection {id} untouched"
            );
        }
    }
    // Explicit false stays false, explicit true stays true, the locally edited one stays false.
    assert_eq!(settings_row(&f.db, 2).await?.0, Some(0));
    assert_eq!(settings_row(&f.db, 3).await?.0, Some(1));
    assert_eq!(settings_row(&f.db, 4).await?.0, Some(0));

    let db = Database::open_local(&f.db).await?;
    let c = db.connect().await?;
    // Monitoring/search behavior, policy and ownership untouched on every row.
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_settings WHERE collection_id=5 AND monitored=1 AND search_on_add=1 AND root_folder_id=1 AND quality_profile_id=1 AND minimum_availability='released' AND local_edit=1").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_settings WHERE collection_id=6 AND monitored=0 AND root_folder_id=1 AND quality_profile_id=1 AND minimum_availability='in_cinemas' AND local_edit=1 AND snapshot_owned=1").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_settings WHERE collection_id=4 AND root_folder_id=1 AND minimum_availability='released' AND local_edit=1").await?,
        1,
        "the post-backup local edit survives"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE monitored=1"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_tags").await?,
        4,
        "tags preserved"
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_tags WHERE (collection_id=1 AND tag_id=1) OR (collection_id=6 AND tag_id IN (1,2)) OR (collection_id=5 AND tag_id=2)").await?,
        4
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=900 AND revision=7 AND local_edit=1 AND removed=1 AND removal_reason='user'").await?,
        1,
        "removal intent preserved"
    );
    assert_eq!(scalar(&c, "PRAGMA foreign_keys").await?, 1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name='collection_settings_revision'").await?,
        1,
        "the revision trigger is still installed"
    );
    integrity(&c).await?;
    drop(c);
    drop(db);

    // The ledger rows are byte-identical (the dump covers every ledger column).
    let after = dump(&f.db).await?;
    let ledger_part = |text: &str| {
        let start = text
            .find("Integer(1)|Text(\"prototype\")")
            .expect("ledger start");
        text[start..].to_owned()
    };
    assert_eq!(ledger_part(&ledger_dump_before), ledger_part(&after));
    assert_eq!(
        report.json["live"]["ledger_sha256_before"],
        report.json["live"]["ledger_sha256_after"]
    );
    Ok(())
}

#[tokio::test]
async fn apply_is_idempotent() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    let first = run(&options(&f, &f.backup, true)).await?;
    assert_eq!(first.restored, 2);
    let after_first = dump(&f.db).await?;
    let file_after_first = file_sha(&f.db);

    let second = run(&options(&f, &f.backup, true)).await?;
    assert_eq!(second.outcome, Outcome::Applied);
    assert_eq!(second.restored, 0);
    assert_eq!(second.counts["restorable"], 0);
    assert_eq!(second.counts["already_null"], 2);
    assert_eq!(
        decisions(&second),
        BTreeMap::from([
            (1, "already_null"),
            (2, "explicit_false_preserved"),
            (3, "explicit_true_preserved"),
            (4, "local_edit_since_backup"),
            (5, "explicit_true_preserved"),
            (6, "ambiguous"),
            (7, "already_null"),
        ])
    );
    assert_eq!(
        dump(&f.db).await?,
        after_first,
        "second run changes nothing"
    );
    assert_eq!(file_sha(&f.db), file_after_first);
    Ok(())
}

#[tokio::test]
async fn an_undetectable_search_only_edit_is_ambiguous_and_apply_preserves_intent()
-> Result<(), Error> {
    let f = fixture(Upgrade::PublishedOnly).await?;
    let checkpoint_revision;
    {
        let raw = libsql::Builder::new_local(&f.db).build().await?;
        let c = raw.connect()?;
        // The fixture's tag inserts advanced the original revision; capture its actual value.
        checkpoint_revision = scalar(
            &c,
            "SELECT checkpoint_settings_revision FROM movie_collections WHERE id=6",
        )
        .await?;
        // Row 6 already had local_edit=1 in the nullable backup. Real inline-policy edits
        // cannot advance its retained checkpoint revision, even when choosing false anew.
        c.execute_batch(
            "UPDATE movie_collections SET search_on_add=1,local_edit=1 WHERE id=6;
             UPDATE movie_collections SET search_on_add=0,local_edit=1 WHERE id=6;
             UPDATE movie_collections SET search_on_add=1,local_edit=1 WHERE id IN (1,7);",
        )
        .await?;
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM movie_collections WHERE id=6 AND search_on_add=0 AND local_edit=1").await?,
            1,
            "the later explicit false is indistinguishable from the backup conversion"
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT checkpoint_settings_revision FROM movie_collections WHERE id=6"
            )
            .await?,
            checkpoint_revision,
            "the real search-only edits leave the backup revision unchanged"
        );
    }
    drop(Database::open_local(&f.db).await?);
    let before = dump(&f.db).await?;
    let before_file = file_sha(&f.db);
    // Other candidates were explicitly changed above, so neither mode has a safe restore.
    // The complete witness covers settings/revisions, tags, removal intents and the ledger.
    for apply in [false, true] {
        let report = run(&options(&f, &f.backup, apply)).await?;
        assert_eq!(report.restored, 0);
        assert_eq!(report.counts["restorable"], 0);
        let row = report
            .entries
            .iter()
            .find(|e| e.collection_id == 6)
            .unwrap();
        assert_eq!(row.decision, Decision::Ambiguous);
        assert_eq!(
            row.reason,
            "backup_row_already_had_local_edit_so_a_later_search_on_add_only_edit_is_undetectable"
        );
        assert_eq!(
            row.live_revision, None,
            "no conditional update is authorized"
        );
        assert_eq!(
            settings_row(&f.db, 6).await?,
            (Some(0), checkpoint_revision)
        );
        assert_eq!(dump(&f.db).await?, before);
        assert_eq!(file_sha(&f.db), before_file);
    }
    Ok(())
}

#[tokio::test]
async fn a_post_backup_edit_that_changes_a_compared_field_blocks_restore() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    {
        let db = Database::open_local(&f.db).await?;
        let c = db.connect().await?;
        // A tag change (revision bumped by the tag trigger), then an availability edit.
        c.execute(
            "INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(7,2)",
            (),
        )
        .await?;
        c.execute(
            "UPDATE movie_collection_settings SET minimum_availability='released', settings_revision=settings_revision+1, local_edit=1 WHERE collection_id=1",
            (),
        )
        .await?;
    }
    let report = run(&options(&f, &f.backup, true)).await?;
    let map = decisions(&report);
    assert_eq!(map[&1], "local_edit_since_backup");
    assert_eq!(map[&7], "local_edit_since_backup");
    // The remaining matching row has ambiguous edit provenance and must also stay untouched.
    assert_eq!(map[&6], "ambiguous");
    assert_eq!(report.restored, 0);
    assert_eq!(settings_row(&f.db, 1).await?.0, Some(0));
    assert_eq!(settings_row(&f.db, 7).await?.0, Some(0));
    assert_eq!(settings_row(&f.db, 6).await?.0, Some(0));
    Ok(())
}

async fn assert_unrecoverable(f: &Fixture, backup: &Path, reason: &str) -> Result<(), Error> {
    let before_file = file_sha(&f.db);
    let before = dump(&f.db).await?;
    for apply in [false, true] {
        let report = run(&options(f, backup, apply)).await?;
        assert_eq!(report.outcome, Outcome::Unrecoverable, "{reason}");
        assert_eq!(report.json["backup"]["rejection"], reason);
        assert_eq!(report.json["backup"]["valid"], false);
        assert_eq!(report.json["applied"], false);
        assert_eq!(report.restored, 0);
        // Every live false is reported as unprovable; nothing is guessed.
        assert_eq!(
            decisions(&report),
            BTreeMap::from([
                (2, "unrecoverable_no_backup_provenance"),
                (1, "unrecoverable_no_backup_provenance"),
                (4, "unrecoverable_no_backup_provenance"),
                (6, "unrecoverable_no_backup_provenance"),
                (7, "unrecoverable_no_backup_provenance"),
            ])
        );
        assert_eq!(
            file_sha(&f.db),
            before_file,
            "{reason}: database file untouched"
        );
        assert_eq!(dump(&f.db).await?, before);
    }
    Ok(())
}

#[tokio::test]
async fn missing_mismatched_tampered_or_late_backups_are_unrecoverable() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;

    assert_unrecoverable(
        &f,
        &f.files.0.join("no-such-backup"),
        "backup_directory_missing",
    )
    .await?;

    let no_manifest = f.files.0.join("no-manifest");
    std::fs::create_dir(&no_manifest)?;
    std::fs::copy(
        f.backup.join("database.db"),
        no_manifest.join("database.db"),
    )?;
    assert_unrecoverable(&f, &no_manifest, "backup_manifest_missing").await?;

    let no_database = f.files.0.join("no-database");
    std::fs::create_dir(&no_database)?;
    std::fs::copy(
        f.backup.join("manifest.json"),
        no_database.join("manifest.json"),
    )?;
    assert_unrecoverable(&f, &no_database, "backup_database_missing").await?;

    let tampered = f.files.0.join("tampered");
    std::fs::create_dir(&tampered)?;
    std::fs::copy(
        f.backup.join("manifest.json"),
        tampered.join("manifest.json"),
    )?;
    let mut bytes = std::fs::read(f.backup.join("database.db"))?;
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(tampered.join("database.db"), bytes)?;
    assert_unrecoverable(&f, &tampered, "backup_database_sha256_mismatch").await?;

    let lying_hash = f.files.0.join("lying-hash");
    std::fs::create_dir(&lying_hash)?;
    std::fs::copy(f.backup.join("database.db"), lying_hash.join("database.db"))?;
    std::fs::copy(
        f.backup.join("manifest.json"),
        lying_hash.join("manifest.json"),
    )?;
    reseal(&lying_hash, |m| m["sha256"] = "0".repeat(64).into());
    // reseal recomputes the hash first; overwrite it afterwards to forge the manifest.
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(lying_hash.join("manifest.json"))?)?;
    manifest["sha256"] = "0".repeat(64).into();
    std::fs::write(
        lying_hash.join("manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    assert_unrecoverable(&f, &lying_hash, "backup_database_sha256_mismatch").await?;

    let wrong_version = f.files.0.join("wrong-version");
    std::fs::create_dir(&wrong_version)?;
    std::fs::copy(
        f.backup.join("database.db"),
        wrong_version.join("database.db"),
    )?;
    std::fs::copy(
        f.backup.join("manifest.json"),
        wrong_version.join("manifest.json"),
    )?;
    reseal(&wrong_version, |m| m["schema_version"] = 60.into());
    assert_unrecoverable(
        &f,
        &wrong_version,
        "backup_manifest_schema_version_disagrees_with_ledger",
    )
    .await?;

    let not_a_backup = f.files.0.join("not-a-backup");
    std::fs::create_dir(&not_a_backup)?;
    std::fs::copy(
        f.backup.join("database.db"),
        not_a_backup.join("database.db"),
    )?;
    std::fs::copy(
        f.backup.join("manifest.json"),
        not_a_backup.join("manifest.json"),
    )?;
    reseal(&not_a_backup, |m| m["purpose"] = "export".into());
    assert_unrecoverable(
        &f,
        &not_a_backup,
        "backup_manifest_not_a_pre_migration_backup",
    )
    .await?;

    // The backup the current runner took at version 82 is real but proves nothing.
    assert_unrecoverable(
        &f,
        f.late_backup.as_ref().unwrap(),
        "backup_taken_after_the_policy_conversion",
    )
    .await?;

    #[cfg(unix)]
    {
        let link = f.files.0.join("link");
        std::os::unix::fs::symlink(&f.backup, &link)?;
        assert_unrecoverable(&f, &link, "backup_directory_not_a_directory").await?;
    }

    // A real, hash-matching backup from the other (main) migration lineage is not an ancestor.
    let other = f.files.0.join("other-lineage");
    std::fs::create_dir(&other)?;
    {
        let (raw, c) = seed_lineage(&other.join("database.db"), MIGRATIONS, 53).await?;
        drop(c);
        drop(raw);
    }
    std::fs::copy(f.backup.join("manifest.json"), other.join("manifest.json"))?;
    reseal(&other, |_| {});
    assert_unrecoverable(&f, &other, "backup_ledger_not_the_published_pr31_lineage").await?;
    Ok(())
}

#[tokio::test]
async fn ambiguous_missing_and_identity_disagreements_are_never_restored() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    rewrite_backup(
        &f.backup,
        "DROP TRIGGER collections_identity_update;
         UPDATE movie_collections SET tmdb_id=999 WHERE id=1;
         UPDATE movie_collections SET tmdb_id=NULL WHERE id=6;
         UPDATE movie_collections SET tmdb_id=777 WHERE id=7;
         INSERT INTO movie_collections(id,tmdb_id,title) VALUES(60,107,'Same tmdb, other id'),(50,151,'Gone');
         INSERT INTO movie_collection_settings(collection_id) VALUES(50),(60);",
    )
    .await?;
    let before_file = file_sha(&f.db);
    let before = dump(&f.db).await?;
    let report = run(&options(&f, &f.backup, true)).await?;
    assert_eq!(report.outcome, Outcome::Applied);
    assert_eq!(report.restored, 0);
    assert_eq!(
        decisions(&report),
        BTreeMap::from([
            (1, "ambiguous"),
            (2, "explicit_false_preserved"),
            (3, "explicit_true_preserved"),
            (4, "local_edit_since_backup"),
            (5, "explicit_true_preserved"),
            (6, "ambiguous"),
            (7, "ambiguous"),
            (50, "missing_in_live"),
            (60, "ambiguous"),
        ])
    );
    let reasons: BTreeMap<i64, &str> = report
        .entries
        .iter()
        .map(|e| (e.collection_id, e.reason))
        .collect();
    assert_eq!(reasons[&6], "backup_collection_has_no_tmdb_identity");
    assert_eq!(reasons[&1], "internal_id_and_tmdb_id_disagree");
    assert_eq!(file_sha(&f.db), before_file, "no guessed policy applied");
    assert_eq!(dump(&f.db).await?, before);
    Ok(())
}

#[tokio::test]
async fn apply_failure_rolls_back_completely_and_the_run_can_be_repeated() -> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    {
        let db = Database::open_local(&f.db).await?;
        let c = db.connect().await?;
        // Row 6 is now ambiguous; fail on restorable row 7 to still undo row 1's earlier update.
        c.execute(
            "CREATE TRIGGER fixture_fail BEFORE UPDATE OF search_on_add ON movie_collection_settings WHEN NEW.collection_id=7 BEGIN SELECT RAISE(ABORT,'fixture failure'); END",
            (),
        )
        .await?;
    }
    let before_file = file_sha(&f.db);
    let before = dump(&f.db).await?;
    let error = run(&options(&f, &f.backup, true))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("rolled back"), "{error}");
    assert_eq!(
        dump(&f.db).await?,
        before,
        "nothing from the transaction survives"
    );
    assert_eq!(file_sha(&f.db), before_file);
    assert_eq!(settings_row(&f.db, 1).await?.0, Some(0));
    // The failed run still left an audit record.
    let failed: Vec<serde_json::Value> = std::fs::read_dir(&f.files.0)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("hrrdarr-collection-policy-recovery-apply")
        })
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["outcome"], "failed");
    assert_eq!(failed[0]["error_class"], "apply_failed_rolled_back");
    assert_eq!(failed[0]["applied"], false);

    {
        let db = Database::open_local(&f.db).await?;
        db.connect()
            .await?
            .execute("DROP TRIGGER fixture_fail", ())
            .await?;
    }
    let report = run(&options(&f, &f.backup, true)).await?;
    assert_eq!(report.restored, 2);
    Ok(())
}

#[tokio::test]
async fn refuses_a_running_service_a_pre_adapter_database_and_the_wrong_lineage()
-> Result<(), Error> {
    let f = fixture(Upgrade::ThroughPublished).await?;
    {
        let running = Database::open_local(&f.db).await?;
        let error = run(&options(&f, &f.backup, false))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already owned"), "{error}");
        drop(running);
    }
    // Still at version 82: the settings authority does not exist yet.
    let early = fixture(Upgrade::PublishedOnly).await?;
    let before = file_sha(&early.db);
    let error = run(&options(&early, &early.backup, true))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("schema version 82"), "{error}");
    assert_eq!(file_sha(&early.db), before);

    // A main-lineage database never ran the conversion.
    let files = Scratch::new();
    let path = files.0.join("db");
    let (raw, c) = seed_lineage(&path, MIGRATIONS, 78).await?;
    drop(c);
    drop(raw);
    let main_lineage = Options {
        database: path.clone(),
        backup: f.backup.clone(),
        apply: false,
    };
    let error = run(&main_lineage).await.unwrap_err().to_string();
    assert!(error.contains("main migration lineage"), "{error}");
    // A missing database is not created.
    let missing = Options {
        database: files.0.join("absent.db"),
        backup: f.backup.clone(),
        apply: false,
    };
    assert!(run(&missing).await.is_err());
    assert!(!files.0.join("absent.db").exists());
    Ok(())
}

fn facts(tmdb: Option<i64>, settings: Option<PolicyFacts>) -> CollectionFacts {
    CollectionFacts {
        tmdb_id: tmdb,
        settings,
    }
}

fn policy(soa: Option<i64>) -> PolicyFacts {
    PolicyFacts {
        settings_revision: 4,
        monitored: 0,
        root_folder_id: None,
        quality_profile_id: None,
        minimum_availability: None,
        search_on_add: soa,
        local_edit: 0,
        snapshot_owned: 0,
        tags: BTreeSet::new(),
    }
}

#[test]
fn planning_is_pure_and_never_turns_every_false_into_absent() {
    let backup = Snapshot {
        collections: BTreeMap::from([
            (1, facts(Some(10), Some(policy(None)))),
            (2, facts(Some(20), Some(policy(Some(0))))),
        ]),
    };
    let live = Snapshot {
        collections: BTreeMap::from([
            (1, facts(Some(10), Some(policy(Some(0))))),
            (2, facts(Some(20), Some(policy(Some(0))))),
            (3, facts(Some(30), Some(policy(Some(0))))),
        ]),
    };
    let first = plan(&backup, &live);
    assert_eq!(first, plan(&backup, &live));
    let by_id: BTreeMap<i64, Decision> = first
        .iter()
        .map(|e| (e.collection_id, e.decision))
        .collect();
    assert_eq!(
        by_id,
        BTreeMap::from([
            (1, Decision::Restorable),
            (2, Decision::ExplicitFalsePreserved),
            (3, Decision::UnrecoverableNoBackupProvenance),
        ])
    );

    // Each compared field independently blocks a restore.
    let mutations: [(&str, fn(&mut PolicyFacts)); 8] = [
        ("settings_revision_differs", |p| p.settings_revision += 1),
        ("monitored_differs", |p| p.monitored = 1),
        ("root_folder_differs", |p| p.root_folder_id = Some(1)),
        ("quality_profile_differs", |p| {
            p.quality_profile_id = Some(1)
        }),
        ("minimum_availability_differs", |p| {
            p.minimum_availability = Some("released".into())
        }),
        ("local_edit_differs", |p| p.local_edit = 1),
        ("snapshot_owned_differs", |p| p.snapshot_owned = 1),
        ("tags_differ", |p| {
            p.tags.insert(9);
        }),
    ];
    for (reason, mutate) in mutations {
        let mut changed = policy(Some(0));
        mutate(&mut changed);
        let live = Snapshot {
            collections: BTreeMap::from([(1, facts(Some(10), Some(changed)))]),
        };
        let planned = plan(&backup, &live);
        let one = planned.iter().find(|e| e.collection_id == 1).unwrap();
        assert_eq!(one.decision, Decision::LocalEditSinceBackup, "{reason}");
        assert_eq!(one.reason, reason);
    }
    // A user who set search-on-add true since the backup is a local edit, not a restore target.
    let live = Snapshot {
        collections: BTreeMap::from([(1, facts(Some(10), Some(policy(Some(1)))))]),
    };
    assert_eq!(
        plan(&backup, &live)[0].decision,
        Decision::LocalEditSinceBackup
    );
}

#[test]
fn command_line_parsing_is_strict() {
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        parse_args(&args(&["--backup", "b"])),
        Ok((PathBuf::from("b"), false))
    );
    assert_eq!(
        parse_args(&args(&["--apply", "--backup=b"])),
        Ok((PathBuf::from("b"), true))
    );
    for bad in [
        args(&[]),
        args(&["--apply"]),
        args(&["--backup"]),
        args(&["--backup="]),
        args(&["--backup", "a", "--backup", "b"]),
        args(&["--backup", "a", "--apply", "--apply"]),
        args(&["--backup", "a", "--force"]),
    ] {
        assert!(parse_args(&bad).is_err(), "{bad:?}");
    }
}
