use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-health-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
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
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    // sqlite_sequence rowids may change during rebuild; its name/high-water pairs must not.
    let order = if table == "sqlite_sequence" {
        "name,seq"
    } else {
        "rowid"
    };
    let mut cursor = c
        .query(&format!("SELECT * FROM {table} ORDER BY {order}"), ())
        .await?;
    let mut values = vec![];
    while let Some(row) = cursor.next().await? {
        values.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok(values)
}
async fn enqueue(c: &Connection) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO health_commands(id,epoch,next_attempt_at,created_at)SELECT ?,epoch,100,100 FROM health_lifecycle WHERE id=1",[id.clone()]).await?;
    Ok(id)
}
async fn member(c: &Connection, id: &str, scope: &str) -> Result<(), libsql::Error> {
    c.execute("INSERT INTO health_command_checks(command_id,scope,check_key,admitted_generation)SELECT ?,scope,check_key,generation FROM health_checks WHERE scope=?",params![id,scope]).await?;
    Ok(())
}
async fn cancel(c: &Connection, id: &str) -> Result<(), libsql::Error> {
    c.execute(
        "UPDATE health_commands SET status='cancelled',completed_at=200 WHERE id=?",
        [id],
    )
    .await?;
    Ok(())
}
#[tokio::test]
async fn health_schema41_upgrade_rollback_preserves_rows_and_admission_predicates()
-> Result<(), Error> {
    let s = Scratch::new();
    let path = s.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(41).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let provider = super::refresh_tests::provider(&c).await?;
    let command = super::refresh_tests::enqueue(&c, &provider, "tv").await?;
    c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode,enabled_override)VALUES(?,'tv',1,1,1,'copy',1)",[provider.clone()]).await?;
    super::refresh_tests::schedule(&c, &provider, "tv").await?;
    c.execute("UPDATE completed_download_handling_settings SET defined=1,locally_edited=1,revision=2 WHERE media_type='tv'",()).await?;
    let tables = [
        "providers",
        "provider_scopes",
        "commands",
        "download_processing_policies",
        "download_refresh_schedules",
        "completed_download_handling_settings",
    ];
    let mut before = vec![];
    for table in tables {
        before.push(rows(&c, table).await?);
    }
    let mut triggers = vec![];
    let mut r=c.query("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%command capacity reached%' ORDER BY name",()).await?;
    while let Some(row) = r.next().await? {
        triggers.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    drop(r);
    assert_eq!(triggers.len(), 8);
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[41].1).await?;
    // Force a late failure after tables, seeds and every replacement trigger exist.
    assert!(tx.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('invalid','late',1,1,'fixture')",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 41);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'health_%'"
        )
        .await?,
        0
    );
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(&rows(&c, table).await?, expected);
    }
    for (name, sql) in &triggers {
        assert_eq!(
            c.query("SELECT sql FROM sqlite_schema WHERE name=?", [name.clone()])
                .await?
                .next()
                .await?
                .unwrap()
                .get::<String>(0)?,
            *sql
        );
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 43); // Reopen applies communication migration too.
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(&rows(&c, table).await?, expected);
    }
    // Exact removal of the one new count must recover every original predicate.
    let added =
        "+(SELECT count(*) FROM health_commands WHERE status IN ('queued','running','retry_wait'))";
    for (name, sql) in &triggers {
        let actual = c
            .query("SELECT sql FROM sqlite_schema WHERE name=?", [name.clone()])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?;
        assert_eq!(actual.replace(added, ""), *sql);
    }
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE severity IS NULL AND checked_at IS NULL AND observed_generation IS NULL AND pending_reasons=0 AND compatibility_type='ImportMechanismCheck'").await?,2);
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_lifecycle WHERE started_at=0 AND grace_due_at=0 AND next_scheduled_at=0 AND last_batch_completed_at IS NULL").await?,1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_commands").await?, 0);
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
    assert_eq!(
        c.query("SELECT status FROM commands WHERE id=?", [command])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "queued"
    );
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert_eq!(version(&db.connect().await?).await?, 43); // Current schema, not historical input.
    Ok(())
}
#[tokio::test]
async fn health_constraints_membership_capacity_and_bounded_diagnostics() -> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    for sql in [
        "DELETE FROM health_lifecycle",
        "UPDATE health_lifecycle SET grace_phase='expired'",
        "UPDATE health_lifecycle SET epoch='bad'",
        "UPDATE health_checks SET generation=-1",
        "UPDATE health_checks SET generation=9007199254740992",
        "UPDATE health_checks SET pending_reasons=32,due_at=1",
        "UPDATE health_checks SET pending_reasons=1",
        "UPDATE health_checks SET severity=0",
        "UPDATE health_checks SET message='cannot fake payload before observation'",
        "UPDATE health_checks SET checked_at=100",
        "UPDATE health_checks SET scope='system' WHERE scope='tv'",
        "UPDATE health_checks SET last_error='private arbitrary upstream body'",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    let epoch = uuid::Uuid::new_v4().to_string();
    c.execute("UPDATE health_lifecycle SET epoch=?,started_at=100,grace_due_at=1000,next_scheduled_at=21700",[epoch.clone()]).await?;
    c.execute(
        "UPDATE health_checks SET generation=1,pending_reasons=1,due_at=100",
        (),
    )
    .await?;
    let id = enqueue(&c).await?;
    member(&c, &id, "tv").await?;
    assert!(enqueue(&c).await.is_err(), "one active batch globally");
    assert!(
        c.execute("DELETE FROM health_commands WHERE id=?", [id.clone()])
            .await
            .is_err()
    );
    c.execute(
        "UPDATE health_checks SET generation=2,pending_reasons=9,due_at=100 WHERE scope='tv'",
        (),
    )
    .await?;
    // Exactly both TV members must retain generation1; an extra or changed member fails.
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_command_checks").await?,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM health_command_checks WHERE admitted_generation=1"
        )
        .await?,
        2, // Both TV identities retain admission generation1 despite later dirtying.
        "later dirty generation is not cancellation ownership"
    );
    c.execute(
        "UPDATE health_command_checks SET admitted_generation=2 WHERE command_id=?",
        [id.clone()],
    )
    .await?;
    assert!(c.execute("UPDATE health_command_checks SET captured_generation=2,captured_reasons=9,captured_due_at=100 WHERE command_id=?",[id.clone()]).await.is_err());
    c.execute(
        "UPDATE health_commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [id.clone()],
    )
    .await?;
    c.execute("UPDATE health_command_checks SET captured_generation=2,captured_reasons=9,captured_due_at=100 WHERE command_id=?",[id.clone()]).await?;
    assert!(
        member(&c, &id, "movies").await.is_err(),
        "running membership frozen"
    );
    assert!(
        c.execute(
            "DELETE FROM health_command_checks WHERE command_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("UPDATE health_commands SET attempts=3,status='retry_wait',error_code='check_failed' WHERE id=?",[id.clone()]).await.is_err());
    c.execute("UPDATE health_commands SET status='retry_wait',error_code='check_timeout',next_attempt_at=200 WHERE id=?",[id.clone()]).await?;
    assert!(
        member(&c, &id, "movies").await.is_err(),
        "retry membership frozen"
    );
    c.execute("UPDATE health_commands SET status='running',attempts=2,started_at=200,error_code=NULL WHERE id=?",[id.clone()]).await?;
    cancel(&c, &id).await?;
    c.execute("DELETE FROM health_commands WHERE id=?", [id])
        .await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_command_checks").await?,
        0
    );
    let grace = enqueue(&c).await?;
    member(&c, &grace, "tv").await?;
    c.execute(
        "UPDATE health_commands SET is_grace=1 WHERE id=?",
        [grace.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE health_commands SET status='running',attempts=1,started_at=999 WHERE id=?",
            [grace.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE health_lifecycle SET grace_phase='rechecking'", ())
        .await?;
    assert!(
        c.execute(
            "UPDATE health_commands SET status='running',attempts=1,started_at=1000 WHERE id=?",
            [grace.clone()]
        )
        .await
        .is_err(),
        "full startup membership required"
    );
    member(&c, &grace, "movies").await?;
    c.execute(
        "UPDATE health_lifecycle SET epoch=?",
        [uuid::Uuid::new_v4().to_string()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE health_commands SET status='running',attempts=1,started_at=1000 WHERE id=?",
            [grace.clone()]
        )
        .await
        .is_err(),
        "old grace cannot claim new epoch"
    );
    cancel(&c, &grace).await?;
    let current_grace = enqueue(&c).await?;
    member(&c, &current_grace, "tv").await?;
    member(&c, &current_grace, "movies").await?;
    c.execute(
        "UPDATE health_commands SET is_grace=1 WHERE id=?",
        [current_grace.clone()],
    )
    .await?;
    c.execute(
        "UPDATE health_commands SET status='running',attempts=1,started_at=1000 WHERE id=?",
        [current_grace.clone()],
    )
    .await?;
    // Legal current-epoch, complete grace work can commit observations and expire together.
    let tx = c.transaction().await?;
    tx.execute("UPDATE health_command_checks SET captured_generation=(SELECT generation FROM health_checks h WHERE h.scope=health_command_checks.scope AND h.check_key=health_command_checks.check_key),captured_reasons=1,captured_due_at=100 WHERE command_id=?",[current_grace.clone()]).await?;
    tx.execute("UPDATE health_checks SET severity=0,observed_generation=generation,observed_epoch=(SELECT epoch FROM health_lifecycle),checked_at=1000,pending_reasons=0,due_at=NULL",()).await?;
    tx.execute(
        "UPDATE health_commands SET status='succeeded',completed_at=1000 WHERE id=?",
        [current_grace.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE health_lifecycle SET grace_phase='expired',last_batch_completed_at=1000",
        (),
    )
    .await?;
    tx.commit().await?;
    // Ordinary retry can recapture a new epoch without resetting its attempt budget.
    let retry = enqueue(&c).await?;
    member(&c, &retry, "tv").await?;
    c.execute(
        "UPDATE health_commands SET status='running',attempts=1,started_at=1001 WHERE id=?",
        [retry.clone()],
    )
    .await?;
    c.execute("UPDATE health_commands SET status='retry_wait',error_code='check_timeout',next_attempt_at=1002 WHERE id=?",[retry.clone()]).await?;
    c.execute("UPDATE health_lifecycle SET epoch=?,started_at=1000,grace_due_at=1900,grace_phase='pending'",[uuid::Uuid::new_v4().to_string()]).await?;
    c.execute("UPDATE health_commands SET status='running',attempts=2,started_at=1002,error_code=NULL,epoch=(SELECT epoch FROM health_lifecycle) WHERE id=?",[retry.clone()]).await?;
    c.execute("UPDATE health_commands SET status='retry_wait',error_code='check_failed',next_attempt_at=1003 WHERE id=?",[retry.clone()]).await?;
    c.execute("UPDATE health_commands SET status='running',attempts=3,started_at=1003,error_code=NULL WHERE id=?",[retry.clone()]).await?;
    assert!(
        c.execute(
            "UPDATE health_commands SET status='retry_wait',error_code='check_failed' WHERE id=?",
            [retry.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE health_commands SET status='failed',completed_at=1004,error_code='check_failed' WHERE id=?",[retry.clone()]).await?;
    assert!(
        c.execute(
            "UPDATE health_commands SET completed_at=1005 WHERE id=?",
            [retry]
        )
        .await
        .is_err(),
        "terminal command immutable"
    );
    let cancelled_retry = enqueue(&c).await?;
    member(&c, &cancelled_retry, "tv").await?;
    c.execute(
        "UPDATE health_commands SET status='running',attempts=1,started_at=1001 WHERE id=?",
        [cancelled_retry.clone()],
    )
    .await?;
    c.execute("UPDATE health_commands SET status='retry_wait',error_code='check_timeout',next_attempt_at=1002 WHERE id=?",[cancelled_retry.clone()]).await?;
    assert!(
        cancel(&c, &cancelled_retry).await.is_err(),
        "cancel must clear retry error"
    );
    c.execute("UPDATE health_commands SET status='cancelled',completed_at=1002,error_code=NULL WHERE id=?",[cancelled_retry]).await?;
    // Seed all observed fields together; NULL severity/partial observations never mean OK.
    c.execute("UPDATE health_checks SET severity=2,reason='disabled',message='previous issue',wiki_url='https://example.invalid/help',observed_generation=generation,observed_epoch=?,checked_at=100 WHERE scope='tv'",[epoch.clone()]).await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM health_checks WHERE scope='movies' AND severity=0"
        )
        .await?,
        2 // The same successful publication covers both registered movie checks.
    );
    assert!(
        c.execute(
            "UPDATE health_checks SET message=? WHERE scope='tv'",
            ["é".repeat(2049)]
        )
        .await
        .is_err(),
        "byte ceiling, not scalar count"
    );
    // Four registered checks leave exactly124 slots before the unchanged128 ceiling.
    for i in 0..124 {
        c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('system',?,0,0,'FutureCheck')",[format!("check_{i}")]).await?;
    }
    assert!(c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('system','overflow',0,0,'FutureCheck')",()).await.is_err());
    for i in 0..130 {
        let id = enqueue(&c).await?;
        member(&c, &id, "tv").await?;
        c.execute(
            "UPDATE health_commands SET status='cancelled',completed_at=? WHERE id=?",
            params![2000 + i, id],
        )
        .await?;
    }
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_commands").await?,
        128
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_command_checks").await?,
        256 // Each retained command admitted both TV identities.
    );
    // Migration43 requires explicit attempt provenance on all new diagnostic events.
    let event_command = uuid::Uuid::new_v4().to_string();
    for i in 0..1025 {
        c.execute("INSERT INTO health_transitions(command_attempt,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type)VALUES(1,?,?,?,'tv','completed_download_handling','issue',0,?,2,'disabled','previous issue','https://example.invalid/help','ImportMechanismCheck')",params![uuid::Uuid::new_v4().to_string(),epoch.clone(),if i==1024 {event_command.clone()}else{uuid::Uuid::new_v4().to_string()},100+i]).await?;
    }
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_transitions").await?,
        1024
    );
    assert_eq!(
        scalar(&c, "SELECT min(sequence) FROM health_transitions").await?,
        2
    );
    assert!(
        c.execute("UPDATE health_transitions SET message='rewritten'", ())
            .await
            .is_err()
    );
    assert!(c.execute("INSERT INTO health_transitions(command_attempt,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type)VALUES(1,?,?,?,'tv','completed_download_handling','issue',0,1,2,'disabled','again','https://example.invalid','ImportMechanismCheck')",params![uuid::Uuid::new_v4().to_string(),epoch,event_command]).await.is_err(),"publication identity deduplicated");
    // An exhausted public sequence fails closed, without resetting retained cursor identity.
    let tx = c.transaction().await?;
    tx.execute("INSERT INTO health_transitions(command_attempt,sequence,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type)VALUES(1,9007199254740991,?,?,?,'tv','completed_download_handling','issue',0,1,2,'disabled','last','https://example.invalid','ImportMechanismCheck')",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string()]).await?;
    assert!(tx.execute("INSERT INTO health_transitions(command_attempt,event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type)VALUES(1,?,?,?,'tv','completed_download_handling','issue',0,1,2,'disabled','overflow','https://example.invalid','ImportMechanismCheck')",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string()]).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        scalar(&c, "SELECT max(sequence) FROM health_transitions").await?,
        1025
    );
    // Real validated search rows fill the shared pool; terminal health history is free.
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,1,'TV','/tv');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'One');").await?;
    let provider = super::refresh_tests::provider(&c).await?;
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    let tx = c.transaction().await?;
    let padding =
        super::refresh_tests::pad_search_commands(&tx, &indexer, &provider, "tv", 1024).await?;
    tx.commit().await?;
    assert!(
        enqueue(&c).await.is_err(),
        "1024 active sibling rows reject health"
    );
    c.execute(
        "UPDATE search_commands SET status='cancelled',completed_at=500 WHERE id=?",
        [padding[0].clone()],
    )
    .await?;
    let active = enqueue(&c).await?;
    assert!(
        super::refresh_tests::enqueue(&c, &provider, "tv")
            .await
            .is_err(),
        "health occupies the last active slot"
    );
    cancel(&c, &active).await?;
    super::refresh_tests::enqueue(&c, &provider, "tv").await?;
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    Ok(())
}

