//! Conversion of the unpublished PR #31 draft database (issue #44). The draft database is rebuilt
//! here from the byte-identical migration files of draft head `1431d97`
//! (`tests/fixtures/draft-ledger-1431d97`), populated, converted, and read back through the same
//! code startup uses.
use super::draft_conversion::*;
use super::published_prefix_tests::Scratch;
use super::*;

/// Draft ledger rows 50 through 76, in ledger order.
const DRAFT_SQL: [&str; 27] = [
    include_str!("../../tests/fixtures/draft-ledger-1431d97/50_indexer_backoff.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/51_download_client_backoff.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/52_download_client_selection.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/53_path_diagnostics.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/54_downloaded_scan.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/55_series_runtime.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/56_import_policy.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/57_scan_media.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/58_scan_successor.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/59_download_client_registry.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/60_recycle_bin.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/61_upgrade_recycle.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/62_transmission.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/63_notifications.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/64_nzbget.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/65_rename_commands.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/66_autotagging_rules.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/67_collections.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/68_torrent_rss_indexer.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/69_collection_snapshot.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/70_deluge.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/71_rtorrent.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/72_utorrent.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/73_aria2.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/74_flood.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/75_downloadstation.sql"),
    include_str!("../../tests/fixtures/draft-ledger-1431d97/76_import_lists.sql"),
];

const POPULATE: &str = "
    INSERT INTO root_folders(id,media_type,path) VALUES(1,'movies','/movies'),(2,'tv','/tv');
    INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'movies','Any'),(2,'tv','Any');
    INSERT INTO tags(id,media_type,label) VALUES(1,'movies','kept'),(2,'movies','also'),(3,'tv','shows');
    INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv/one');
    INSERT INTO seasons VALUES(1,1,1);
    INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One');
    INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,11,'One',2001),(2,12,'Two',2002),(3,13,'Three',2003);
    INSERT INTO movie_collections(id,tmdb_id,title,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability,metadata_revision,local_edit,snapshot_owned) VALUES
        (1,101,'Plain',0,0,NULL,NULL,NULL,0,0,0),
        (2,102,'Local policy',1,1,1,1,'released',3,1,0),
        (3,103,'Snapshot policy',0,1,NULL,NULL,NULL,2,0,1),
        (4,104,'Unknown provenance',0,1,NULL,NULL,NULL,0,0,0),
        (5,105,'Tagged only',0,0,NULL,NULL,NULL,5,0,0),
        (6,106,'Monitored without a root',1,0,NULL,NULL,NULL,0,1,0);
    INSERT INTO movie_collection_members(collection_id,metadata_id,origin) VALUES (2,1,'snapshot'),(2,2,'graph'),(5,3,'legacy');
    INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES (5,1),(2,2);
    INSERT INTO notifications(id,name,implementation,settings_json,created_at,updated_at) VALUES
        ('22222222-2222-4222-8222-222222222222','Hook tv','webhook','{}',1,1),('33333333-3333-4333-8333-333333333333','Hook movies','webhook','{}',1,1);
    INSERT INTO notification_scopes(notification_id,media_type,on_grab) VALUES ('22222222-2222-4222-8222-222222222222','tv',1),('33333333-3333-4333-8333-333333333333','movies',1);
    INSERT INTO notification_events(sequence,kind,scope,subject_id,payload_json,created_at) VALUES
        (1,'grab','movies',1,'{}',1),(2,'grab','tv',1,'{}',2);
    INSERT INTO notification_deliveries(id,event_sequence,notification_id,state,attempts,next_attempt_at,created_at) VALUES
        (1,1,'33333333-3333-4333-8333-333333333333','pending',0,5,1),(2,2,'22222222-2222-4222-8222-222222222222','pending',2,9,2);
    INSERT INTO snapshot_imports(application,fingerprint,schema_version,collection_version) VALUES ('radarr','abc',1,1);
    INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials) VALUES
        ('11111111-1111-4111-8111-111111111111','flood','Torrents',1,1,1,1,'http://127.0.0.1:1/',CAST('SECRET!SECRET!SECRET!SECRET!AB' AS BLOB));
    INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES
        ('11111111-1111-4111-8111-111111111111','flood','tv','tv',0,0,'started');
    INSERT INTO download_client_tags VALUES ('11111111-1111-4111-8111-111111111111','tv',3);
