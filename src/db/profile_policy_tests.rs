use super::*;
struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[tokio::test]
async fn profile_policy_upgrade_rollback_constraints_and_reopen() -> Result<(), Error> {
    let files = Sandbox(
        std::env::temp_dir().join(format!("hrrdarr-profile-policy-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let conn = raw.connect()?;
    conn.execute("PRAGMA foreign_keys=ON", ()).await?;
    conn.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(18).enumerate() {
        conn.execute_batch(sql).await?;
        conn.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    conn.execute_batch("INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'tv','TV'),(2,'movies','Movie');
 INSERT INTO quality_profile_groups(id,profile_id,name,position,allowed) VALUES(100,1,'Grouped',1,1);
 INSERT INTO quality_profile_items(profile_id,media_type,quality_id,group_id,position,allowed) VALUES(1,'tv',1,NULL,0,1),(1,'tv',7,100,0,1),(2,'movies',1,NULL,0,1);
 INSERT INTO series(id,title,path) VALUES(1,'TV','/tv');
 INSERT INTO library_settings(media_type,series_id,quality_profile_id) VALUES('tv',1,1);").await?;
    let tx = conn.transaction().await?;
    tx.execute_batch(MIGRATIONS[18].1).await?;
    tx.execute(
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,NULL,100,-1,0,1,NULL)",
        (),
    )
    .await?;
    assert!(tx.execute_batch(MIGRATIONS[18].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&conn).await?, 18);
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='quality_profile_policies'"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT quality_profile_id FROM library_settings").await?,
        1
    );
    drop(conn);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let conn = db.connect().await?;
    assert_eq!(version(&conn).await?, 46); // Latest adds removed metadata health46; historical migration prefixes stay fixed.
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM quality_profile_policies").await?,
        0,
        "legacy policy remains unknown, no guessed defaults"
    );
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM quality_profile_items").await?,
        3
    );
    assert_eq!(
        scalar(&conn, "SELECT quality_profile_id FROM library_settings").await?,
        1
    );
    for sql in [
        "INSERT INTO quality_profile_policies VALUES(1,'movies',1,1,NULL,0,0,1,1)",
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,7,NULL,0,0,1,NULL)",
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,1,100,0,0,1,NULL)",
        // Migration 0034 permits positive custom-format minimums; retain the i32 bound.
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,1,NULL,2147483648,0,1,NULL)",
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,1,NULL,0,0,0,NULL)",
        "INSERT INTO quality_profile_policies VALUES(1,'tv',1,1,NULL,0,0,1,1)",
        "INSERT INTO quality_profile_policies VALUES(2,'movies',1,1,NULL,0,0,1,NULL)",
        "INSERT INTO quality_profile_policies VALUES(2,'movies',1,1,NULL,0,0,1,58)",
    ] {
        assert!(conn.execute(sql, ()).await.is_err(), "{sql}");
    }
    conn.execute("INSERT INTO quality_profile_policies VALUES(1,'tv',0,NULL,100,-2147483648,2147483647,2147483647,NULL)",()).await?;
    conn.execute(
        "INSERT INTO quality_profile_policies VALUES(2,'movies',1,1,NULL,0,-2147483648,1,-2)",
        (),
    )
    .await?;
    assert!(
        conn.execute(
            "UPDATE quality_profile_groups SET allowed=0 WHERE id=100",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE quality_profile_items SET allowed=0 WHERE profile_id=2",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        conn.execute("DELETE FROM quality_profile_groups WHERE id=100", ())
            .await
            .is_err()
    );
    assert!(conn.execute("UPDATE quality_profile_policies SET cutoff_group_id=NULL,cutoff_quality_id=7 WHERE profile_id=1",()).await.is_err());
    integrity(&conn).await?;
    drop(conn);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let conn = db.connect().await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM quality_profile_policies").await?,
        2
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT cutoff_group_id FROM quality_profile_policies WHERE profile_id=1"
        )
        .await?,
        100
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT language_id FROM quality_profile_policies WHERE profile_id=2"
        )
        .await?,
        -2
    );
    assert_eq!(
        scalar(&conn, "SELECT quality_profile_id FROM library_settings").await?,
        1
    );
    integrity(&conn).await?;
    Ok(())
}
