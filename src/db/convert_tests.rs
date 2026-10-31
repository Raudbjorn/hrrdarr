//! Conversion of databases created by pre-merge PR #32 (#47). The pre-merge ledger is rebuilt from the
//! pinned SQL (verified below against the checksums recorded from `eb08c2b`), populated, converted
//! and compared; refusals must leave the source byte-identical and no destination behind.
use super::convert::{convert_premerge_pr32, premerge_ledger};
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-convert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(scalar(c, sql).await?)
}

async fn text(c: &Connection, sql: &str) -> Result<String, Error> {
    Ok(c.query(sql, ())
        .await?
        .next()
        .await?
        .ok_or("no row")?
        .get::<String>(0)?)
}

/// Checksums of the entries that differ from main, recorded with `git show eb08c2b:migrations/…`.
const RECORDED: [(usize, &str, &str); 10] = [
    (
        63,
        "notifications",
        "03e6f38daffbf608852ed300917a5b873b11e4183ad03609ad028bb3d648366f",
    ),
    (
        65,
        "rename_commands",
        "7c8106d4274becaa772aba0909151c5d03202608e6b8114de7865527f7d3ea87",
    ),
    (
        69,
        "collection_snapshot",
        "5a6141514f1211b247b285c3f102c37fe704a9bf8faa9b4095e7bfdaf7d8170a",
    ),
    (
        70,
        "deluge",
        "3394cda1821872fa1be679a918b6154c4cb60a97c5bc0abd0b13e52cb425b0ab",
    ),
    (
        71,
        "rtorrent",
        "f1da8c1c9f5744610d2f8939b797d79d5eb16ec854addff0a340974ec49c03df",
    ),
    (
        72,
        "utorrent",
        "1ddd53efa62f7e91f4e9261f56b6cad32984cd587b23b55d939c24f029f9b525",
    ),
    (
        73,
        "aria2",
        "b7d7078d5e42bffbd601ac8006d111431e6c3980c5cc1e357d48aececda6645f",
    ),
    (
        74,
        "flood",
        "ab3166b323a3d02c01d578d182603abd62f08f1582c302a1707811a24ad9c6ae",
    ),
    (
        75,
        "downloadstation",
        "ab4400d881f2a6f98023ca3f8e35b6182008fbb870700f5141cb934b5f8f2fce",
    ),
    (
        76,
        "indexer_breadth",
        "d3d9e59c0be9b7fcaa92591ad23c90efdea556d8401094c296a13e40b4247358",
    ),
];

/// Build what the pre-merge runner produced for its first `upto` entries (it applies each entry's
/// SQL, then records name/checksum/sql, with foreign keys suspended for table rebuilds).
async fn premerge(path: &Path, upto: usize) -> Result<libsql::Database, Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute_batch(&format!("PRAGMA foreign_keys=OFF; {HISTORY_SQL};"))
        .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    for (i, (name, sql)) in premerge_ledger().into_iter().take(upto).enumerate() {
        tx.execute_batch(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, name, checksum(sql), sql],
        )
        .await?;
    }
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(raw)
}

const NOTIFY_A: &str = "00000000-0000-4000-8000-00000000000a";
const NOTIFY_B: &str = "00000000-0000-4000-8000-00000000000b";