";

/// A draft database at the draft head, with the ledger exactly as the draft build wrote it.
pub(super) async fn draft_database(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    let ledger = MIGRATIONS
        .iter()
        .take(49)
        .map(|&(name, sql)| (name, sql))
        .chain(
            DRAFT_TAIL_NAMES
                .iter()
                .zip(DRAFT_SQL)
                .map(|(&name, sql)| (name, sql)),
        );
    for (i, (name, sql)) in ledger.enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, name, checksum(sql), sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok((raw, c))
}

const DRAFT_TAIL_NAMES: [&str; 27] = [
    "indexer_backoff",
    "download_client_backoff",
    "download_client_selection",
    "path_diagnostics",
    "downloaded_scan",
    "series_runtime",
    "import_policy",
    "scan_media",
    "scan_successor",
    "download_client_registry",
    "recycle_bin",
    "upgrade_recycle",
    "transmission",
    "notifications",
    "nzbget",
    "rename_commands",
    "autotagging_rules",
    "collections",
    "torrent_rss_indexer",
    "collection_snapshot",
    "deluge",
    "rtorrent",
    "utorrent",
    "aria2",
    "flood",
    "downloadstation",
    "import_lists",
];

#[tokio::test]
async fn draft_schema_fingerprint_matches_the_fixture_build() -> Result<(), Error> {
    let files = Scratch::new();
    let (_raw, c) = draft_database(&files.0.join("draft.db")).await?;
    let digest = schema_digest(&c).await?;
    assert_eq!(
        digest, DRAFT_SCHEMA_SHA256,
        "update DRAFT_SCHEMA_SHA256 to {digest}"
    );
    for (name, sql) in DRAFT_TAIL_NAMES.iter().zip(DRAFT_SQL) {
        assert!(
            DRAFT_TAIL
                .iter()
                .any(|&(n, h)| n == *name && h == checksum(sql)),
            "{name} fixture checksum is not in DRAFT_TAIL"
        );
    }
    Ok(())
}

/// The main prefix the converted database must carry is pinned: if a published migration changes,
/// this fails and the conversion must be reviewed.
#[test]
fn conversion_target_is_the_pinned_main_prefix() {
    assert!(MIGRATIONS.len() >= TARGET_VERSION);
    assert_eq!(MIGRATIONS[TARGET_VERSION - 1].0, "import_lists");
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for &(name, sql) in MIGRATIONS.iter().take(TARGET_VERSION) {
        digest.update(name.as_bytes());
        digest.update(checksum(sql).as_bytes());
    }
    assert_eq!(hex(digest.finish().as_ref()), TARGET_PREFIX_SHA256);
    assert_eq!(DRAFT_VERSION, 49 + DRAFT_SQL.len());
}

const TARGET_PREFIX_SHA256: &str =
    "e3fe479949469505577e27fd9942373b7f314b5b16f74ce8abee2d9b46e651ee";

async fn populated_draft(files: &Scratch) -> Result<PathBuf, Error> {
    let path = files.0.join("draft.db");
    let (raw, c) = draft_database(&path).await?;
    c.execute_batch(POPULATE).await?;
    drop(c);
    drop(raw);
    Ok(path)
}

fn file_digest(path: &Path) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(&std::fs::read(path).unwrap());
    hex(digest.finish().as_ref())
}

async fn open_plain(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok((raw, c))
}

fn leftovers(directory: &Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".hrrdarr-convert-"))
        .collect()
}

