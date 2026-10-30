//! The published origin/main prefix continues into this branch's history. A database that ran
//! those migrations keeps a valid history and upgrades, and the collections checkpoint it created
//! is superseded with the policy a person configured carried over.
use super::*;
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-published-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How many migrations origin/main published. This branch continues that history instead of
/// reusing those version slots.
const PUBLISHED: usize = 53;
/// Published versions 50–53 are immutable: their names and SQL bytes are part of the installed
/// migration history, not just the current source tree.
const PUBLISHED_TAIL: [(usize, &str, &str); 4] = [
    (
        50,
        "torrent_rss_indexer",
        "f092910a3183e3cb32287dbcf6fe6fe84ace79ddd7d6154af250b2a8095a4a7b",
    ),
    (
        51,
        "download_client_backoff",
        "8a89120239b50eb4491ce3ab7ef49e348a30829626f64514975fe3afc77da6af",
    ),
    (
        52,
        "collections",
        "c2b4219c28772e266e872e5630c4017618ff8496b1c356d108f0cbe1f479b275",
    ),
    (
        53,
        "provider_authority",
        "af70455c64b2872339be34a650c044b5450ad4b8b50dfed6d0fe3588318dcaa0",
    ),
];

#[tokio::test]
async fn published_prefix_upgrades_and_keeps_its_collection_policy() -> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(PUBLISHED).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    // The published prefix is the one origin/main recorded.
    for (version, name, expected_checksum) in PUBLISHED_TAIL {
        let row = c
            .query(
                "SELECT name,checksum FROM schema_migrations WHERE version=?",
                [version as i64],
            )
            .await?
            .next()
            .await?
            .unwrap();
        assert_eq!(
            row.get::<String>(0)?,
            name,
            "published migration {version} name"
        );
        assert_eq!(
            row.get::<String>(1)?,
            expected_checksum,
            "published migration {version} checksum"
        );
    }
    let mut rows = c
        .query(
            "SELECT version,name FROM schema_migrations WHERE version>=50 ORDER BY version",
            (),
        )
        .await?;
    let mut published = Vec::new();
    while let Some(row) = rows.next().await? {
        published.push((row.get::<i64>(0)?, row.get::<String>(1)?));
    }
    assert_eq!(
        published,
        vec![
            (50, "torrent_rss_indexer".to_owned()),
            (51, "download_client_backoff".to_owned()),
            (52, "collections".to_owned()),
            (53, "provider_authority".to_owned()),
        ]
    );
    // A collection configured under the published checkpoint, with a tag and a local edit.
    c.execute_batch(
        "INSERT INTO root_folders(id,media_type,path) VALUES(1,'movies','/movies');
         INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'movies','Any');
         INSERT INTO tags(id,media_type,label) VALUES(1,'movies','kept');
         INSERT INTO movie_collections(id,tmdb_id,title) VALUES(1,42,'Kept');
         INSERT INTO movie_collection_settings(collection_id,monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit,snapshot_owned)
           VALUES(1,1,1,1,'in_cinemas',1,1,0);
         INSERT INTO movie_collection_tags(collection_id,tag_id) VALUES(1,1);",
    )
    .await?;
    drop(rows);
    drop(c);
    drop(raw);

    // Reopening runs the pending migrations over the published history.
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await?,
        MIGRATIONS.len() as i64
    );
    // The checkpoint's policy now lives on the collection itself.
    let row = c
        .query(
            "SELECT monitored,root_folder_id,quality_profile_id,minimum_availability,search_on_add,local_edit FROM movie_collections WHERE id=1",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(row.get::<i64>(0)?, 1, "monitored");
    assert_eq!(row.get::<i64>(1)?, 1, "root folder");
    assert_eq!(row.get::<i64>(2)?, 1, "quality profile");
    assert_eq!(row.get::<String>(3)?, "in_cinemas", "minimum availability");
    assert_eq!(row.get::<i64>(4)?, 1, "search on add");
    assert_eq!(row.get::<i64>(5)?, 1, "local edit");
    // The tag survived the supersede, and the checkpoint's own tables are gone.
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_collection_tags WHERE collection_id=1"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='movie_collection_settings'"
        )
        .await?,
        0
    );
    Ok(())
}

