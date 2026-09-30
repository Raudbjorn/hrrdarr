use super::*;
#[tokio::test]
async fn release_profile_upgrade39_rollback_constraints_and_reopen() -> Result<(), Error> {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-release-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0)?;
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(39).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr','kept',233)",()).await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[39].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[39].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 39);
    assert_eq!(
        c.query(
            "SELECT count(*) FROM sqlite_schema WHERE name='release_profiles'",
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
    assert_eq!(version(&c).await?, 43); // Latest schema43 includes communication storage; historical starting version is unchanged.
    assert_eq!(
        c.query(
            "SELECT release_profile_version FROM snapshot_imports WHERE fingerprint='kept'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    c.execute_batch("INSERT INTO tags(id,media_type,label)VALUES(1,'tv','tv'),(2,'movies','movie');INSERT INTO release_profiles(id,media_type,enabled,required_json,ignored_json,air_date_restriction,air_date_grace_period_days,allow_season_pack_without_all_episodes_aired)VALUES(1,'tv',0,'[\"WEB\"]','[]',0,0,0);INSERT INTO release_profiles(id,media_type,enabled,required_json,ignored_json)VALUES(2,'movies',1,'[\"WEB\"]','[]');").await?;
    for sql in [
        "INSERT INTO release_profile_tags VALUES(1,'tv',2,'include')",
        "INSERT INTO release_profile_tags VALUES(2,'movies',2,'exclude')",
        "UPDATE release_profiles SET media_type='movies' WHERE id=1",
        "UPDATE release_profiles SET air_date_restriction=1 WHERE id=2",
        "UPDATE release_profiles SET required_json='[]' WHERE id=2",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    let fingerprint = "a".repeat(64);
    c.execute(
        "INSERT INTO release_profile_indexers VALUES(1,'tv',?,NULL,'sonarr',?,7)",
        params![format!("source:sonarr:{fingerprint}:7"), fingerprint],
    )
    .await?;
    assert!(
        c.execute("UPDATE release_profiles SET enabled=1 WHERE id=1", ())
            .await
            .is_err()
    );
    c.execute(
        "INSERT INTO release_profile_tags VALUES(1,'tv',1,'include')",
        (),
    )
    .await?;
    assert!(c.execute("DELETE FROM tags WHERE id=1", ()).await.is_err());
    c.execute("DELETE FROM release_profiles WHERE id=1", ())
        .await?;
    c.execute("DELETE FROM tags WHERE id=1", ()).await?;
    drop(c);
    drop(db);
    assert_eq!(
        version(&Database::open_local(&path).await?.connect().await?).await?,
        43 // Latest schema43 includes communication storage; historical starting version is unchanged.
    );
    Ok(())
}