#[tokio::test]
async fn converts_a_populated_draft_database_and_leaves_the_source_untouched() -> Result<(), Error>
{
    let files = Scratch::new();
    let draft = populated_draft(&files).await?;
    let before = file_digest(&draft);
    let out = files.0.join("converted.db");
    let report = run(&Options {
        from: draft.clone(),
        to: Some(out.clone()),
    })
    .await?;
    assert_eq!(report.outcome, Outcome::Converted);
    assert_eq!(
        file_digest(&draft),
        before,
        "the source must be byte-identical"
    );
    assert!(
        leftovers(&files.0).is_empty(),
        "scratch space must be removed"
    );

    let (_raw, c) = open_plain(&out).await?;
    // The application's own reader accepts the pinned conversion target as the main lineage.
    let (version, history) = migration_history(&c).await?;
    assert_eq!(version, TARGET_VERSION);
    assert_eq!(history, MIGRATIONS);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await?,
        TARGET_VERSION as i64
    );
    integrity(&c).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM pragma_foreign_key_check").await?,
        0
    );
    // Reference schema: exactly what the real migrations produce.
    let reference = files.0.join("reference.db");
    let (_r, rc) = open_plain(&reference).await?;
    rc.execute("PRAGMA foreign_keys=OFF", ()).await?;
    rc.execute(HISTORY_SQL, ()).await?;
    for &(_, sql) in MIGRATIONS.iter().take(TARGET_VERSION) {
        rc.execute_batch(sql).await?;
    }
    assert_eq!(schema_digest(&c).await?, schema_digest(&rc).await?);

    // Catalog and library data.
    assert_eq!(scalar(&c, "SELECT count(*) FROM series").await?, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM episodes").await?, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM movie_metadata").await?, 3);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await?,
        6
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_members").await?,
        3
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_tags").await?,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_client_tags").await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM provider_scope_tags").await?,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM provider_status").await?, 0);
    assert_eq!(
        scalar(&c, "SELECT length(credentials) FROM providers WHERE id='11111111-1111-4111-8111-111111111111'").await?,
        30,
        "stored credentials are carried over verbatim"
    );

    // Collection policy: inline columns are retired, the policy lives in settings (migration 0077).
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections WHERE monitored!=0 OR search_on_add!=0 OR root_folder_id IS NOT NULL OR quality_profile_id IS NOT NULL OR minimum_availability IS NOT NULL").await?,
        0
    );
    let settings = |sql: &'static str| {
        let c = c.clone();
        async move { scalar(&c, sql).await }
    };
    // Collection 1 is plain: no settings row at all.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=1").await?,
        0
    );
    // Collection 2: local policy kept, monitoring intact, local edit stays 1.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=2 AND monitored=1 AND root_folder_id=1 AND quality_profile_id=1 AND minimum_availability='released' AND search_on_add=1 AND local_edit=1 AND snapshot_owned=0 AND settings_revision=1").await?,
        1
    );
    // Collection 3: snapshot-owned stays replaceable by a newer backup.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=3 AND search_on_add=1 AND local_edit=0 AND snapshot_owned=1").await?,
        1
    );
    // Collection 4: unknown provenance is protected like 0077 does.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=4 AND search_on_add=1 AND local_edit=1 AND snapshot_owned=0").await?,
        1
    );
    // Collection 5: only its tag asks for a settings row.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=5 AND monitored=0 AND search_on_add=0").await?,
        1
    );
    // Collection 6: monitored without a root cannot stay monitored.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings WHERE collection_id=6 AND monitored=0 AND local_edit=1").await?,
        1
    );
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_settings").await?,
        5
    );
    // Members carry their collection's metadata revision, as the main triggers require.
    assert_eq!(
        settings("SELECT count(*) FROM movie_collection_members m JOIN movie_collections c ON c.id=m.collection_id WHERE m.metadata_revision=c.metadata_revision AND m.origin IN('snapshot','graph','legacy')").await?,
        3
    );
    assert_eq!(
        settings("SELECT metadata_revision FROM movie_collection_members WHERE collection_id=5")
            .await?,
        5
    );
    assert_eq!(
        settings("SELECT graph_complete FROM movie_collections WHERE id=2").await?,
        0
    );

    // Notification deliveries gain the delivery scope (migration 0083).
    assert_eq!(settings("SELECT count(*) FROM notification_deliveries WHERE id=1 AND media_type='movies' AND sent_parts='[]' AND lease_owner IS NULL AND next_attempt_at=5").await?, 1);
    assert_eq!(settings("SELECT count(*) FROM notification_deliveries WHERE id=2 AND media_type='tv' AND attempts=2").await?, 1);
    // Snapshot import bookkeeping keeps its versions; the new authority marker starts at 0.
    assert_eq!(settings("SELECT count(*) FROM snapshot_imports WHERE application='radarr' AND collection_version=1 AND provider_authority_version=0").await?, 1);

    // The converted database works under the restored triggers.
    c.execute("UPDATE movie_collection_settings SET search_on_add=0, settings_revision=settings_revision+1 WHERE collection_id=2", ()).await?;
    assert!(
        c.execute(
            "UPDATE movie_collection_settings SET search_on_add=1 WHERE collection_id=2",
            ()
        )
        .await
        .is_err(),
        "the revision trigger must be live"
    );

    // Conversion stays pinned; startup applies any newer migrations and backs up populated data.
    drop(c);
    let db = Database::open_local(&out).await?;
    assert_eq!(db.backup.is_some(), MIGRATIONS.len() > TARGET_VERSION);
    let startup_conn = db.connect().await?;
    assert_eq!(migration_history(&startup_conn).await?.0, MIGRATIONS.len());
    drop(startup_conn);
    drop(db);
    assert!(leftovers(&files.0).is_empty());

    // The report holds counts and decisions, never row values or secrets.
    let text = serde_json::to_string(&report.json)?;
    for secret in ["SECRET!", "/movies", "Plain", "Hook"] {
        assert!(!text.contains(secret), "report leaked {secret}");
    }
    assert_eq!(report.json["published"], true);
    assert_eq!(report.json["source"]["modified"], false);
    assert_eq!(report.json["collection_settings_rows_created"], 5);
    assert_eq!(
        report.json["tables"]["movie_collections"]["converted_rows"],
        6
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&out)?.permissions().mode() & 0o077, 0);
        assert_eq!(
            std::fs::metadata(&report.report_path)?.permissions().mode() & 0o077,
            0
        );
    }
    Ok(())
}