async fn version42(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(42).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    Ok((db, c))
}
async fn diagnostic(
    c: &Connection,
    command: &str,
    attempt: Option<i64>,
    kind: &str,
    scope: &str,
    grace: bool,
) -> Result<(), libsql::Error> {
    let columns = "event_id,epoch,command_id,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type";
    let values = "?,?,?,?, 'completed_download_handling',?,?,100,2,'disabled','retained issue','https://example.invalid/help','ImportMechanismCheck'";
    let sql = match attempt {
        Some(attempt) => format!(
            "INSERT INTO health_transitions({columns},command_attempt)VALUES({values},{attempt})"
        ),
        None => format!("INSERT INTO health_transitions({columns})VALUES({values})"),
    };
    c.execute(
        &sql,
        params![
            uuid::Uuid::new_v4().to_string(),
            "00000000-0000-0000-0000-000000000000",
            command,
            scope,
            kind,
            grace
        ],
    )
    .await?;
    Ok(())
}
async fn schema(c: &Connection) -> Result<Vec<Vec<libsql::Value>>, Error> {
    // Stable ordering and explicit columns also preserve autoindex/trigger definitions.
    let mut r = c
        .query(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
            (),
        )
        .await?;
    let mut result = vec![];
    while let Some(row) = r.next().await? {
        result.push(
            (0..4)
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<_, _>>()?,
        );
    }
    Ok(result)
}
#[tokio::test]
async fn health_communication_upgrade_rollback_reopen_and_attempt_identity() -> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = version42(&path).await?;
    c.execute("UPDATE health_checks SET generation=1,pending_reasons=1,due_at=100,severity=2,reason='disabled',message='old',wiki_url='https://example.invalid',observed_generation=0,observed_epoch='00000000-0000-0000-0000-000000000000',checked_at=50 WHERE scope='tv'", ()).await?;
    let pruned = enqueue(&c).await?;
    cancel(&c, &pruned).await?;
    diagnostic(&c, &pruned, None, "issue", "tv", false).await?;
    c.execute("DELETE FROM health_commands WHERE id=?", [pruned])
        .await?;
    let terminal = enqueue(&c).await?;
    member(&c, &terminal, "movies").await?;
    cancel(&c, &terminal).await?;
    let active = enqueue(&c).await?;
    member(&c, &active, "tv").await?;
    c.execute(
        "UPDATE sqlite_sequence SET seq=4000 WHERE name='health_transitions'",
        (),
    )
    .await?;
    c.execute(
        "INSERT INTO sqlite_sequence(name,seq)VALUES('unrelated_fixture',17)",
        (),
    )
    .await?;
    let tables = [
        "health_checks",
        "health_lifecycle",
        "health_commands",
        "health_command_checks",
        "health_transitions",
        "sqlite_sequence",
        "schema_migrations",
    ];
    let mut original = vec![];
    for table in tables {
        original.push(rows(&c, table).await?);
    }
    let original_schema = schema(&c).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[42].1).await?;
    assert!(tx.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type)VALUES('invalid','late',1,1,'fixture')", ()).await.is_err());
    tx.rollback().await?;
    assert_eq!(schema(&c).await?, original_schema);
    for (table, expected) in tables.iter().zip(&original) {
        assert_eq!(&rows(&c, table).await?, expected, "rollback: {table}");
    }
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 43);
    // Existing states are unchanged; only the two requested registry identities are appended.
    for (table, expected) in tables[1..4].iter().zip(&original[1..4]) {
        assert_eq!(&rows(&c, table).await?, expected);
    }
    let registry = rows(&c, "health_checks").await?;
    assert_eq!(&registry[..2], &original[0]);
    assert_eq!(scalar(&c,"SELECT count(*) FROM health_checks WHERE scope IN ('tv','movies') AND check_key='download_client_communication' AND startup=1 AND scheduled=1 AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL AND last_error IS NULL AND severity IS NULL AND reason IS NULL AND message IS NULL AND wiki_url IS NULL AND compatibility_type='DownloadClientCheck'").await?, 2);
    let migrated = rows(&c, "health_transitions").await?;
    assert_eq!(migrated.len(), 1);
    // The new attempt column follows command_id; every original value is preserved.
    let mut legacy = migrated[0].clone();
    assert_eq!(legacy.remove(4), libsql::Value::Integer(0));
    assert_eq!(legacy, original[4][0]);
    assert_eq!(rows(&c, "sqlite_sequence").await?, original[5]);
    assert!(
        c.execute("UPDATE health_transitions SET command_attempt=1", ())
            .await
            .is_err()
    );
    let backups: Vec<_> = std::fs::read_dir(&scratch.0)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".hrrdarr-migration-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = libsql::Builder::new_local(backups[0].path().join("database.db"))
        .build()
        .await?;
    assert_eq!(version(&backup.connect()?).await?, 42);
    assert_eq!(
        rows(&backup.connect()?, "health_transitions").await?,
        original[4]
    );
    drop(backup);
    let command = uuid::Uuid::new_v4().to_string();
    for (attempt, kind) in [(1, "issue"), (2, "restored"), (3, "issue")] {
        diagnostic(&c, &command, Some(attempt), kind, "tv", false).await?;
    }
    assert_eq!(
        scalar(&c, "SELECT max(sequence) FROM health_transitions").await?,
        4003
    );
    assert!(
        diagnostic(&c, &command, Some(3), "issue", "tv", false)
            .await
            .is_err()
    );
    diagnostic(&c, &command, Some(3), "issue", "movies", false).await?;
    diagnostic(&c, &command, Some(3), "issue", "tv", true).await?;
    for attempt in [None, Some(0), Some(-1), Some(4)] {
        assert!(
            diagnostic(&c, &command, attempt, "restored", "movies", false)
                .await
                .is_err()
        );
    }
    assert!(c.execute("INSERT INTO health_transitions SELECT sequence+10000,?,epoch,command_id,1.5,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type FROM health_transitions LIMIT 1",[uuid::Uuid::new_v4().to_string()]).await.is_err());
    // Event UUID remains unique independently of the attempt identity.
    assert!(c.execute("INSERT INTO health_transitions SELECT sequence+10000,event_id,epoch,command_id,3,scope,check_key,'restored',in_grace,created_at,severity,reason,message,wiki_url,compatibility_type FROM health_transitions LIMIT 1",()).await.is_err());
    let before = rows(&c, "health_transitions").await?;
    let sequence = rows(&c, "sqlite_sequence").await?;
    let tx = c.transaction().await?;
    diagnostic(
        &tx,
        &uuid::Uuid::new_v4().to_string(),
        Some(1),
        "issue",
        "tv",
        false,
    )
    .await?;
    tx.rollback().await?;
    assert_eq!(rows(&c, "health_transitions").await?, before);
    assert_eq!(rows(&c, "sqlite_sequence").await?, sequence);
    integrity(&c).await?;
    drop(c);
    drop(db);
    for _ in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 43);
        assert_eq!(rows(&c, "health_transitions").await?, before);
        assert_eq!(rows(&c, "sqlite_sequence").await?, sequence);
        assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 4);
    }
    Ok(())
}