/// The published prefix admits a 'torrentrss' provider, so an installation that configured one
/// carries that row. The registry the providers table is rebuilt against must know the
/// implementation, or the upgrade fails on the foreign key.
#[tokio::test]
async fn published_prefix_upgrades_with_a_torrentrss_provider() -> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(PUBLISHED).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute(
        "INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint,credentials)
           VALUES('11111111-1111-4111-8111-111111111111','torrentrss','Feed',1,50,1,1,'http://feed.example/rss',NULL)",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,enable_automatic_search,enable_interactive_search,minimum_seeders)
           VALUES('11111111-1111-4111-8111-111111111111','torrentrss','tv',NULL,NULL,0,0,7)",
        (),
    )
    .await?;
    drop(c);
    drop(raw);

    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM schema_migrations").await?,
        MIGRATIONS.len() as i64
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM providers WHERE implementation='torrentrss'"
        )
        .await?,
        1,
        "the torrentrss provider survives the upgrade"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT minimum_seeders FROM provider_scopes WHERE provider_id='11111111-1111-4111-8111-111111111111' AND media_type='tv'"
        )
        .await?,
        7,
        "the published feed's minimum seeders survives the registry rebuild"
    );
    Ok(())
}

async fn published(path: &Path, count: usize) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(count).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok((raw, c))
}

#[tokio::test]
async fn every_published_tail_prefix_upgrades_without_rewriting_history() -> Result<(), Error> {
    for count in 50..=53 {
        let files = Scratch::new();
        let path = files.0.join("db");
        let (raw, c) = published(&path, count).await?;
        c.execute(
            "INSERT INTO series(id,title,path) VALUES(123,'Kept','/tv/kept')",
            (),
        )
        .await?;
        let client = super::refresh_tests::provider(&c).await?;
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES('11111111-1111-4111-8111-111111111111','torrentrss','Feed',1,1,1,1,'http://fixture.invalid')",()).await?;
        for media in ["tv", "movies"] {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,enable_automatic_search,enable_interactive_search,minimum_seeders) VALUES('11111111-1111-4111-8111-111111111111','torrentrss',?,0,0,7)",[media]).await?;
            if count >= 51 {
                c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until) VALUES(?,?,1,2,100,200,500)",params![client.clone(),media]).await?;
            }
        }
        if count >= 52 {
            c.execute("INSERT INTO movie_collection_intents(tmdb_id,local_edit,removed,removal_reason) VALUES(42,1,1,'user')",()).await?;
        }
        drop(c);
        drop(raw);
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, MIGRATIONS.len());
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM series WHERE id=123").await?,
            1
        );
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scopes WHERE implementation='torrentrss' AND minimum_seeders=7").await?,2);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM download_client_status WHERE disabled_until=500"
            )
            .await?,
            if count >= 51 { 2 } else { 0 }
        );
        assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_intents WHERE tmdb_id=42 AND local_edit=1 AND removed=1 AND removal_reason='user'").await?,if count >= 52 {1} else {0});
        for (i, (name, sql)) in MIGRATIONS.iter().take(count).enumerate() {
            let row = c
                .query(
                    "SELECT name,checksum,sql FROM schema_migrations WHERE version=?",
                    [i as i64 + 1],
                )
                .await?
                .next()
                .await?
                .unwrap();
            assert_eq!(row.get::<String>(0)?, *name);
            assert_eq!(row.get::<String>(1)?, checksum(sql));
            assert_eq!(row.get::<String>(2)?, *sql);
        }
        drop(c);
        drop(db);
        assert!(
            Database::open_local(&path)
                .await?
                .migration_backup()
                .is_none()
        );
    }
    Ok(())
}

