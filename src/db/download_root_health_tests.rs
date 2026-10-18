use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-download-root-health-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn rows(c: &Connection, query: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut cursor = c.query(query, ()).await?;
    let mut result = vec![];
    while let Some(row) = cursor.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn integrity(c: &Connection) -> Result<(), Error> {
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    assert_eq!(
        c.query("PRAGMA integrity_check", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "ok"
    );
    Ok(())
}
async fn new_registry(c: &Connection) -> Result<(), Error> {
    assert_eq!(
        scalar(
            c,
            "SELECT count(*) FROM health_checks WHERE check_key='download_client_root_folder'"
        )
        .await?,
        2
    );
    assert_eq!(scalar(c,"SELECT count(*) FROM health_checks WHERE scope IN ('tv','movies') AND check_key='download_client_root_folder' AND startup=1 AND scheduled=1 AND compatibility_type='DownloadClientRootFolderCheck' AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL AND last_error IS NULL AND severity IS NULL AND reason IS NULL AND message IS NULL AND wiki_url IS NULL").await?, 2);
    Ok(())
}

#[tokio::test]
async fn download_root_health_fresh_registry_uses_existing_startup_generation() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 49); // Reasoning: latest migration is now 0049 indexer client binding (was 48: indexer operation policy); historical migration prefixes stay fixed.
    // Two removed-metadata registry entries join the six previously seeded checks.
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 14); // Reasoning: 0048 seeds four indexer_search/indexer_rss rows and 0049 two indexer_download_client rows (one per domain), so the registry is 14 (was 12 at schema48, 8 before).
    new_registry(&c).await?;
    let tx = c.transaction().await?;
    let error = tx.execute("UPDATE health_checks SET generation=2 WHERE scope='tv' AND check_key='download_client_root_folder'", ()).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid health identity or generation"),
        "{error}"
    );
    tx.execute("UPDATE health_checks SET generation=1,pending_reasons=8,due_at=105 WHERE scope='tv' AND check_key='download_client_root_folder'", ()).await?;
    assert_eq!(scalar(&tx,"SELECT count(*) FROM health_checks WHERE scope='tv' AND check_key='download_client_root_folder' AND generation=1 AND pending_reasons=8 AND due_at=105").await?, 1);
    assert_eq!(scalar(&tx,"SELECT count(*) FROM health_checks WHERE scope='movies' AND check_key='download_client_root_folder' AND generation=0 AND pending_reasons=0 AND due_at IS NULL").await?, 1);
    tx.rollback().await?;
    new_registry(&c).await?;
    // Exercise the native startup selector: migration itself does not invent observations or time.
    crate::health::startup(&db).await.unwrap();
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE scope IN ('tv','movies') AND check_key='download_client_root_folder' AND generation=1 AND pending_reasons=1 AND due_at=(SELECT started_at FROM health_lifecycle WHERE id=1) AND observed_generation IS NULL AND severity IS NULL").await?, 2);
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_commands").await?, 0);
    integrity(&c).await
}

