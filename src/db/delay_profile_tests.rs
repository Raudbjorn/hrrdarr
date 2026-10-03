use super::*;
#[tokio::test]
async fn delay_upgrade38_rollback_reopen_preserves_legacy_facts() -> Result<(), Error> {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let s = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-delay-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&s.0)?;
    let path = s.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(38).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/synthetic');INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/synthetic/file');INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',1,1,'{\"version\":3,\"real\":1,\"is_repack\":true}');").await?;
    c.execute(
        "INSERT INTO release_delay_policies VALUES('tv',123,456,0)",
        (),
    )
    .await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[38].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[38].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 38);
    assert_eq!(
        c.query(
            "SELECT count(*) FROM sqlite_schema WHERE name='delay_profiles'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 48); // Reasoning: latest migration is now 0048 indexer operation policy (was 47: movie credits); historical migration prefixes stay fixed.
    assert_eq!(
        c.query("SELECT revision_json FROM file_metadata", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "{\"version\":3,\"real\":1,\"is_repack\":true}"
    );
    assert_eq!(c.query("SELECT count(*) FROM delay_profiles WHERE semantics='legacy_age_only' AND torrent_delay_minutes IS NULL AND preferred_protocol IS NULL",()).await?.next().await?.unwrap().get::<i64>(0)?,2);
    assert_eq!(
        c.query(
            "SELECT torrent_delay_minutes FROM release_delay_policies WHERE media_type='tv'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        123
    );
    assert_eq!(
        c.query(
            "SELECT count(*) FROM release_delay_policies WHERE media_type='movies'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    for sql in [
        "DELETE FROM delay_profiles WHERE id=1",
        "UPDATE delay_profiles SET media_type='movies' WHERE id=1",
        "UPDATE delay_profiles SET torrent_delay_minutes=1 WHERE id=1",
    ] {
        assert!(c.execute(sql, ()).await.is_err());
    }
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert_eq!(version(&reopened.connect().await?).await?, 48); // Reasoning: latest migration is now 0048 indexer operation policy (was 47: movie credits); historical migration prefixes stay fixed.
    Ok(())
}