/// The conversion replaces the target's seeded default rows with the draft's. That is only exact
/// while a fresh draft and a fresh target seed the same defaults.
#[tokio::test]
async fn seed_rows_match_between_draft_and_target() -> Result<(), Error> {
    let files = Scratch::new();
    let (_raw, c) = draft_database(&files.0.join("draft.db")).await?;
    let reference = files.0.join("reference.db");
    {
        let (_r, rc) = open_plain(&reference).await?;
        rc.execute("PRAGMA foreign_keys=OFF", ()).await?;
        rc.execute(HISTORY_SQL, ()).await?;
        for &(_, sql) in MIGRATIONS.iter().take(TARGET_VERSION) {
            rc.execute_batch(sql).await?;
        }
    }
    c.execute(
        "ATTACH DATABASE ?1 AS target",
        params![reference.to_str().unwrap()],
    )
    .await?;
    let draft_tables = inventory(&c, "main").await?;
    let target_tables = inventory(&c, "target").await?;
    let mut seeded = 0;
    for (name, draft) in &draft_tables {
        let Some(target) = target_tables.get(name) else {
            continue;
        };
        if name == "schema_migrations" {
            continue;
        }
        let common: Vec<String> = draft
            .columns
            .iter()
            .filter(|d| target.columns.iter().any(|t| t.name == d.name))
            .map(|d| format!("\"{}\"", d.name))
            .collect();
        let list = common.join(",");
        let table = format!("\"{name}\"");
        let left = scalar(&c, &format!("SELECT count(*) FROM (SELECT {list} FROM main.{table} EXCEPT SELECT {list} FROM target.{table})")).await?;
        let right = scalar(&c, &format!("SELECT count(*) FROM (SELECT {list} FROM target.{table} EXCEPT SELECT {list} FROM main.{table})")).await?;
        assert_eq!((left, right), (0, 0), "seed rows differ in {name}");
        seeded += scalar(&c, &format!("SELECT count(*) FROM target.{table}")).await?;
    }
    assert!(seeded > 0, "the guard must actually see seeded tables");
    Ok(())
}

