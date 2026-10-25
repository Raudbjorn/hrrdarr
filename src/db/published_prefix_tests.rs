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