#[tokio::test]
async fn health_communication_sequence_empty_exhausted_invalid_and_ring() -> Result<(), Error> {
    let scratch = Scratch::new();
    // Use real version42 state, including an empty ring with an allocated/exhausted cursor.
    for (index, high_water) in [None, Some(7000), Some(9007199254740991)]
        .into_iter()
        .enumerate()
    {
        let path = scratch.0.join(format!("sequence-{index}"));
        let (raw, c) = version42(&path).await?;
        if let Some(seq) = high_water {
            c.execute(
                "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions',?)",
                [seq],
            )
            .await?;
        }
        drop(c);
        drop(raw);
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(
            scalar(
                &c,
                "SELECT seq FROM sqlite_sequence WHERE name='health_transitions'"
            )
            .await?,
            high_water.unwrap_or(0)
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM sqlite_sequence WHERE name LIKE 'health_transitions%'"
            )
            .await?,
            1
        );
        let result = diagnostic(
            &c,
            &uuid::Uuid::new_v4().to_string(),
            Some(1),
            "issue",
            "tv",
            false,
        )
        .await;
        if high_water == Some(9007199254740991) {
            assert!(result.is_err());
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM health_transitions").await?,
                0
            );
        } else {
            result?;
            assert_eq!(
                scalar(&c, "SELECT max(sequence) FROM health_transitions").await?,
                high_water.unwrap_or(0) + 1
            );
        }
        integrity(&c).await?;
    }
    let (raw, c) = version42(&scratch.0.join("invalid")).await?;
    for sql in [
        "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions',NULL)",
        "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions','oops')",
        "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions',-1)",
        "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions',9007199254740992)",
        "INSERT INTO sqlite_sequence(name,seq)VALUES('health_transitions',1),('health_transitions',2)",
    ] {
        c.execute(sql, ()).await?;
        let before = rows(&c, "sqlite_sequence").await?;
        let tx = c.transaction().await?;
        assert!(tx.execute_batch(MIGRATIONS[42].1).await.is_err(), "{sql}");
        tx.rollback().await?;
        assert_eq!(rows(&c, "sqlite_sequence").await?, before);
        assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 2);
        assert_eq!(scalar(&c,"SELECT count(*) FROM sqlite_schema WHERE name LIKE '%_next' OR name='health_transition_migration_sequence'").await?,0);
        c.execute(
            "DELETE FROM sqlite_sequence WHERE name='health_transitions'",
            (),
        )
        .await?;
    }
    // Fill the old ring, then prove migration itself retains every payload and identity.
    for _ in 0..1024 {
        diagnostic(
            &c,
            &uuid::Uuid::new_v4().to_string(),
            None,
            "issue",
            "tv",
            false,
        )
        .await?;
    }
    let original = rows(&c, "health_transitions").await?;
    c.execute_batch(MIGRATIONS[42].1).await?;
    let migrated = rows(&c, "health_transitions").await?;
    assert_eq!(migrated.len(), 1024);
    for (mut row, old) in migrated.into_iter().zip(original) {
        assert_eq!(row.remove(4), libsql::Value::Integer(0));
        assert_eq!(row, old);
    }
    diagnostic(
        &c,
        &uuid::Uuid::new_v4().to_string(),
        Some(1),
        "issue",
        "tv",
        false,
    )
    .await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM health_transitions").await?,
        1024
    );
    assert_eq!(
        scalar(&c, "SELECT min(sequence) FROM health_transitions").await?,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT max(sequence) FROM health_transitions").await?,
        1025
    );
    integrity(&c).await?;
    drop(c);
    drop(raw);
    Ok(())
}
