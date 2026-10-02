use super::*;
use libsql::Value;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("removed-metadata-health-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("removed-health fixture cleanup failed: {e}");
        }
    }
}
async fn rows(c: &Connection, sql: &str) -> Result<Vec<Vec<Value>>, Error> {
    let mut cursor = c.query(sql, ()).await?;
    let mut all = vec![];
    while let Some(row) = cursor.next().await? {
        all.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(all)
}
async fn health(c: &Connection) -> Result<Vec<Vec<Value>>, Error> {
    rows(c, "SELECT * FROM health_checks ORDER BY scope,check_key").await
}
async fn generation(c: &Connection, scope: &str) -> Result<i64, Error> {
    Ok(c.query(
        "SELECT generation FROM health_checks WHERE scope=? AND check_key='removed_metadata'",
        [scope],
    )
    .await?
    .next()
    .await?
    .unwrap()
    .get(0)?)
}
async fn mutate(c: &Connection, sql: &str, tv: i64, movies: i64) -> Result<(), Error> {
    let t = generation(c, "tv").await?;
    let m = generation(c, "movies").await?;
    c.execute(sql, ()).await?;
    assert_eq!(generation(c, "tv").await?, t + tv, "{sql}");
    assert_eq!(generation(c, "movies").await?, m + movies, "{sql}");
    Ok(())
}
#[tokio::test]
async fn removed_health_fresh_library_mutations_are_scoped_and_transactional() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("library.db")).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 47); // Reasoning: latest is now 0047 movie credits; 46 stays injected-failure target below.
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND startup=1 AND scheduled=1 AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL AND severity IS NULL").await?,2);
    assert_eq!(rows(&c,"SELECT scope,compatibility_type FROM health_checks WHERE check_key='removed_metadata' ORDER BY scope").await?,vec![vec![Value::Text("movies".into()),Value::Text("RemovedMovieCheck".into())],vec![Value::Text("tv".into()),Value::Text("RemovedSeriesCheck".into())]]);
    let siblings = rows(
        &c,
        "SELECT * FROM health_checks WHERE check_key!='removed_metadata' ORDER BY scope,check_key",
    )
    .await?;
    mutate(&c,"INSERT INTO series(id,tvdb_id,title,path,status,monitored)VALUES(1,101,'TV','/fixture/tv','deleted',0)",1,0).await?;
    mutate(
        &c,
        "UPDATE series SET title=title,status=status,tvdb_id=tvdb_id WHERE id=1",
        0,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE series SET monitored=1,path='/fixture/tv-moved',year=2026 WHERE id=1",
        0,
        0,
    )
    .await?;
    mutate(&c, "UPDATE series SET title='Renamed TV' WHERE id=1", 1, 0).await?;
    mutate(&c, "UPDATE series SET tvdb_id=102 WHERE id=1", 1, 0).await?;
    mutate(&c, "UPDATE series SET status='ended' WHERE id=1", 1, 0).await?;
    mutate(&c, "UPDATE series SET title='Healthy TV' WHERE id=1", 0, 0).await?;
    mutate(&c, "UPDATE series SET status='deleted' WHERE id=1", 1, 0).await?;
    mutate(
        &c,
        "INSERT INTO movie_metadata(id,tmdb_id,title,status)VALUES(101,201,'Movie','deleted')",
        0,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET title='Orphan rename',tmdb_id=202 WHERE id=101",
        0,
        0,
    )
    .await?;
    // Coincident native series/movie IDs do not confuse domain, and catalog identity differs.
    mutate(
        &c,
        "INSERT INTO movies(id,metadata_id,path,monitored)VALUES(1,101,'/fixture/movie',0)",
        0,
        1,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET status=status,title=title,tmdb_id=tmdb_id WHERE id=101",
        0,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movies SET monitored=1,path='/fixture/movie-moved' WHERE id=1",
        0,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET title='Owned rename' WHERE id=101",
        0,
        1,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET tmdb_id=203 WHERE id=101",
        0,
        1,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET status='released' WHERE id=101",
        0,
        1,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET title='Healthy Movie' WHERE id=101",
        0,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET status='deleted' WHERE id=101",
        0,
        1,
    )
    .await?;
    mutate(&c,"INSERT INTO movie_metadata(id,tmdb_id,title,status)VALUES(102,204,'Healthy catalog','released')",0,0).await?;
    mutate(&c, "UPDATE movies SET metadata_id=102 WHERE id=1", 0, 1).await?;
    mutate(&c, "UPDATE movies SET metadata_id=102 WHERE id=1", 0, 0).await?;
    mutate(&c, "UPDATE movies SET metadata_id=101 WHERE id=1", 0, 1).await?;
    mutate(&c, "UPDATE movies SET id=2 WHERE id=1", 0, 1).await?;
    mutate(&c, "UPDATE series SET id=2 WHERE id=1", 1, 0).await?;
    // Existing observation remains a visible stale result; no mutation publishes OK.
    c.execute("UPDATE health_checks SET observed_generation=generation,observed_epoch='00000000-0000-0000-0000-000000000000',checked_at=1,severity=3,reason='previous',message='Previous observation',wiki_url='https://example.invalid/health',pending_reasons=3,due_at=1 WHERE check_key='removed_metadata'",()).await?;
    mutate(
        &c,
        "UPDATE series SET title='Changed after observation' WHERE id=2",
        1,
        0,
    )
    .await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET title='Changed after observation' WHERE id=101",
        0,
        1,
    )
    .await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND generation=observed_generation+1 AND due_at=1 AND pending_reasons=11 AND severity=3 AND message='Previous observation'").await?,2);
    let before = health(&c).await?;
    let library_before=rows(&c,"SELECT id,title,status FROM series UNION ALL SELECT id,title,status FROM movie_metadata ORDER BY id").await?;
    let tx = c.transaction().await?;
    tx.execute("UPDATE series SET status='ended' WHERE id=2", ())
        .await?;
    tx.execute(
        "UPDATE movie_metadata SET status='released' WHERE id=101",
        (),
    )
    .await?;
    assert!(
        tx.execute(
            "INSERT INTO movies(id,metadata_id,path)VALUES(3,999,'/fixture/missing')",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(health(&c).await?, before);
    assert_eq!(rows(&c,"SELECT id,title,status FROM series UNION ALL SELECT id,title,status FROM movie_metadata ORDER BY id").await?,library_before);
    mutate(&c, "DELETE FROM movies WHERE id=2", 0, 1).await?;
    mutate(
        &c,
        "UPDATE movie_metadata SET title='Now orphan again' WHERE id=101",
        0,
        0,
    )
    .await?;
    mutate(&c, "DELETE FROM series WHERE id=2", 1, 0).await?;
    mutate(
        &c,
        "INSERT INTO series(id,title,path,status)VALUES(3,'Not removed','/fixture/other',NULL)",
        0,
        0,
    )
    .await?;
    mutate(&c, "DELETE FROM series WHERE id=3", 0, 0).await?;
    assert_eq!(rows(&c,"SELECT * FROM health_checks WHERE check_key!='removed_metadata' ORDER BY scope,check_key").await?,siblings);
    integrity(&c).await?;
    Ok(())
}
#[tokio::test]
async fn removed_health_exhaustion_and_missing_registry_abort_source_writes() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("library.db")).await?;
    let c = db.connect().await?;
    for scope in ["tv", "movies"] {
        // No command membership exists: install an exhausted but otherwise valid registry row.
        c.execute(
            "DELETE FROM health_checks WHERE scope=? AND check_key='removed_metadata'",
            [scope],
        )
        .await?;
        c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,compatibility_type)VALUES(?,'removed_metadata',1,1,9007199254740991,'Fixture')",[scope]).await?;
    }
    c.execute(
        "INSERT INTO series(id,title,path,status)VALUES(1,'Kept TV','/fixture/tv','ended')",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO movie_metadata(id,title,status)VALUES(101,'Kept Movie','released')",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO movies(id,metadata_id,path)VALUES(1,101,'/fixture/movie')",
        (),
    )
    .await?;
    let before = health(&c).await?;
    for sql in [
        "UPDATE series SET status='deleted',title='Lost TV' WHERE id=1",
        "UPDATE movie_metadata SET status='deleted',title='Lost Movie' WHERE id=101",
    ] {
        let error = c.execute(sql, ()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("removed metadata health generation exhausted")
        );
        assert_eq!(health(&c).await?, before);
    }
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE title='Kept TV' AND status='ended'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_metadata WHERE title='Kept Movie' AND status='released'"
        )
        .await?,
        1
    );
    c.execute(
        "DELETE FROM health_checks WHERE check_key='removed_metadata'",
        (),
    )
    .await?;
    for sql in [
        "UPDATE series SET status='deleted' WHERE id=1",
        "UPDATE movie_metadata SET status='deleted' WHERE id=101",
    ] {
        let error = c.execute(sql, ()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("removed metadata health registry is missing")
        );
    }
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM series WHERE status='ended'").await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_metadata WHERE status='released'"
        )
        .await?,
        1
    );
    integrity(&c).await?;
    Ok(())
}
async fn predecessor(path: &Path) -> Result<(), Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    // Fixed accepted45 predecessor, not a moving latest-minus-one fixture.
    for (i, (name, sql)) in MIGRATIONS.iter().take(45).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path,status)VALUES(1,'Deleted TV','/fixture/tv','deleted');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/fixture/tv/shared.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(1,1,1,1,'One',1),(2,1,1,2,'Two',1);INSERT INTO movie_metadata(id,title,status)VALUES(101,'Deleted Movie','deleted');INSERT INTO movies(id,metadata_id,path)VALUES(1,101,'/fixture/movie');INSERT INTO movie_files(id,movie_id,path,edition)VALUES(1,1,'/fixture/movie/file.mkv','Extended');INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr','owned',233);UPDATE health_checks SET generation=1,pending_reasons=8,due_at=100 WHERE scope='tv';").await?;
    // Preserve a real captured attempt and prior observation, not merely empty health tables.
    let command = uuid::Uuid::new_v4().to_string();
    c.execute("UPDATE health_checks SET observed_generation=1,observed_epoch=(SELECT epoch FROM health_lifecycle WHERE id=1),checked_at=90,severity=3,reason='previous',message='Previous issue',wiki_url='https://example.invalid/health' WHERE scope='tv' AND check_key='completed_download_handling'", ()).await?;
    c.execute("INSERT INTO health_commands(id,epoch,next_attempt_at,created_at) SELECT ?,epoch,100,100 FROM health_lifecycle WHERE id=1", [command.clone()]).await?;
    c.execute("INSERT INTO health_command_checks(command_id,scope,check_key,admitted_generation) SELECT ?,scope,check_key,generation FROM health_checks WHERE scope='tv'", [command.clone()]).await?;
    c.execute(
        "UPDATE health_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [command.clone()],
    )
    .await?;
    c.execute("UPDATE health_command_checks SET captured_generation=1,captured_reasons=8,captured_due_at=100 WHERE command_id=?", [command.clone()]).await?;
    c.execute("INSERT INTO health_transitions(event_id,epoch,command_id,command_attempt,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type) SELECT ?,epoch,?,1,'tv','completed_download_handling','issue',0,90,3,'previous','Previous issue','https://example.invalid/health','ImportMechanismCheck' FROM health_lifecycle WHERE id=1",params![uuid::Uuid::new_v4().to_string(),command]).await?;
    drop(c);
    drop(db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
async fn witness(c: &Connection) -> Result<Vec<Vec<Vec<Value>>>, Error> {
    let mut out = vec![];
    for table in [
        "series",
        "seasons",
        "episodes",
        "episode_files",
        "movie_metadata",
        "movies",
        "movie_files",
        "snapshot_imports",
        "health_lifecycle",
        "health_commands",
        "health_command_checks",
        "health_transitions",
    ] {
        out.push(rows(c, &format!("SELECT * FROM {table} ORDER BY 1")).await?);
    }
    out.push(rows(c,"SELECT * FROM health_checks WHERE check_key!='removed_metadata' ORDER BY scope,check_key").await?);
    out.push(
        rows(
            c,
            "SELECT * FROM schema_migrations WHERE version<=45 ORDER BY version",
        )
        .await?,
    );
    Ok(out)
}
#[tokio::test]
async fn removed_health_real45_upgrade_backup_late_failure_reopen() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("library.db");
    predecessor(&path).await?;
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    let before = witness(&c).await?;
    c.execute_batch("CREATE TRIGGER fail_removed_health BEFORE INSERT ON schema_migrations WHEN NEW.version=46 BEGIN SELECT RAISE(ABORT,'owned late publication failure'); END;").await?;
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await?;
    drop(c);
    drop(raw);
    assert!(Database::open_local(&path).await.is_err());
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(version(&c).await?, 45);
    assert_eq!(witness(&c).await?, before);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await?,
        schema
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM health_checks WHERE check_key='removed_metadata'"
        )
        .await?,
        0
    );
    integrity(&c).await?;
    c.execute("DROP TRIGGER fail_removed_health", ()).await?;
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let backup = db.migration_backup().expect("predecessor backup directory");
    let copy = libsql::Builder::new_local(backup.join("database.db"))
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await?;
    let bc = copy.connect()?;
    assert_eq!(version(&bc).await?, 45);
    assert_eq!(witness(&bc).await?, before);
    integrity(&bc).await?;
    drop(bc);
    drop(copy);
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 47); // Reasoning: latest is now 0047 movie credits.
    assert_eq!(witness(&c).await?, before);
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND generation=0 AND observed_generation IS NULL AND severity IS NULL AND pending_reasons=0").await?,2);
    integrity(&c).await?;
    let complete = health(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(witness(&c).await?, before);
    assert_eq!(health(&c).await?, complete);
    integrity(&c).await?;
    // Normal lifecycle discovers existing removed owners; migration never asserts success.
    // Health errors intentionally do not convert into the database test error type.
    assert!(
        crate::health::startup(&db).await.is_ok(),
        "startup must succeed after the preserved-state upgrade"
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE check_key='removed_metadata' AND generation=1 AND pending_reasons=1 AND observed_generation IS NULL AND severity IS NULL").await?,2);
    Ok(())
}
