//! Migration 0091 (Wanted search admission receipts, ordinal 80): storage guards, a populated
//! upgrade from the previous head, rollback of a failed upgrade, and reopen durability.
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-wanted-receipts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
// Ordinals 1-79 are the unchanged main history plus indexer breadth; receipts are appended.
const RECEIPT_MIGRATION: usize = 79;

async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    // WITHOUT ROWID tables have no rowid; their primary key is the stable order.
    let order = if table == "wanted_search_receipt_targets" {
        "request_id,target_id"
    } else {
        "rowid"
    };
    let mut cursor = c
        .query(&format!("SELECT * FROM {table} ORDER BY {order}"), ())
        .await?;
    let mut values = Vec::new();
    while let Some(row) = cursor.next().await? {
        values.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(values)
}
async fn at_prior(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let raw = libsql::Builder::new_local(path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(RECEIPT_MIGRATION).enumerate() {
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
fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}
const INSERT: &str = "INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at)VALUES(?,?,?,?,?,?,?,?)";

#[tokio::test]
async fn storage_guards_on_a_fresh_database() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    let digest = vec![7u8; 32];
    let ok = |n: u32, media: &str, matched: i64, skipped: i64, admitted: i64| {
        let (c, digest) = (c.clone(), digest.clone());
        let media = media.to_owned();
        async move {
            c.execute(
                INSERT,
                params![
                    id(n),
                    media,
                    digest,
                    matched,
                    skipped,
                    admitted,
                    1000_i64,
                    2000_i64
                ],
            )
            .await
        }
    };
    // Empty and all-skipped results are valid receipts: the counters must reconcile.
    ok(1, "tv", 0, 0, 0).await?;
    ok(2, "movies", 5, 5, 0).await?;
    ok(3, "tv", 2, 0, 2).await?;
    for (n, media, m, s, a) in [
        (4, "tv", 3, 0, 2),
        (5, "tv", 1, 0, 2),
        (6, "radio", 0, 0, 0),
        (7, "tv", 1025, 0, 1025),
    ] {
        assert!(
            ok(n, media, m, s, a).await.is_err(),
            "accepted {n} {media} {m} {s} {a}"
        );
    }
    // Duplicate UUID, malformed UUID, short digest, backwards expiry.
    assert!(ok(1, "tv", 0, 0, 0).await.is_err());
    for (request_id, digest, created, expires) in [
        ("not-a-uuid".to_owned(), vec![7u8; 32], 1000_i64, 2000_i64),
        (id(0xab).to_uppercase(), vec![7u8; 32], 1000, 2000),
        (id(8), vec![7u8; 31], 1000, 2000),
        (id(8), vec![7u8; 32], 2000, 2000),
    ] {
        assert!(
            c.execute(
                INSERT,
                params![request_id, "tv", digest, 0, 0, 0, created, expires]
            )
            .await
            .is_err()
        );
    }
    // Targets are typed integers keyed by receipt and cascade with it.
    c.execute(
        "INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)",
        params![id(3), 11, id(900)],
    )
    .await?;
    c.execute(
        "INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)",
        params![id(3), 12, id(901)],
    )
    .await?;
    assert!(c.execute("INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)", params![id(3), 11, id(902)]).await.is_err());
    assert!(c.execute("INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)", params![id(99), 1, id(903)]).await.is_err());
    assert!(c.execute("INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)", params![id(3), 0, id(904)]).await.is_err());
    // Written once: neither table can be rewritten, only deleted.
    assert!(
        c.execute(
            "UPDATE wanted_search_receipts SET matched=9,admitted=9 WHERE request_id=?",
            [id(3)]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE wanted_search_receipt_targets SET command_id=? WHERE request_id=?",
            params![id(905), id(3)]
        )
        .await
        .is_err()
    );
    c.execute(
        "DELETE FROM wanted_search_receipts WHERE request_id=?",
        [id(3)],
    )
    .await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM wanted_search_receipt_targets").await?,
        0
    );
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn capacity_counts_only_unexpired_receipts_and_targets() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    let digest = vec![1u8; 32];
    // 4096 unexpired receipts fill the store; the next is refused by the schema itself.
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<4096) INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at) SELECT printf('00000000-0000-4000-8000-%012x',i),'tv',?,0,0,0,5000,9000 FROM n",
        [digest.clone()],
    )
    .await?;
    assert!(
        c.execute(
            INSERT,
            params![id(70000), "tv", digest.clone(), 0, 0, 0, 5000_i64, 9000_i64]
        )
        .await
        .is_err()
    );
    // Expired receipts do not count: one created before the others expire is accepted.
    c.execute(
        "DELETE FROM wanted_search_receipts WHERE request_id=?",
        [id(1)],
    )
    .await?;
    c.execute(
        INSERT,
        params![id(70001), "tv", digest.clone(), 0, 0, 0, 5000_i64, 9000_i64],
    )
    .await?;
    assert!(
        c.execute(
            INSERT,
            params![id(70002), "tv", digest.clone(), 0, 0, 0, 5000_i64, 9000_i64]
        )
        .await
        .is_err()
    );
    // Created after every other receipt expired, the store is empty from its point of view.
    c.execute(
        INSERT,
        params![id(70003), "tv", digest, 0, 0, 0, 9000_i64, 19000_i64],
    )
    .await?;
    // Admitted targets are bounded separately: 64 receipts of 1024 fill 65536, one more is refused.
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<64) INSERT INTO wanted_search_receipts(request_id,media_type,digest,matched,skipped_active,admitted,created_at,expires_at) SELECT printf('00000000-0000-4000-8000-%012x',i),'movies',?,1024,0,1024,5000,9000 FROM n",
        [vec![1u8; 32]],
    )
    .await?;
    assert!(
        c.execute(
            INSERT,
            params![
                id(80000),
                "movies",
                vec![1u8; 32],
                1,
                0,
                1,
                5000_i64,
                9000_i64
            ]
        )
        .await
        .is_err()
    );
    c.execute(
        INSERT,
        params![
            id(80001),
            "movies",
            vec![1u8; 32],
            1,
            1,
            0,
            5000_i64,
            9000_i64
        ],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn upgrade_from_the_previous_head_preserves_data_and_survives_reopen() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = at_prior(&path).await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM schema_migrations").await?,
        RECEIPT_MIGRATION as i64
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'wanted_search_receipt%'"
        )
        .await?,
        0
    );
    let provider = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'torznab','Native',1,2,3,1,'http://127.0.0.1:9/api')", [provider.clone()]).await?;
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search) VALUES(?,'torznab','tv','[5000]','[]',0)", [provider.clone()]).await?;
    c.execute_batch(
        "INSERT INTO series(id,title,path,monitored,runtime) VALUES(1,'Kept','/owned/kept',1,30);
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date_utc) VALUES(1,1,1,1,'Pilot',1,'2020-01-01T00:00:00Z');
         INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,201,'Kept',2020);
         INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/owned/film',1);",
    )
    .await?;
    let preserved = [
        "schema_migrations",
        "providers",
        "provider_scopes",
        "series",
        "episodes",
        "movie_metadata",
        "movies",
        "search_commands",
    ];
    let mut before = Vec::new();
    for table in preserved {
        before.push(rows(&c, table).await?);
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(
        db.migration_backup().is_some(),
        "a recovery backup precedes the upgrade"
    );
    let c = db.connect().await?;
    for (table, expected) in preserved.iter().zip(&before) {
        let actual = rows(&c, table).await?;
        if *table == "schema_migrations" {
            assert_eq!(
                &actual[..RECEIPT_MIGRATION],
                expected,
                "published history unchanged"
            );
        } else {
            assert_eq!(&actual, expected, "changed {table}");
        }
    }
    integrity(&c).await?;
    assert_eq!(
        count(&c, "SELECT count(*) FROM schema_migrations").await?,
        MIGRATIONS.len() as i64
    );
    assert_eq!(
        c.query("SELECT name FROM schema_migrations WHERE version=80", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "wanted_search_receipts"
    );
    // A receipt written now survives close and reopen; reopening applies nothing.
    c.execute(
        INSERT,
        params![id(1), "tv", vec![9u8; 32], 2, 1, 1, 1000_i64, 2000_i64],
    )
    .await?;
    c.execute(
        "INSERT INTO wanted_search_receipt_targets(request_id,target_id,command_id)VALUES(?,?,?)",
        params![id(1), 1, id(2)],
    )
    .await?;
    c.execute(
        INSERT,
        params![id(3), "movies", vec![8u8; 32], 0, 0, 0, 1000_i64, 2000_i64],
    )
    .await?;
    let receipts = rows(&c, "wanted_search_receipts").await?;
    let targets = rows(&c, "wanted_search_receipt_targets").await?;
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    let c = reopened.connect().await?;
    assert_eq!(rows(&c, "wanted_search_receipts").await?, receipts);
    assert_eq!(rows(&c, "wanted_search_receipt_targets").await?, targets);
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn failed_upgrade_rolls_back_and_leaves_a_usable_backup() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = at_prior(&path).await?;
    c.execute_batch(
        "INSERT INTO series(id,title,path,monitored,runtime) VALUES(1,'Kept','/owned/kept',1,30);
         CREATE TABLE wanted_search_receipt_targets(blocking INTEGER);",
    )
    .await?;
    let before = (
        rows(&c, "schema_migrations").await?,
        rows(&c, "series").await?,
    );
    drop(c);
    drop(raw);
    // The pre-existing table makes migration 80 fail after its first statement succeeded.
    let error = match Database::open_local(&path).await {
        Ok(_) => panic!("upgrade must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("migration 80"), "{error}");
    assert!(error.contains("migration rolled back"), "{error}");
    assert!(error.contains("recovery backup:"), "{error}");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(
        (
            rows(&c, "schema_migrations").await?,
            rows(&c, "series").await?
        ),
        before,
        "the failed upgrade left history and data untouched"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='wanted_search_receipts'"
        )
        .await?,
        0
    );
    let backups: Vec<_> = std::fs::read_dir(&scratch.0)?
        .filter_map(Result::ok)
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .starts_with(".hrrdarr-migration-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "one recovery backup remains");
    Ok(())
}