#[tokio::test]
async fn download_root_health_schema43_preserves_pending_observations_and_replays()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    // Real predecessor43: do not replace the historical prefix with latest44.
    for (i, (name, sql)) in MIGRATIONS.iter().take(43).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let epoch = uuid::Uuid::new_v4().to_string();
    c.execute("UPDATE health_lifecycle SET epoch=?,started_at=100,grace_due_at=1000,next_scheduled_at=21700,schedule_error='storage_error',last_batch_completed_at=150", [epoch.clone()]).await?;
    c.execute("UPDATE health_checks SET generation=1,pending_reasons=9,due_at=105,observed_generation=0,observed_epoch=?,checked_at=100,severity=2,reason='previous_issue',message='Retained diagnostic',wiki_url='https://example.invalid/health',last_error='stale_inputs' WHERE scope='tv'", [epoch.clone()]).await?;
    c.execute("UPDATE health_checks SET observed_generation=0,observed_epoch=?,checked_at=100,severity=0 WHERE scope='movies'", [epoch.clone()]).await?;
    let command = uuid::Uuid::new_v4().to_string();
    c.execute(
        "INSERT INTO health_commands(id,epoch,next_attempt_at,created_at)VALUES(?,?,105,100)",
        params![command.clone(), epoch.clone()],
    )
    .await?;
    c.execute("INSERT INTO health_command_checks(command_id,scope,check_key,admitted_generation)SELECT ?,scope,check_key,generation FROM health_checks", [command]).await?;
    c.execute("INSERT INTO health_transitions(event_id,epoch,command_id,command_attempt,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type)VALUES(?,?,?,1,'tv','download_client_communication','issue',0,100,2,'previous_issue','Retained diagnostic','https://example.invalid/health','DownloadClientCheck')", params![uuid::Uuid::new_v4().to_string(),epoch,uuid::Uuid::new_v4().to_string()]).await?;
    c.execute(
        "UPDATE sqlite_sequence SET seq=7000 WHERE name='health_transitions'",
        (),
    )
    .await?;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv'); INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'Pilot'); INSERT INTO movie_metadata(id,title)VALUES(1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies'); INSERT INTO root_folders(media_type,path)VALUES('tv','/unused/tv'),('movies','/unused/movies');").await?;
    // Compare every predecessor check; only migration44, 46, 48 and 49 registrations are new (Reasoning: 0048 indexer_search/indexer_rss and 0049 indexer_download_client rows are excluded from the predecessor witness like the earlier additions).
    let queries = [
        "SELECT * FROM health_checks WHERE check_key NOT IN ('download_client_root_folder','removed_metadata','indexer_search','indexer_rss','indexer_download_client') ORDER BY scope,check_key",
        "SELECT * FROM health_lifecycle",
        "SELECT * FROM health_commands ORDER BY id",
        "SELECT * FROM health_command_checks ORDER BY command_id,scope,check_key",
        "SELECT * FROM health_transitions ORDER BY sequence",
        "SELECT * FROM sqlite_sequence ORDER BY name,seq",
        // Metadata45 appends nullable facts; preserve every predecessor series column by name.
        "SELECT id,tvdb_id,title,year,path,poster,monitored,original_language FROM series ORDER BY id",
        "SELECT * FROM episodes ORDER BY id",
        "SELECT * FROM movies ORDER BY id",
        "SELECT * FROM root_folders ORDER BY id",
    ];
    let mut before = vec![];
    for query in queries {
        before.push(rows(&c, query).await?);
    }
    // Registration succeeds inside a transaction, then rollback removes only the two new rows.
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[43].1).await?;
    new_registry(&tx).await?;
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 43);
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 4);
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(&rows(&c, query).await?, expected, "rollback {query}");
    }
    // Registry has128 bounded entries: a one-slot predecessor must fail atomically, not half-register.
    let tx = c.transaction().await?;
    for index in 0..123 {
        tx.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('system',?,0,0,'Fixture')", [format!("capacity_{index}")]).await?;
    }
    let error = tx.execute_batch(MIGRATIONS[43].1).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("health registry capacity reached"),
        "{error}"
    );
    tx.rollback().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 4);
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(
            &rows(&c, query).await?,
            expected,
            "capacity rollback {query}"
        );
    }
    drop(c);
    drop(raw);
    for _ in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 49); // Reasoning: latest migration is now 0049 indexer client binding (was 48: indexer operation policy); historical migration prefixes stay fixed.
        assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 14); // Migration46 adds two removed-metadata identities; 0048 adds four indexer identities (8 -> 12); 0049 adds two indexer_download_client identities (12 -> 14).
        new_registry(&c).await?;
        for (query, expected) in queries.iter().zip(&before) {
            assert_eq!(&rows(&c, query).await?, expected, "upgrade/replay {query}");
        }
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM schema_migrations WHERE version=44"
            )
            .await?,
            1
        );
        assert_eq!(
            c.query(
                "SELECT checksum FROM schema_migrations WHERE version=44",
                ()
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
            checksum(MIGRATIONS[43].1)
        );
        integrity(&c).await?;
    }
    Ok(())
}