fn reports_in(directory: &Path, mode: &str) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .starts_with(&format!("hrrdarr-draft-conversion-{mode}-"))
            })
        })
        .collect()
}

#[tokio::test]
async fn dry_run_writes_only_the_report() -> Result<(), Error> {
    let files = Scratch::new();
    let draft = populated_draft(&files).await?;
    let before = file_digest(&draft);
    let report = run(&Options {
        from: draft.clone(),
        to: None,
    })
    .await?;
    assert_eq!(report.outcome, Outcome::DryRun);
    assert_eq!(report.json["published"], false);
    assert_eq!(report.json["collection_settings_rows_created"], 5);
    assert_eq!(file_digest(&draft), before);
    assert!(leftovers(&files.0).is_empty());
    assert_eq!(reports_in(&files.0, "dry-run").len(), 1);
    let names: std::collections::BTreeSet<String> = std::fs::read_dir(&files.0)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "only the source and the report remain: {names:?}"
    );
    Ok(())
}

async fn expect_rejection(files: &Scratch, draft: &Path, expected: Rejection) -> Result<(), Error> {
    let before = file_digest(draft);
    let out = files.0.join(format!("never-{}.db", uuid::Uuid::new_v4()));
    let report = run(&Options {
        from: draft.to_path_buf(),
        to: Some(out.clone()),
    })
    .await?;
    assert_eq!(
        report.outcome,
        Outcome::NotConvertible(expected),
        "{}",
        report.json
    );
    assert_eq!(report.json["published"], false);
    assert_eq!(report.json["outcome"], "not_convertible");
    assert!(
        !out.exists(),
        "nothing may be published for a rejected source"
    );
    assert_eq!(file_digest(draft), before);
    assert!(leftovers(&files.0).is_empty());
    Ok(())
}

#[tokio::test]
async fn only_the_complete_unmodified_draft_database_is_converted() -> Result<(), Error> {
    // Already supported: a database startup accepts.
    let files = Scratch::new();
    let supported = files.0.join("supported.db");
    drop(Database::open_local(&supported).await?);
    expect_rejection(&files, &supported, Rejection::AlreadySupported).await?;

    // No migration history at all.
    let bare = files.0.join("bare.db");
    {
        let (_raw, c) = open_plain(&bare).await?;
        c.execute("CREATE TABLE unrelated(id INTEGER)", ()).await?;
    }
    expect_rejection(&files, &bare, Rejection::NoMigrationHistory).await?;

    // An incomplete draft ledger (the draft build never finished migrating).
    let partial = files.0.join("partial.db");
    {
        let (_raw, c) = draft_database(&partial).await?;
        c.execute("DELETE FROM schema_migrations WHERE version > 66", ())
            .await?;
    }
    expect_rejection(&files, &partial, Rejection::DraftLedgerIncomplete).await?;

    // Draft rows 50 through 62 are byte-identical to main's, so a draft database that stopped
    // there is an ordinary main-lineage database and startup upgrades it. Nothing to convert.
    let early = files.0.join("early.db");
    {
        let (_raw, c) = draft_database(&early).await?;
        c.execute("DELETE FROM schema_migrations WHERE version > 62", ())
            .await?;
    }
    expect_rejection(&files, &early, Rejection::AlreadySupported).await?;

    // A draft ledger row whose SQL was altered.
    let tampered = files.0.join("tampered.db");
    {
        let (_raw, c) = draft_database(&tampered).await?;
        c.execute(
            "UPDATE schema_migrations SET sql = sql || ' ' WHERE version = 70",
            (),
        )
        .await?;
    }
    expect_rejection(&files, &tampered, Rejection::NotTheDraftLedger).await?;

    // A longer history than the draft head wrote.
    let longer = files.0.join("longer.db");
    {
        let (_raw, c) = draft_database(&longer).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(77,'later','x','y')",
            (),
        )
        .await?;
    }
    expect_rejection(&files, &longer, Rejection::NotTheDraftLedger).await?;

    // The exact ledger over a schema that was changed by hand.
    let edited = files.0.join("edited.db");
    {
        let (_raw, c) = draft_database(&edited).await?;
        c.execute("CREATE TABLE hand_made(id INTEGER)", ()).await?;
    }
    expect_rejection(&files, &edited, Rejection::DraftSchemaDiffers).await?;
    Ok(())
}