#[tokio::test]
async fn populated_published_authority_survives_rollback_upgrade_and_reopen() -> Result<(), Error> {
    let files = Scratch::new();
    let path = files.0.join("db");
    let (raw, c) = published(&path, PUBLISHED).await?;
    let client = super::refresh_tests::provider(&c).await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Indexer',1,1,1,1,'http://fixture.invalid')",[indexer.clone()]).await?;
    c.execute_batch("INSERT INTO tags(id,media_type,label) VALUES(1,'tv','kept'),(2,'movies','kept');
        INSERT INTO movie_collection_intents(tmdb_id,revision,local_edit,removed,removal_reason) VALUES(42,7,1,1,'user'),(43,9,0,1,'metadata_missing');
        INSERT INTO movie_import_exclusions(tmdb_id,title,excluded,local_edit,revision) VALUES(123,'Excluded',1,1,4);
        INSERT INTO movie_collections(id,tmdb_id,title,sort_title,overview,images_json,metadata_revision,graph_complete,added,last_info_sync) VALUES(1,55,'Kept','kept','Overview','[]',8,1,'2026-01-01T00:00:00Z','2026-01-02T00:00:00Z');
        INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,123,'Movie');
        INSERT INTO movie_collection_members(collection_id,metadata_id,origin,metadata_revision) VALUES(1,1,'graph',8);
        INSERT INTO movie_collection_settings(collection_id,settings_revision,local_edit,snapshot_owned) VALUES(1,6,1,1);") .await?;
    for (media, tag) in [("tv", 1), ("movies", 2)] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,'[1]','[]',?,?)",params![indexer.clone(),media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
        for id in [&client, &indexer] {
            c.execute(
                "INSERT INTO provider_scope_tags(provider_id,media_type,tag_id) VALUES(?,?,?)",
                params![id.clone(), media, tag],
            )
            .await?;
        }
        c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until) VALUES(?,?,1,2,100,200,500)",params![client.clone(),media]).await?;
        c.execute("UPDATE health_checks SET next_expiry_at=400 WHERE scope=? AND check_key='download_client_backoff'",[media]).await?;
        c.execute("INSERT INTO provider_selection_cursors(media_type,protocol,last_client_id,revision) VALUES(?,'torrent',?,1)",params![media,client.clone()]).await?;
    }
    c.execute("INSERT INTO provider_status(provider_id,escalation_level,status_version,last_observed_config_revision,initial_failure,most_recent_failure,disabled_until) VALUES(?,2,1,1,'2026-01-01T00:00:00Z','2026-01-02T00:00:00Z','2026-01-03T00:00:00Z')",[indexer.clone()]).await?;
    c.execute("INSERT INTO provider_tests(provider_id,config_revision,tested_at,status,error_code) VALUES(?,1,100,'success',NULL)",[client.clone()]).await?;
    // A late publication failure must roll back both schema and transferred authority.
    c.execute("CREATE TRIGGER reject_upgrade BEFORE INSERT ON schema_migrations WHEN NEW.version=82 BEGIN SELECT RAISE(ABORT,'fixture rollback'); END",()).await?;
    drop(c);
    drop(raw);
    assert!(
        Database::open_local(&path)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("rolled back")
    );
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(version(&c).await?, PUBLISHED);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM provider_scope_tags").await?,
        4
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM movie_collection_intents").await?,
        2
    );
    c.execute("DROP TRIGGER reject_upgrade", ()).await?;
    drop(c);
    drop(raw);
    for reopen in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(db.migration_backup().is_some(), reopen == 0);
        assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_intents WHERE (tmdb_id=42 AND revision=7 AND local_edit=1 AND removed=1 AND removal_reason='user') OR (tmdb_id=43 AND revision=9 AND local_edit=0 AND removed=1 AND removal_reason='metadata_missing')").await?,2);
        assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collections WHERE id=1 AND sort_title='kept' AND overview='Overview' AND images_json='[]' AND metadata_revision=8 AND graph_complete=1 AND added='2026-01-01T00:00:00Z' AND last_info_sync='2026-01-02T00:00:00Z' AND local_edit=1 AND snapshot_owned=1 AND checkpoint_settings_revision=6").await?,1);
        assert_eq!(scalar(&c,"SELECT count(*) FROM movie_collection_members WHERE metadata_revision=8 AND origin='graph'").await?,1);
        assert_eq!(scalar(&c,"SELECT count(*) FROM movie_import_exclusions WHERE tmdb_id=123 AND revision=4 AND local_edit=1 AND excluded=1").await?,1);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM download_client_tags").await?,
            2
        );
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scope_tags t JOIN providers p ON p.id=t.provider_id WHERE p.implementation='torznab'").await?,2);
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_scope_tags t JOIN providers p ON p.id=t.provider_id WHERE p.implementation='qbittorrent'").await?,0);
        assert_eq!(scalar(&c,"SELECT count(*) FROM download_client_status WHERE config_revision=1 AND escalation_level=2 AND initial_failure_at=100 AND last_failure_at=200 AND disabled_until=500 AND next_health_at=400").await?,2);
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_selection_cursors WHERE revision=1 AND protocol='torrent'").await?,2);
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_status WHERE status_version=1 AND escalation_level=2 AND disabled_until='2026-01-03T00:00:00Z'").await?,1);
        assert_eq!(scalar(&c,"SELECT count(*) FROM provider_tests WHERE config_revision=1 AND tested_at=100 AND status='success'").await?,1);
        integrity(&c).await?;
        drop(c);
        drop(db);
    }
    Ok(())
}
