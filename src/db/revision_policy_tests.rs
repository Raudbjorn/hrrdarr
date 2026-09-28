use super::*;
#[tokio::test]
async fn revision_policy_upgrade37_rollback_and_reopen_preserves_facts() -> Result<(), Error> {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let s = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-revision-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&s.0)?;
    let path = s.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(37).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/synthetic');INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/synthetic/file');INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',1,1,'{\"version\":3,\"real\":1,\"is_repack\":true}');").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[37].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[37].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 37);
    assert_eq!(
        c.query(
            "SELECT count(*) FROM sqlite_schema WHERE name='revision_policies'",
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
    assert_eq!(version(&c).await?, 41); // Latest open includes release profiles and CDH intent; historical starting schema is unchanged.
    assert_eq!(
        c.query("SELECT revision_json FROM file_metadata", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "{\"version\":3,\"real\":1,\"is_repack\":true}"
    );
    assert_eq!(c.query("SELECT count(*) FROM revision_policies WHERE mode='prefer_and_upgrade' AND revision=1 AND locally_edited=0",()).await?.next().await?.unwrap().get::<i64>(0)?,2);
    for sql in [
        "UPDATE revision_policies SET mode='unknown'",
        "UPDATE revision_policies SET revision=0",
        "UPDATE revision_policies SET locally_edited=2",
    ] {
        assert!(c.execute(sql, ()).await.is_err());
    }
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert_eq!(version(&reopened.connect().await?).await?, 41); // Latest open includes release profiles and CDH intent; historical starting schema is unchanged.
    Ok(())
}