/// Pending deliveries use recorded event domains; system events are terminal because their
/// historical fan-out domain is unavailable. Delivery states still cover every outcome.
async fn populate_notifications(c: &Connection) -> Result<(), Error> {
    c.execute_batch(&format!(
        "INSERT INTO notifications(id,name,implementation,settings_json,created_at,updated_at) VALUES('{NOTIFY_A}','A','webhook','{{}}',1,1),('{NOTIFY_B}','B','webhook','{{}}',1,1);
         INSERT INTO notification_scopes(notification_id,media_type,on_health_issue,on_add) VALUES('{NOTIFY_A}','tv',1,0),('{NOTIFY_A}','movies',1,1),('{NOTIFY_B}','movies',0,0);
         UPDATE notification_scopes SET on_application_update=1 WHERE notification_id='{NOTIFY_B}';
         INSERT INTO notification_events(sequence,kind,scope,subject_id,payload_json,created_at) VALUES
          (1,'add','movies',1,'{{}}',1),(2,'health_issue','tv',NULL,'{{}}',2),(3,'application_update','movies',NULL,'{{}}',3),(4,'add','movies',1,'{{}}',4),
          (5,'health_issue','system',NULL,'{{}}',2),(6,'application_update','system',NULL,'{{}}',3);
         INSERT INTO notification_deliveries(event_sequence,notification_id,state,attempts,next_attempt_at,lease_until,last_error,created_at,completed_at) VALUES
          (1,'{NOTIFY_A}','pending',2,10,20,NULL,1,NULL),
          (2,'{NOTIFY_A}','pending',0,11,0,NULL,2,NULL),
          (3,'{NOTIFY_B}','pending',1,12,99,'http_500',3,NULL),
          (5,'{NOTIFY_B}','delivered',1,13,0,NULL,2,5),
          (1,'{NOTIFY_B}','failed',16,14,0,'http_404',1,6),
          (6,'{NOTIFY_A}','delivered',1,15,0,NULL,3,7);
         INSERT INTO notification_deliveries(event_sequence,notification_id,next_attempt_at,created_at) VALUES(4,'{NOTIFY_B}',1,1);
         DELETE FROM notification_deliveries WHERE id=7;"
    ))
    .await?;
    Ok(())
}

/// Library rows and collections; policy columns and ownership markers only exist from entry 69.
async fn populate_library(c: &Connection, upto: usize) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,100,'TV','/tv/one');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/one/file.mkv');
         INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'Pilot',1);
         INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,101,'Movie'),(2,102,'Catalog'),(3,103,'Third');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/one');
         INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/one/file.mkv');
         INSERT INTO quality_profiles(id,media_type,name) VALUES(50,'tv','TV profile'),(51,'movies','Movie profile');
         INSERT INTO root_folders(id,media_type,path) VALUES(50,'tv','/tv'),(51,'movies','/movies');
         INSERT INTO tags(id,media_type,label) VALUES(50,'tv','tv-tag'),(51,'movies','movie-tag');").await?;
    if upto < 67 {
        return Ok(());
    }
    struct Seed {
        id: i64,
        tmdb: Option<i64>,
        title: &'static str,
        monitored: u8,
        search: u8,
        profile: Option<i64>,
        root: Option<i64>,
        availability: Option<&'static str>,
        local: u8,
        snapshot: u8,
    }
    let seed =
        |id, tmdb, title, (monitored, search), (profile, root, availability), (local, snapshot)| {
            Seed {
                id,
                tmdb,
                title,
                monitored,
                search,
                profile,
                root,
                availability,
                local,
                snapshot,
            }
        };
    let rows = [
        seed(
            1,
            Some(201),
            "Owned",
            (1, 1),
            (Some(51), Some(51), Some("released")),
            (1, 0),
        ),
        seed(
            2,
            Some(202),
            "Snapshot",
            (0, 0),
            (Some(51), Some(51), Some("announced")),
            (0, 1),
        ),
        seed(3, Some(203), "Tag only", (0, 0), (None, None, None), (0, 0)),
        seed(
            4,
            Some(204),
            "Incomplete",
            (1, 0),
            (Some(51), None, Some("released")),
            (1, 0),
        ),
        seed(5, None, "Untouched", (0, 0), (None, None, None), (0, 0)),
    ];
    for row in rows {
        let ownership = if upto >= 69 {
            ",local_edit,snapshot_owned"
        } else {
            ""
        };
        let ownership_values = if upto >= 69 {
            format!(",{},{}", row.local, row.snapshot)
        } else {
            String::new()
        };
        c.execute(
            &format!("INSERT INTO movie_collections(id,tmdb_id,title,monitored,search_on_add,quality_profile_id,root_folder_id,minimum_availability{ownership}) VALUES(?,?,?,?,?,?,?,?{ownership_values})"),
            params![row.id, row.tmdb, row.title, row.monitored, row.search, row.profile, row.root, row.availability],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(3,51);")
        .await?;
    c.execute_batch(
        "INSERT INTO movie_collection_members(collection_id,metadata_id) VALUES(1,1),(1,2),(5,3);",
    )
    .await?;
    if upto >= 69 {
        c.execute_batch(
            "UPDATE movie_collections SET images_json='[{\"cover_type\":\"poster\",\"source_url\":\"https://img.invalid/p.jpg\"}]',sort_title='owned',added='2024-01-02T03:04:05Z',last_info_sync='2024-02-03T04:05:06Z',metadata_revision=3 WHERE id=1;
             UPDATE movie_collection_members SET origin='snapshot' WHERE collection_id=1 AND metadata_id=2;
             INSERT INTO movie_import_exclusions(tmdb_id,title,year,excluded,local_edit,revision) VALUES(102,'Excluded',2001,1,1,4);
             UPDATE snapshot_imports SET collection_version=collection_version;
             INSERT INTO snapshot_imports(application,fingerprint,schema_version,collection_version) VALUES('radarr','abc',1,1);",
        )
        .await?;
    }
    Ok(())
}

async fn populate_providers(c: &Connection, upto: usize) -> Result<(), Error> {
    for (id, implementation) in [
        ("00000000-0000-4000-8000-0000000000c1", "torrentrss"),
        ("00000000-0000-4000-8000-0000000000c2", "downloadstation"),
    ] {
        if implementation == "downloadstation" && upto < 75 {
            continue;
        }
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1/')", params![id, implementation, implementation]).await?;
    }
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,minimum_seeders,enable_automatic_search,enable_interactive_search) VALUES('00000000-0000-4000-8000-0000000000c1','torrentrss','tv',3,0,0)", ()).await?;
    if upto >= 75 {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES('00000000-0000-4000-8000-0000000000c2','downloadstation','tv','tv',0,0,'started')", ()).await?;
    }
    Ok(())
}