#[tokio::test]
async fn existing_or_identical_output_paths_are_refused() -> Result<(), Error> {
    let files = Scratch::new();
    let draft = populated_draft(&files).await?;
    let before = file_digest(&draft);
    let existing = files.0.join("existing.db");
    std::fs::write(&existing, b"keep me")?;
    for to in [existing.clone(), draft.clone()] {
        let error = run(&Options {
            from: draft.clone(),
            to: Some(to),
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
    }
    assert_eq!(std::fs::read(&existing)?, b"keep me");
    assert_eq!(file_digest(&draft), before);
    let error = run(&Options {
        from: files.0.join("missing.db"),
        to: None,
    })
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("cannot resolve the source"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn output_sidecars_are_refused_before_conversion_and_at_publication() -> Result<(), Error> {
    let files = Scratch::new();
    let source = files.0.join("source.db");
    // Preflight must reject the occupied basename before even attempting to read SQLite data.
    std::fs::write(&source, b"source untouched")?;
    let output = files.0.join("output.db");
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = files.0.join(format!("output.db{suffix}"));
        std::fs::write(&sidecar, b"sidecar untouched")?;
        let error = run(&Options {
            from: source.clone(),
            to: Some(output.clone()),
        })
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("sidecar already exists"),
            "{error}"
        );
        assert_eq!(
            publish(&source, &output).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&sidecar)?, b"sidecar untouched");
        assert_eq!(std::fs::read(&source)?, b"source untouched");
        std::fs::remove_file(sidecar)?;
    }
    assert!(leftovers(&files.0).is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn output_sidecar_symlinks_are_refused_without_touching_the_link_or_target() {
    let files = Scratch::new();
    let source = files.0.join("source.db");
    let target = files.0.join("sidecar-target");
    let output = files.0.join("output.db");
    std::fs::write(&source, b"converted").unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = files.0.join(format!("output.db{suffix}"));
        std::os::unix::fs::symlink(&target, &sidecar).unwrap();
        for target_exists in [true, false] {
            if target_exists {
                std::fs::write(&target, b"keep me").unwrap();
            } else {
                std::fs::remove_file(&target).unwrap();
            }
            assert_eq!(
                publish(&source, &output).unwrap_err().kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert!(!output.exists());
            assert_eq!(std::fs::read_link(&sidecar).unwrap(), target);
            if target_exists {
                assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
            } else {
                assert!(!target.exists());
            }
        }
        std::fs::remove_file(sidecar).unwrap();
    }
}

#[tokio::test]
async fn a_database_held_by_another_process_is_refused() -> Result<(), Error> {
    let files = Scratch::new();
    let draft = populated_draft(&files).await?;
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&draft)?;
    holder.try_lock().unwrap();
    let error = run(&Options {
        from: draft.clone(),
        to: None,
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("stop hrrdarr first"), "{error}");
    drop(holder);
    assert!(leftovers(&files.0).is_empty());
    Ok(())
}

async fn refused_with(files: &Scratch, statements: &str, message: &str) -> Result<(), Error> {
    let draft = populated_draft(files).await?;
    {
        let (_raw, c) = open_plain(&draft).await?;
        c.execute_batch(statements).await?;
    }
    let before = file_digest(&draft);
    let out = files.0.join(format!("refused-{}.db", uuid::Uuid::new_v4()));
    let error = run(&Options {
        from: draft.clone(),
        to: Some(out.clone()),
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains(message), "{error}");
    assert!(!out.exists());
    assert_eq!(file_digest(&draft), before);
    assert!(leftovers(&files.0).is_empty());
    let reports = reports_in(&files.0, "convert");
    let failed = reports
        .iter()
        .any(|p| std::fs::read_to_string(p).is_ok_and(|t| t.contains("\"outcome\": \"failed\"")));
    assert!(failed, "a failed run still leaves its audit report");
    Ok(())
}

#[tokio::test]
async fn data_main_would_reject_is_refused_instead_of_published() -> Result<(), Error> {
    // A monitored collection whose root folder is a TV root: the draft only guarded updates.
    let files = Scratch::new();
    refused_with(
        &files,
        "INSERT INTO movie_collections(id,tmdb_id,title,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability,local_edit) VALUES(7,107,'Wrong root',1,0,1,2,'released',1);",
        "root folder outside the movies domain",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_directory_client_category_main_rejects_is_refused() -> Result<(), Error> {
    let files = Scratch::new();
    refused_with(
        &files,
        "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('44444444-4444-4444-8444-444444444444','aria2','Aria',1,1,1,1,'http://127.0.0.1:1/');
         INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES('44444444-4444-4444-8444-444444444444','aria2','tv','..',0,0,'started');",
        "category is '.' or '..'",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn an_uncheckpointed_write_ahead_log_is_included_and_never_modified() -> Result<(), Error> {
    let files = Scratch::new();
    let live = files.0.join("live");
    std::fs::create_dir(&live)?;
    let live_db = live.join("draft.db");
    let (raw, c) = draft_database(&live_db).await?;
    c.query("PRAGMA journal_mode=WAL", ()).await?;
    c.execute_batch(POPULATE).await?;
    // Snapshot the files while the writer is still open: this is the state a crash leaves behind.
    let crashed = files.0.join("crashed");
    std::fs::create_dir(&crashed)?;
    let db = crashed.join("draft.db");
    std::fs::copy(&live_db, &db)?;
    let wal = live.join("draft.db-wal");
    assert!(
        wal.exists() && std::fs::metadata(&wal)?.len() > 0,
        "the test needs a live WAL"
    );
    std::fs::copy(&wal, crashed.join("draft.db-wal"))?;
    drop(c);
    drop(raw);
    let wal_before = std::fs::read(crashed.join("draft.db-wal"))?;
    let before = file_digest(&db);
    let out = crashed.join("converted.db");
    let report = run(&Options {
        from: db.clone(),
        to: Some(out.clone()),
    })
    .await?;
    assert_eq!(report.outcome, Outcome::Converted, "{}", report.json);
    assert_eq!(file_digest(&db), before);
    assert_eq!(std::fs::read(crashed.join("draft.db-wal"))?, wal_before);
    let (_raw, c) = open_plain(&out).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collections").await?,
        6
    );
    Ok(())
}

#[test]
fn command_line_arguments_are_strict() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let ok = parse_args(&args(&["--from", "a.db", "--to", "b.db"])).unwrap();
    assert_eq!(
        (ok.from, ok.to),
        (PathBuf::from("a.db"), Some(PathBuf::from("b.db")))
    );
    let ok = parse_args(&args(&["--from=a.db"])).unwrap();
    assert_eq!((ok.from, ok.to), (PathBuf::from("a.db"), None));
    for bad in [
        &[][..],
        &["--to", "b.db"],
        &["--from"],
        &["--from", ""],
        &["--from", "a", "--from", "b"],
        &["--from", "a", "--to", "b", "--to", "c"],
        &["--from", "a", "--apply"],
        &["--from", "a", "stray"],
    ] {
        assert!(parse_args(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn publishing_never_replaces_an_existing_file() {
    let files = Scratch::new();
    let from = files.0.join("from");
    std::fs::write(&from, b"converted").unwrap();
    let taken = files.0.join("taken");
    std::fs::write(&taken, b"keep me").unwrap();
    let error = publish(&from, &taken).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&taken).unwrap(), b"keep me");
    let fresh = files.0.join("fresh");
    publish(&from, &fresh).unwrap();
    assert_eq!(std::fs::read(&fresh).unwrap(), b"converted");
}

#[test]
fn failed_publication_preserves_the_destination_and_creates_nothing() {
    let files = Scratch::new();
    let missing = files.0.join("missing");
    let destination = files.0.join("destination");
    std::fs::write(&destination, b"keep me").unwrap();
    assert!(publish(&missing, &destination).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"keep me");
    std::fs::remove_file(&destination).unwrap();
    assert!(publish(&missing, &destination).is_err());
    assert!(!destination.exists());
}

#[test]
fn publishing_survives_process_exit_without_cleanup() {
    const CHILD_STAGE: &str = "HRRDARR_PUBLISH_TEST_STAGE";
    const CHILD_DIRECTORY: &str = "HRRDARR_PUBLISH_TEST_DIRECTORY";
    if let Ok(stage) = std::env::var(CHILD_STAGE) {
        let directory = std::path::PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap());
        if stage == "before" {
            std::process::exit(91);
        }
        publish(&directory.join("from"), &directory.join("published")).unwrap();
        // Exit without Rust destructors: correctness must not depend on cleanup after publication.
        std::process::exit(92);
    }
    let files = Scratch::new();
    let bytes = vec![0xa5; 256 * 1024 + 17];
    std::fs::write(files.0.join("from"), &bytes).unwrap();
    for (stage, expected) in [("before", 91), ("after", 92)] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "db::draft_conversion_tests::publishing_survives_process_exit_without_cleanup",
            ])
            .env(CHILD_STAGE, stage)
            .env(CHILD_DIRECTORY, &files.0)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(expected));
        if stage == "before" {
            assert!(!files.0.join("published").exists());
        } else {
            std::fs::remove_file(files.0.join("from")).unwrap();
            assert_eq!(std::fs::read(files.0.join("published")).unwrap(), bytes);
        }
    }
}

#[test]
fn digest_streams_bounded_reads_and_propagates_read_errors() {
    use std::io::Read;
    struct BoundedReader {
        remaining: usize,
        reads: usize,
    }
    impl Read for BoundedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            assert!(buffer.len() <= 64 * 1024);
            self.reads += 1;
            let count = buffer.len().min(self.remaining);
            buffer[..count].fill(0xa5);
            self.remaining -= count;
            Ok(count)
        }
    }
    let size = 256 * 1024 + 17;
    let mut reader = BoundedReader {
        remaining: size,
        reads: 0,
    };
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    update_digest(&mut reader, &mut digest).unwrap();
    assert_eq!(reader.remaining, 0);
    assert!(reader.reads > 4);
    assert_eq!(
        digest.finish().as_ref(),
        ring::digest::digest(&ring::digest::SHA256, &vec![0xa5; size]).as_ref()
    );

    struct FailedReader;
    impl Read for FailedReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("read failed"))
        }
    }
    let error = update_digest(
        FailedReader,
        &mut ring::digest::Context::new(&ring::digest::SHA256),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
}
