use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-backoff-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn rows(c: &Connection, sql: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut r = c.query(sql, ()).await?;
    let mut result = vec![];
    while let Some(row) = r.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<_, _>>()?,
        )
    }
    Ok(result)
}
async fn seed43(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(43).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    Ok((db, c))
}
async fn status(c: &Connection, p: &str, media: &str) -> Result<(), libsql::Error> {
    c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level,initial_failure_at,last_failure_at,disabled_until)VALUES(?,?,1,2,100,200,500)",params![p,media]).await?;
    Ok(())
}
#[tokio::test]
async fn backoff_upgrade43_rollback_reopen_preserves_authority() -> Result<(), Error> {
    let s = Scratch::new();
    let path = s.0.join("db");
    let (raw, c) = seed43(&path).await?;
    let p = super::refresh_tests::provider(&c).await?;
    super::refresh_tests::enqueue(&c, &p, "tv").await?;
    c.execute("UPDATE health_checks SET next_expiry_at=NULL", ())
        .await
        .expect_err("expiry absent before migration44");
    // Migration43 already creates exactly one sequence row, including for an empty ring.
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_sequence WHERE name='health_transitions'"
        )
        .await?,
        1
    );
    c.execute(
        "UPDATE sqlite_sequence SET seq=7000 WHERE name='health_transitions'",
        (),
    )
    .await?;
    let queries = [
        "SELECT * FROM providers ORDER BY id",
        "SELECT * FROM provider_scopes ORDER BY provider_id,media_type",
        "SELECT * FROM commands ORDER BY id",
        "SELECT * FROM health_checks ORDER BY scope,check_key",
        "SELECT * FROM health_lifecycle",
        "SELECT * FROM health_transitions ORDER BY sequence",
        "SELECT * FROM sqlite_sequence ORDER BY name,seq",
        "SELECT * FROM schema_migrations ORDER BY version",
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    ];
    let mut before = vec![];
    for q in queries {
        before.push(rows(&c, q).await?);
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[43].1).await?;
    assert!(
        tx.execute(
            "UPDATE health_checks SET next_expiry_at=-1 WHERE check_key='download_client_backoff'",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    for (q, expected) in queries.iter().zip(&before) {
        assert_eq!(&rows(&c, q).await?, expected, "rollback: {q}");
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 44);
    for i in [0, 1, 4, 5, 6] {
        assert_eq!(rows(&c, queries[i]).await?, before[i]);
    }
    let mut commands = before[2].clone();
    for row in &mut commands {
        row.extend([
            libsql::Value::Text("legacy_unknown".into()),
            libsql::Value::Integer(0),
        ]);
    }
    assert_eq!(rows(&c, queries[2]).await?, commands);
    let mut checks = before[3].clone();
    for row in &mut checks {
        row.push(libsql::Value::Null);
    }
    assert_eq!(rows(&c,"SELECT * FROM health_checks WHERE check_key!='download_client_backoff' ORDER BY scope,check_key").await?,checks);
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE check_key='download_client_backoff' AND scope IN ('tv','movies') AND startup=1 AND scheduled=1 AND compatibility_type='DownloadClientStatusCheck' AND generation=0 AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL AND severity IS NULL AND last_error IS NULL AND due_at IS NULL AND next_expiry_at IS NULL AND pending_reasons=0").await?,2);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_client_status").await?,
        0
    );
    let backup = libsql::Builder::new_local(db.migration_backup().unwrap().join("database.db"))
        .build()
        .await?;
    assert_eq!(version(&backup.connect()?).await?, 43);
    drop(backup);
    integrity(&c).await?;
    let schema = rows(&c, queries[8]).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert!(db.migration_backup().is_none());
    assert_eq!(rows(&c, queries[8]).await?, schema);
    assert_eq!(rows(&c, queries[2]).await?, commands);
    Ok(())
}
#[tokio::test]
async fn backoff_status_constraints_revision_scope_retention_and_expiry() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    let p = super::refresh_tests::provider(&c).await?;
    status(&c, &p, "tv").await?;
    status(&c, &p, "movies").await?;
    assert!(status(&c, &p, "tv").await.is_err());
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    assert!(status(&c, &indexer, "tv").await.is_err());
    assert!(
        status(&c, &uuid::Uuid::new_v4().to_string(), "tv")
            .await
            .is_err()
    );
    // A block expired at a deterministic policy clock remains valid retained history.
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_client_status WHERE disabled_until<=1000"
        )
        .await?,
        2
    );
    for assignment in [
        "config_revision=0",
        "config_revision=2",
        "escalation_level=-1",
        "escalation_level=6",
        "escalation_level=1.5",
        "initial_failure_at=NULL",
        "initial_failure_at=1.5",
        "initial_failure_at=9007199254740992",
        "last_failure_at=1.5",
        "initial_failure_at=NULL,last_failure_at=NULL,disabled_until=NULL",
        "disabled_until=9007199254740992",
        "provider_id='00000000-0000-0000-0000-000000000000'",
        "last_failure_at=99",
        "last_failure_at=9007199254740992",
        "disabled_until=-1",
        "disabled_until=1.5",
        "escalation_level=0",
        "media_type='invalid'",
    ] {
        assert!(
            c.execute(
                &format!("UPDATE download_client_status SET {assignment} WHERE media_type='tv'"),
                ()
            )
            .await
            .is_err(),
            "{assignment}"
        );
    }
    c.execute("UPDATE download_client_status SET escalation_level=0,disabled_until=NULL WHERE media_type='tv'",()).await?;
    c.execute("UPDATE download_client_status SET escalation_level=1,initial_failure_at=0,last_failure_at=9007199254740991,disabled_until=9007199254740991 WHERE media_type='tv'",()).await?;
    c.execute("UPDATE download_client_status SET escalation_level=0,initial_failure_at=NULL,last_failure_at=NULL,disabled_until=NULL WHERE media_type='tv'",()).await?;
    let retained = rows(
        &c,
        "SELECT * FROM download_client_status ORDER BY media_type",
    )
    .await?;
    // Model the actual delete/reinsert scope writer: history survives temporary absence.
    let tx = c.transaction().await?;
    let scopes = rows(
        &tx,
        "SELECT * FROM provider_scopes WHERE implementation='qbittorrent' ORDER BY media_type",
    )
    .await?;
    tx.execute(
        "UPDATE providers SET revision=revision+1,enabled=0 WHERE id=?",
        [p.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM provider_scopes WHERE provider_id=?",
        [p.clone()],
    )
    .await?;
    assert_eq!(
        rows(
            &tx,
            "SELECT * FROM download_client_status ORDER BY media_type"
        )
        .await?,
        retained
    );
    assert!(
        tx.execute("UPDATE download_client_status SET config_revision=2", ())
            .await
            .is_err()
    );
    for row in scopes {
        let placeholders = vec!["?"; row.len()].join(",");
        tx.execute(
            &format!("INSERT INTO provider_scopes VALUES({placeholders})"),
            row,
        )
        .await?;
    }
    tx.execute("UPDATE download_client_status SET config_revision=2", ())
        .await?;
    let mut rebound = retained.clone();
    for row in &mut rebound {
        row[2] = libsql::Value::Integer(2);
    }
    assert_eq!(
        rows(
            &tx,
            "SELECT * FROM download_client_status ORDER BY media_type"
        )
        .await?,
        rebound
    );
    tx.rollback().await?;
    assert_eq!(
        rows(
            &c,
            "SELECT * FROM download_client_status ORDER BY media_type"
        )
        .await?,
        retained
    );
    // Committed disable/re-enable retains every history field; only revision ownership changes.
    for (revision, enabled) in [(2, 0), (3, 1)] {
        let tx = c.transaction().await?;
        tx.execute(
            "UPDATE providers SET revision=?,enabled=? WHERE id=?",
            params![revision, enabled, p.clone()],
        )
        .await?;
        tx.execute(
            "UPDATE download_client_status SET config_revision=? WHERE provider_id=?",
            params![revision, p.clone()],
        )
        .await?;
        tx.commit().await?;
        let mut expected = retained.clone();
        for row in &mut expected {
            row[2] = libsql::Value::Integer(revision);
        }
        assert_eq!(
            rows(
                &c,
                "SELECT * FROM download_client_status ORDER BY media_type"
            )
            .await?,
            expected
        );
    }
    let removed_scope = rows(
        &c,
        "SELECT * FROM provider_scopes WHERE implementation='qbittorrent' AND media_type='movies'",
    )
    .await?
    .remove(0);
    // Removing a final scope requires explicit same-transaction reconciliation, not cascade.
    let tx = c.transaction().await?;
    tx.execute("DELETE FROM provider_scopes WHERE media_type='movies'", ())
        .await?;
    tx.execute("DELETE FROM download_client_status WHERE NOT EXISTS(SELECT 1 FROM provider_scopes s WHERE s.provider_id=download_client_status.provider_id AND s.media_type=download_client_status.media_type)",()).await?;
    tx.commit().await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_client_status").await?,
        1
    );
    assert!(c.execute("INSERT INTO download_client_status(provider_id,media_type,config_revision,escalation_level)VALUES(?,'movies',3,0)",[p.clone()]).await.is_err());
    let placeholders = vec!["?"; removed_scope.len()].join(",");
    c.execute(
        &format!("INSERT INTO provider_scopes VALUES({placeholders})"),
        removed_scope,
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM download_client_status WHERE media_type='movies'"
        )
        .await?,
        0,
        "readded scope has no fabricated history"
    );

    for value in [-1_i64, 9007199254740992] {
        assert!(c.execute("UPDATE health_checks SET next_expiry_at=? WHERE check_key='download_client_backoff'",[value]).await.is_err());
    }
    assert!(c.execute("UPDATE health_checks SET next_expiry_at=1 WHERE check_key='completed_download_handling'",()).await.is_err());
    c.execute("UPDATE health_checks SET next_expiry_at=9007199254740991 WHERE check_key='download_client_backoff'",()).await?;
    c.execute(
        "UPDATE health_checks SET next_expiry_at=NULL WHERE check_key='download_client_backoff'",
        (),
    )
    .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM health_checks WHERE pending_reasons!=0 OR due_at IS NOT NULL"
        )
        .await?,
        0
    );
    c.execute("DELETE FROM providers WHERE id=?", [p]).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_client_status").await?,
        0
    );
    integrity(&c).await?;
    Ok(())
}
#[tokio::test]
async fn backoff_origin_bypass_is_metadata_only_and_preserves_claim_guards() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    let p = super::refresh_tests::provider(&c).await?;
    for phase in ["queued", "running", "retry_wait"] {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at,origin)VALUES(?,?,'tv',1,100,100,'automatic')",params![id.clone(),p.clone()]).await?;
        if phase != "queued" {
            c.execute(
                "UPDATE commands SET status='running',attempts=1,started_at=100 WHERE id=?",
                [id.clone()],
            )
            .await?;
        }
        if phase == "retry_wait" {
            c.execute("UPDATE commands SET status='retry_wait',error_code='refresh_failed',next_attempt_at=200 WHERE id=?",[id.clone()]).await?;
        }
        for assignment in [
            "origin='manual'",
            "manual_bypass=1,next_attempt_at=300",
            "manual_bypass=1,error_code='refresh_timeout'",
            "manual_bypass=1,items_observed=1",
            "manual_bypass=1,attempts=2",
            "manual_bypass=1,created_at=101",
            "manual_bypass=1,status='cancelled',completed_at=300,error_code=NULL",
        ] {
            assert!(
                c.execute(
                    &format!("UPDATE commands SET {assignment} WHERE id=?"),
                    [id.clone()]
                )
                .await
                .is_err(),
                "{phase}: {assignment}"
            );
        }
        let before = rows(&c, "SELECT * FROM commands ORDER BY id").await?;
        c.execute(
            "UPDATE commands SET manual_bypass=1 WHERE id=?",
            [id.clone()],
        )
        .await?;
        let after = rows(&c, "SELECT * FROM commands ORDER BY id").await?;
        let mut expected = before;
        *expected[0].last_mut().unwrap() = libsql::Value::Integer(1);
        assert_eq!(after, expected);
        assert!(
            c.execute(
                "UPDATE commands SET manual_bypass=0 WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        c.execute(
            "UPDATE commands SET status='cancelled',error_code=NULL,completed_at=300 WHERE id=?",
            [id.clone()],
        )
        .await?;
        assert!(
            c.execute(
                "UPDATE commands SET manual_bypass=1 WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        c.execute("DELETE FROM commands WHERE id=?", [id]).await?;
    }
    // Stale automatic metadata promotion is harmless; it cannot make a stale claim legal.
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at,origin)VALUES(?,?,'tv',1,100,100,'automatic')",params![id.clone(),p.clone()]).await?;
    c.execute("UPDATE providers SET revision=2 WHERE id=?", [p.clone()])
        .await?;
    c.execute(
        "UPDATE commands SET manual_bypass=1 WHERE id=?",
        [id.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE commands SET status='running',attempts=1,started_at=100 WHERE id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE commands SET status='cancelled',completed_at=300 WHERE id=?",
        [id],
    )
    .await?;
    // Running stale rows can receive only the bypass metadata, not a stale observation update.
    let running = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at,origin)VALUES(?,?,'tv',2,100,100,'automatic')",params![running.clone(),p.clone()]).await?;
    c.execute(
        "UPDATE commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [running.clone()],
    )
    .await?;
    c.execute("UPDATE providers SET revision=3 WHERE id=?", [p.clone()])
        .await?;
    c.execute(
        "UPDATE commands SET manual_bypass=1 WHERE id=?",
        [running.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE commands SET next_attempt_at=101 WHERE id=?",
            [running.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE commands SET status='cancelled',completed_at=300 WHERE id=?",
        [running],
    )
    .await?;
    let terminal = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at,origin)VALUES(?,?,'tv',3,100,100,'automatic')",params![terminal.clone(),p]).await?;
    c.execute(
        "UPDATE commands SET status='cancelled',completed_at=300 WHERE id=?",
        [terminal.clone()],
    )
    .await?;
    assert!(
        c.execute("UPDATE commands SET manual_bypass=1 WHERE id=?", [terminal])
            .await
            .is_err()
    );
    integrity(&c).await?;
    Ok(())
}