async fn file_digest(path: &Path) -> Result<String, Error> {
    Ok(hex(ring::digest::digest(
        &ring::digest::SHA256,
        &std::fs::read(path)?,
    )
    .as_ref()))
}

fn assert_clean(scratch: &Scratch, keep: &[&str]) {
    let mut names: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected: Vec<_> = keep.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(
        names, expected,
        "no work directory or partial destination may remain"
    );
}

#[test]
fn pinned_ledger_matches_the_recorded_pre_merge_checksums() {
    let ledger = premerge_ledger();
    assert_eq!(ledger.len(), 76);
    let divergent: Vec<usize> = ledger
        .iter()
        .enumerate()
        .filter(|(i, entry)| MIGRATIONS[*i] != **entry)
        .map(|(i, _)| i + 1)
        .collect();
    // Everything else is main's entry at the same ordinal, byte for byte.
    assert_eq!(divergent, RECORDED.map(|(n, _, _)| n));
    for (ordinal, name, expected) in RECORDED {
        assert_eq!(ledger[ordinal - 1].0, name);
        assert_eq!(checksum(ledger[ordinal - 1].1), expected, "entry {ordinal}");
    }
}

#[tokio::test]
async fn startup_still_refuses_the_pre_merge_ledger() -> Result<(), Error> {
    let s = Scratch::new();
    drop(premerge(&s.file("old.db"), 76).await?);
    let error = Database::open_local(s.file("old.db"))
        .await
        .err()
        .ok_or("must refuse")?;
    assert!(
        error
            .to_string()
            .contains("migration history mismatch at version 63"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn full_history_converts_populated_state_without_touching_the_source() -> Result<(), Error> {
    let s = Scratch::new();
    let raw = premerge(&s.file("old.db"), 76).await?;
    let c = raw.connect()?;
    populate_library(&c, 76).await?;
    populate_providers(&c, 76).await?;
    populate_notifications(&c).await?;
    assert_eq!(
        count(
            &c,
            "SELECT seq FROM sqlite_sequence WHERE name='notification_deliveries'"
        )
        .await?,
        7
    );
    drop(c);
    drop(raw);
    let before = file_digest(&s.file("old.db")).await?;

    let report = convert_premerge_pr32(&s.file("old.db"), &s.file("new.db")).await?;
    assert_eq!(
        file_digest(&s.file("old.db")).await?,
        before,
        "the source is never modified"
    );
    assert_clean(&s, &["old.db", "new.db"]);
    assert_eq!(
        (report.source_version, report.destination_version),
        (76, MIGRATIONS.len())
    );
    assert_eq!(report.collections_unmonitored, 1);
    assert_eq!(report.pending_deliveries, 3);

    let db = Database::open_local(s.file("new.db")).await?;
    assert!(
        db.migration_backup().is_none(),
        "a converted database is already current"
    );
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, MIGRATIONS.len());
    integrity(&c).await?;

    // Collections: catalog facts move as-is; policy now lives in settings; markers are carried.
    assert_eq!(count(&c, "SELECT count(*) FROM movie_collections WHERE monitored!=0 OR search_on_add!=0 OR root_folder_id IS NOT NULL OR quality_profile_id IS NOT NULL OR minimum_availability IS NOT NULL").await?, 0);
    let setting = |id: i64, column: &'static str| {
        format!("SELECT {column} FROM movie_collection_settings WHERE collection_id={id}")
    };
    assert_eq!(count(&c, &setting(1, "monitored")).await?, 1);
    assert_eq!(
        text(&c, &setting(1, "minimum_availability")).await?,
        "released"
    );
    assert_eq!(count(&c, &setting(1, "root_folder_id")).await?, 51);
    assert_eq!(count(&c, &setting(1, "quality_profile_id")).await?, 51);
    assert_eq!(
        (
            count(&c, &setting(1, "local_edit")).await?,
            count(&c, &setting(1, "snapshot_owned")).await?
        ),
        (1, 0)
    );
    assert_eq!(
        (
            count(&c, &setting(2, "local_edit")).await?,
            count(&c, &setting(2, "snapshot_owned")).await?
        ),
        (0, 1)
    );
    assert_eq!(count(&c, &setting(3, "local_edit")).await?, 0);
    assert_eq!(
        (
            count(&c, &setting(4, "monitored")).await?,
            count(&c, &setting(4, "local_edit")).await?
        ),
        (0, 1)
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM movie_collection_settings WHERE collection_id=5"
        )
        .await?,
        0
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_collection_tags WHERE collection_id=3 AND tag_id=51 AND media_type='movies'").await?, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM movie_collections WHERE id=1 AND metadata_revision=3 AND sort_title='owned' AND added='2024-01-02T03:04:05Z' AND last_info_sync='2024-02-03T04:05:06Z' AND images_json LIKE '%p.jpg%'").await?, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM movie_collection_members m JOIN movie_collections c ON c.id=m.collection_id WHERE m.metadata_revision=c.metadata_revision").await?, 3);
    assert_eq!(
        text(
            &c,
            "SELECT origin FROM movie_collection_members WHERE collection_id=1 AND metadata_id=2"
        )
        .await?,
        "snapshot"
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_import_exclusions WHERE tmdb_id=102 AND local_edit=1 AND revision=4").await?, 1);
    assert_eq!(count(&c, "SELECT count(*) FROM snapshot_imports WHERE application='radarr' AND collection_version=1 AND provider_authority_version=0").await?, 1);

    // Notifications: the fixture now includes two separate terminal system events; pending
    // deliveries retain recorded domains. Nothing is marked delivered or invented by conversion.
    assert_eq!(
        count(&c, "SELECT count(*) FROM notification_events").await?,
        6
    );
    assert_eq!(
        count(&c, "SELECT count(*) FROM notification_deliveries").await?,
        6
    );
    let delivery = |n: i64, column: &'static str| {
        format!("SELECT {column} FROM notification_deliveries WHERE id={n}")
    };
    assert_eq!(text(&c, &delivery(1, "media_type")).await?, "movies");
    assert_eq!(
        text(&c, &delivery(2, "media_type")).await?,
        "tv",
        "the pending delivery retains its recorded event scope"
    );
    assert_eq!(text(&c, &delivery(3, "media_type")).await?, "movies");
    assert_eq!(text(&c, &delivery(4, "media_type")).await?, "movies");
    assert_eq!(text(&c, &delivery(5, "state")).await?, "failed");
    assert_eq!(count(&c, &delivery(5, "attempts")).await?, 16);
    assert_eq!(text(&c, &delivery(1, "state")).await?, "pending");
    assert_eq!(
        (
            count(&c, &delivery(1, "attempts")).await?,
            count(&c, &delivery(1, "next_attempt_at")).await?
        ),
        (2, 10)
    );
    assert_eq!(
        count(&c, &delivery(1, "lease_until")).await?,
        0,
        "a stopped instance's claim is released"
    );
    assert_eq!(text(&c, &delivery(3, "last_error")).await?, "http_500");
    assert_eq!(text(&c, &delivery(4, "state")).await?, "delivered");
    assert_eq!(count(&c, &delivery(4, "completed_at")).await?, 5);
    assert_eq!(count(&c, "SELECT count(*) FROM notification_deliveries WHERE sent_parts!='[]' OR lease_owner IS NOT NULL").await?, 0);
    assert_eq!(
        count(
            &c,
            "SELECT seq FROM sqlite_sequence WHERE name='notification_deliveries'"
        )
        .await?,
        7,
        "ids already issued are never reused"
    );

    // Providers, library and tracker options survive.
    assert_eq!(
        count(
            &c,
            "SELECT minimum_seeders FROM provider_scopes WHERE implementation='torrentrss'"
        )
        .await?,
        3
    );
    assert_eq!(
        text(
            &c,
            "SELECT category FROM provider_scopes WHERE implementation='downloadstation'"
        )
        .await?,
        "tv"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id"
        )
        .await?,
        1
    );
    assert_eq!(count(&c, "SELECT count(*) FROM movie_files").await?, 1);
    assert_eq!(
        count(&c, "SELECT count(*) FROM provider_status").await?,
        0,
        "no failure state is invented"
    );

    // The converted schema is exactly what a fresh install has, triggers included.
    let fresh = Database::open_local(s.file("fresh.db")).await?;
    let f = fresh.connect().await?;
    let schema = "SELECT group_concat(type||name||sql,char(10)) FROM (SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name)";
    assert_eq!(text(&c, schema).await?, text(&f, schema).await?);
    let ledger = "SELECT group_concat(version||name||checksum||sql,char(10)) FROM (SELECT * FROM schema_migrations ORDER BY version)";
    assert_eq!(text(&c, ledger).await?, text(&f, ledger).await?);

    // Rerunning never overwrites; the converted database reopens unchanged.
    assert!(
        convert_premerge_pr32(&s.file("old.db"), &s.file("new.db"))
            .await
            .is_err()
    );
    drop((c, db));
    Database::open_local(s.file("new.db")).await?;
    Ok(())
}

