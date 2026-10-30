use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn review_integrity_upgrade_preserves_rows_rolls_back_and_reopens() -> Result<(), Error> {
    let files = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-review-integrity-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    // The historical prefix stays at 75; only the new migration may change this populated DB.
    for (i, (name, sql)) in MIGRATIONS.iter().take(75).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name) VALUES(900,'movies','Collection policy');
        INSERT INTO movie_collections(id,title,quality_profile_id) VALUES(900,'Collection',900);
        INSERT INTO notifications(id,name,implementation,settings_json,created_at,updated_at) VALUES('00000000-0000-4000-8000-000000000001','Retained','webhook','{}',1,1);
        INSERT INTO notification_events(sequence,kind,scope,subject_id,payload_json,created_at) VALUES(1,'add','movies',900,'{}',1);
        INSERT INTO notification_deliveries(event_sequence,notification_id,media_type,attempts,next_attempt_at,lease_until,sent_parts,created_at) VALUES(1,'00000000-0000-4000-8000-000000000001','movies',2,10,20,'[\"acknowledged\"]',1);").await?;
    // Fail at the final ALTER, after the guards were created: the runner must roll them back too.
    c.execute(
        "ALTER TABLE notification_deliveries ADD COLUMN lease_owner TEXT",
        (),
    )
    .await?;
    assert!(Database::open_local(&path).await.is_err());
    assert_eq!(version(&c).await?, 75);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name IN ('collection_profile_delete','collection_profile_identity_update','directory_client_category_insert','directory_client_category_update')"
        )
        .await?,
        0
    );
    c.execute(
        "ALTER TABLE notification_deliveries DROP COLUMN lease_owner",
        (),
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT quality_profile_id FROM movie_collections WHERE id=900"
        )
        .await?,
        900
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM notification_deliveries WHERE attempts=2 AND lease_until=20 AND sent_parts='[\"acknowledged\"]'").await?, 1);
    for implementation in ["aria2", "downloadstation"] {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1/')",params![id.clone(),implementation,implementation]).await?;
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,?,'tv','.',0,0,'started')",params![id.clone(),implementation]).await?;
        for category in [".", ".."] {
            c.execute(
                "UPDATE provider_scopes SET category=? WHERE provider_id=?",
                params![category, id.clone()],
            )
            .await?;
            let error = Database::open_local(&path)
                .await
                .err()
                .expect("unsafe legacy category must fail upgrade");
            assert!(
                error
                    .to_string()
                    .contains("invalid directory client category")
            );
            assert_eq!(version(&c).await?, 75);
        }
        // Repair on the unchanged old schema, then the normal upgrade may proceed.
        c.execute(
            "UPDATE provider_scopes SET category='tv' WHERE provider_id=?",
            [id],
        )
        .await?;
    }
    drop(c);
    drop(raw);

    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 76);
    assert_eq!(scalar(&c, "SELECT count(*) FROM notification_deliveries WHERE attempts=2 AND next_attempt_at=10 AND lease_until=20 AND sent_parts='[\"acknowledged\"]' AND lease_owner IS NULL").await?, 1);
    assert!(
        c.execute("DELETE FROM quality_profiles WHERE id=900", ())
            .await
            .is_err()
    );
    assert!(
        c.execute("UPDATE quality_profiles SET id=901 WHERE id=900", ())
            .await
            .is_err()
    );
    assert!(
        c.execute(
            "UPDATE quality_profiles SET media_type='tv' WHERE id=900",
            ()
        )
        .await
        .is_err()
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT quality_profile_id FROM movie_collections WHERE id=900"
        )
        .await?,
        900
    );
    c.execute(
        "UPDATE quality_profiles SET name='Renamed policy' WHERE id=900",
        (),
    )
    .await?;
    c.execute(
        "UPDATE movie_collections SET quality_profile_id=NULL WHERE id=900",
        (),
    )
    .await?;
    c.execute("DELETE FROM quality_profiles WHERE id=900", ())
        .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(version(&reopened.connect().await?).await?, 76);
    Ok(())
}