#[tokio::test]
async fn every_supported_prefix_converts() -> Result<(), Error> {
    for upto in [63, 64, 66, 67, 68, 69, 70, 75, 76] {
        let s = Scratch::new();
        let raw = premerge(&s.file("old.db"), upto).await?;
        let c = raw.connect()?;
        populate_library(&c, upto)
            .await
            .map_err(|e| format!("library {upto}: {e}"))?;
        populate_notifications(&c)
            .await
            .map_err(|e| format!("notifications {upto}: {e}"))?;
        if upto >= 68 {
            populate_providers(&c, upto)
                .await
                .map_err(|e| format!("providers {upto}: {e}"))?;
        }
        drop((c, raw));
        let report = convert_premerge_pr32(&s.file("old.db"), &s.file("new.db"))
            .await
            .map_err(|e| format!("prefix {upto}: {e}"))?;
        assert_eq!(report.source_version, upto);
        let db = Database::open_local(s.file("new.db")).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, MIGRATIONS.len());
        assert_eq!(
            count(
                &c,
                "SELECT count(*) FROM notification_deliveries WHERE media_type IN ('tv','movies')"
            )
            .await?,
            6,
            "prefix {upto}"
        );
        if upto >= 67 {
            // Before the ownership markers a set policy was a person's edit, as in main's own migration.
            let marker = count(
                &c,
                "SELECT local_edit FROM movie_collection_settings WHERE collection_id=1",
            )
            .await?;
            assert_eq!(marker, 1, "prefix {upto}");
            assert_eq!(
                count(
                    &c,
                    "SELECT monitored FROM movie_collection_settings WHERE collection_id=1"
                )
                .await?,
                1,
                "prefix {upto}"
            );
        }
        // Seeds of migrations the source never ran are kept (the registry knows every implementation).
        assert_eq!(
            count(&c, "SELECT count(*) FROM provider_implementations").await?,
            count(
                &Database::open_local(s.file("fresh.db"))
                    .await?
                    .connect()
                    .await?,
                "SELECT count(*) FROM provider_implementations"
            )
            .await?,
            "prefix {upto}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn unrecognized_state_is_refused_before_anything_is_written() -> Result<(), Error> {
    // A modified ledger entry, a stray table, an undeterminable delivery domain, a main-lineage
    // database and a non-empty write-ahead log are each refused with the source untouched.
    type Break = fn(
        &Connection,
    )
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + '_>>;
    let cases: [(&str, Break, &str); 3] = [
        (
            "tampered",
            |c| {
                Box::pin(async move {
                    c.execute(
                        "UPDATE schema_migrations SET sql=sql||' ' WHERE version=70",
                        (),
                    )
                    .await?;
                    Ok(())
                })
            },
            "differs from the pinned pre-merge PR #32 ledger at version 70",
        ),
        (
            "stray",
            |c| {
                Box::pin(async move {
                    c.execute("CREATE TABLE stray(x)", ()).await?;
                    Ok(())
                })
            },
            "stray has no destination",
        ),
        (
            "unscoped",
            |c| {
                Box::pin(async move {
                    c.execute_batch(&format!("INSERT INTO notifications(id,name,implementation,settings_json,created_at,updated_at) VALUES('{NOTIFY_A}','A','webhook','{{}}',1,1);
              INSERT INTO notification_events(sequence,kind,scope,subject_id,payload_json,created_at) VALUES(1,'health_issue','system',NULL,'{{}}',1);
              INSERT INTO notification_deliveries(event_sequence,notification_id,next_attempt_at,created_at) VALUES(1,'{NOTIFY_A}',1,1);")).await?;
                    Ok(())
                })
            },
            // Pending system deliveries are ambiguous even before checking current scopes.
            "no recorded fan-out domain",
        ),
    ];
    for (label, tamper, expected) in cases {
        let s = Scratch::new();
        let raw = premerge(&s.file("old.db"), 76).await?;
        tamper(&raw.connect()?).await?;
        drop(raw);
        let before = file_digest(&s.file("old.db")).await?;
        let error = convert_premerge_pr32(&s.file("old.db"), &s.file("new.db"))
            .await
            .err()
            .ok_or(label)?;
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert_eq!(file_digest(&s.file("old.db")).await?, before, "{label}");
        assert_clean(&s, &["old.db"]);
    }

    let s = Scratch::new();
    drop(Database::open_local(s.file("main.db")).await?);
    let error = convert_premerge_pr32(&s.file("main.db"), &s.file("new.db"))
        .await
        .err()
        .ok_or("main lineage")?;
    assert!(
        error
            .to_string()
            .contains("differs from the pinned pre-merge PR #32 ledger at version 63"),
        "{error}"
    );
    assert_clean(&s, &["main.db"]);

    drop(premerge(&s.file("old.db"), 76).await?);
    std::fs::write(s.file("old.db-wal"), b"pending")?;
    let error = convert_premerge_pr32(&s.file("old.db"), &s.file("new.db"))
        .await
        .err()
        .ok_or("wal")?;
    assert!(error.to_string().contains("write-ahead log"), "{error}");
    std::fs::remove_file(s.file("old.db-wal"))?;
    std::fs::write(s.file("new.db"), b"occupied")?;
    assert!(
        convert_premerge_pr32(&s.file("old.db"), &s.file("new.db"))
            .await
            .err()
            .ok_or("exists")?
            .to_string()
            .contains("never overwrites")
    );
    assert_eq!(std::fs::read(s.file("new.db"))?, b"occupied");
    Ok(())
}

#[tokio::test]
async fn pending_system_deliveries_are_refused_regardless_of_current_scopes() -> Result<(), Error> {
    for scopes in ["'tv'", "'movies'", "'tv','movies'"] {
        let s = Scratch::new();
        let source = s.file("old.db");
        let raw = premerge(&source, 76).await?;
        let c = raw.connect()?;
        populate_notifications(&c).await?;
        // Simulate scope changes after fan-out: neither one scope nor two establish its domain.
        c.execute_batch(&format!("UPDATE notification_events SET scope='system' WHERE sequence=2;
            DELETE FROM notification_scopes WHERE notification_id='{NOTIFY_A}' AND media_type NOT IN ({scopes});")).await?;
        drop((c, raw));
        let before = file_digest(&source).await?;
        let error = convert_premerge_pr32(&source, &s.file("new.db"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no recorded fan-out domain"),
            "{error}"
        );
        assert_eq!(file_digest(&source).await?, before);
        assert_clean(&s, &["old.db"]);
    }
    Ok(())
}

#[tokio::test]
async fn source_ownership_excludes_conversion_and_application_opens() -> Result<(), Error> {
    let s = Scratch::new();
    let source = s.file("old.db");
    drop(premerge(&source, 76).await?);
    let before = file_digest(&source).await?;
    let owner = super::convert::lock_source(&source)?;
    let error = Database::open_local(&source)
        .await
        .err()
        .ok_or("application took owned source")?;
    assert!(error.to_string().contains("already owned"), "{error}");
    let error = convert_premerge_pr32(&source, &s.file("new.db"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already owned"), "{error}");
    assert_eq!(file_digest(&source).await?, before);
    assert_clean(&s, &["old.db"]);
    drop(owner);
    // Ownership is released on refusal/completion; an offline source can then convert normally.
    convert_premerge_pr32(&source, &s.file("new.db")).await?;
    assert_eq!(file_digest(&source).await?, before);
    Ok(())
}

#[tokio::test]
async fn rollback_journal_is_refused_without_recovery_or_copy() -> Result<(), Error> {
    let s = Scratch::new();
    let source = s.file("old.db");
    drop(premerge(&source, 76).await?);
    let before = file_digest(&source).await?;
    let journal = s.file("old.db-journal");
    std::fs::write(&journal, b"unrecovered pages")?;
    let error = convert_premerge_pr32(&source, &s.file("new.db"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rollback journal"), "{error}");
    assert_eq!(std::fs::read(&journal)?, b"unrecovered pages");
    assert_eq!(file_digest(&source).await?, before);
    assert_clean(&s, &["old.db", "old.db-journal"]);
    std::fs::remove_file(&journal)?;
    std::fs::create_dir(&journal)?;
    assert!(
        convert_premerge_pr32(&source, &s.file("new.db"))
            .await
            .is_err()
    );
    Ok(())
}

#[test]
fn publication_never_replaces_a_destination_created_after_preflight() -> Result<(), Error> {
    let s = Scratch::new();
    let built = s.file("built.db");
    let destination = s.file("new.db");
    std::fs::write(&built, b"converted")?;
    assert!(!destination.exists());
    // The destination appears after the converter's initial existence check.
    std::fs::write(&destination, b"another database")?;
    assert!(super::convert::publish(&built, &destination).is_err());
    assert_eq!(std::fs::read(&destination)?, b"another database");
    assert_eq!(std::fs::read(&built)?, b"converted");
    #[cfg(unix)]
    {
        std::fs::remove_file(&destination)?;
        std::os::unix::fs::symlink(s.file("missing.db"), &destination)?;
        assert!(super::convert::publish(&built, &destination).is_err());
        assert!(
            std::fs::symlink_metadata(&destination)?
                .file_type()
                .is_symlink()
        );
        assert!(!s.file("missing.db").exists());
    }
    Ok(())
}
