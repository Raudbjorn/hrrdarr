use super::rss_tests::{command, identity};
use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-selection-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn rows(c: &Connection, sql: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut query = c.query(sql, ()).await?;
    let mut result = vec![];
    while let Some(row) = query.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<_, _>>()?,
        );
    }
    Ok(result)
}
async fn seed51(path: &Path) -> Result<(libsql::Database, Connection), Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(51).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    Ok((db, c))
}
async fn library(c: &Connection) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'TV','/tv'); INSERT INTO seasons VALUES(1,1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'); INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,101,'Movie',2020); INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies');").await?;
    Ok(())
}

#[tokio::test]
async fn selection_upgrade51_preserves_legacy_children_and_late_failure_rolls_back()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let (raw, c) = seed51(&path).await?;
    library(&c).await?;
    let client = super::refresh_tests::provider(&c).await?;
    for media in ["tv", "movies"] {
        let indexer = super::refresh_tests::search_indexer(&c, media).await?;
        let cmd = command(&c, &indexer, &client, media).await?;
        let receipt = super::processing_tests::observed(
            &c,
            &cmd,
            &client,
            media,
            &if media == "tv" {
                "a".repeat(40)
            } else {
                "b".repeat(40)
            },
        )
        .await?;
        c.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at)VALUES(?,?,?,1,?,1,60,1,100,100)",params![uuid::Uuid::new_v4().to_string(),media,indexer.clone(),client.clone()]).await?;
        c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode)VALUES(?,?,1,1,1,'copy')",params![client.clone(),media]).await?;
        c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,1,'queued',100,100,100)",[receipt.clone()]).await?;
        let operation = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message)VALUES(?,?,?,?,'/download/new.mkv','copy',?,'preview','fixture')",params![operation.clone(),if media=="tv"{"episode"}else{"movie"},(media=="tv").then_some(1),(media=="movies").then_some(1),format!("/{media}/new.mkv")]).await?;
        c.execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,'{}','preview')",
            [operation.clone()],
        )
        .await?;
        c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json)VALUES(?,?,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')",params![receipt,operation]).await?;
        super::refresh_tests::enqueue(&c, &client, media).await?;
        // Selected results exercise the incoming search/result/candidate reference cycle.
        let search =
            super::search_tests::search(&c, &indexer, &client, media, "interactive").await?;
        super::search_tests::start(&c, &search).await?;
        let offer = super::search_tests::offer(&c, &search, 0, 29, 0).await?;
        // Fetch facts publish while running; success is a separate existing checkpoint.
        c.execute(
            "UPDATE search_commands SET fetch_complete=1 WHERE id=?",
            [search.clone()],
        )
        .await?;
        c.execute(
            "UPDATE search_commands SET status='succeeded',completed_at=101 WHERE id=?",
            [search],
        )
        .await?;
        super::search_tests::select(&c, &offer).await?;
    }
    // Historical imported data is unrelated to the pending imports but shares their library owners.
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/old.mkv'); UPDATE episodes SET episode_file_id=1 WHERE id=1; INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message)VALUES('prior-import','episode',1,'/prior','copy','/tv/old.mkv','preview','fixture'); INSERT INTO import_journal(operation_id,plan_json,phase)VALUES('prior-import','{}','preview'); INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)VALUES('prior-import','episode',1,1,'/prior','/tv/old.mkv',1,printf('%064d',0)); UPDATE import_journal SET phase='complete',stage_json='{}' WHERE operation_id='prior-import';").await?;
    c.execute("CREATE TRIGGER selection_fail_history BEFORE INSERT ON schema_migrations WHEN NEW.version=52 BEGIN SELECT RAISE(ABORT,'injected late selection migration failure'); END",()).await?;
    let tables = [
        "rss_commands",
        "rss_schedules",
        "search_commands",
        "rss_candidates",
        "rss_candidate_episodes",
        "rss_hash_claims",
        "search_results",
        "download_processing",
        "rss_candidate_imports",
        "import_journal",
        "import_history",
        "providers",
        "provider_scopes",
        "commands",
        "health_checks",
        "schema_migrations",
    ];
    let mut before = vec![];
    let mut columns = vec![];
    for table in tables {
        before.push(rows(&c, &format!("SELECT * FROM {table} ORDER BY 1")).await?);
        let mut q = c.query(&format!("PRAGMA table_info({table})"), ()).await?;
        let mut names = vec![];
        while let Some(row) = q.next().await? {
            names.push(row.get::<String>(1)?);
        }
        columns.push(names.join(","));
    }
    let schema = rows(
        &c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )
    .await?;
    drop(c);
    drop(raw);
    let error = match Database::open_local(&path).await {
        Ok(_) => panic!("injected migration failure ignored"),
        Err(e) => e,
    };
    assert!(
        error
            .to_string()
            .contains("injected late selection migration failure")
    );
    assert!(error.to_string().contains("migration rolled back"));
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    assert_eq!(version(&c).await?, 51);
    assert_eq!(
        rows(
            &c,
            "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"
        )
        .await?,
        schema
    );
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(
            &rows(&c, &format!("SELECT * FROM {table} ORDER BY 1")).await?,
            expected,
            "rollback {table}"
        );
    }
    c.execute("DROP TRIGGER selection_fail_history", ()).await?;
    drop(c);
    drop(raw);
    for reopen in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 59); // Reasoning: latest adds 0059 recycle bin settings, deletion journal and cleanup commands; historical prefixes stay fixed.
        assert_eq!(scalar(&c, "PRAGMA foreign_keys").await?, 1);
        for ((table, cols), expected) in tables.iter().zip(&columns).zip(&before) {
            let suffix = if *table == "schema_migrations" {
                " WHERE version<=51"
            } else if *table == "health_checks" {
                // 0053 and 0059 add two identities each; all schema51 observations remain exact.
                " WHERE check_key NOT IN ('remote_path_mapping','recycle_bin')"
            } else {
                ""
            };
            assert_eq!(
                &rows(
                    &c,
                    &format!("SELECT {cols} FROM {table}{suffix} ORDER BY 1")
                )
                .await?,
                expected,
                "upgrade {table}"
            );
        }
        for table in [
            "rss_commands",
            "rss_schedules",
            "search_commands",
            "rss_candidates",
        ] {
            assert_eq!(scalar(&c,&format!("SELECT count(*) FROM {table} WHERE selection_kind!='legacy_fixed' OR client_id IS NULL OR client_revision IS NULL")).await?,0);
        }
        assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE selection_round!=0 OR payload_expires_at IS NOT NULL").await?,0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM rss_client_attempts").await?,
            0
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM sqlite_temp_schema WHERE name LIKE '_selection52_%'"
            )
            .await?,
            0
        );
        if reopen == 0 {
            let backup =
                libsql::Builder::new_local(db.migration_backup().unwrap().join("database.db"))
                    .build()
                    .await?;
            assert_eq!(version(&backup.connect()?).await?, 51);
        } else {
            assert!(db.migration_backup().is_none());
        }
        integrity(&c).await?;
    }
    Ok(())
}

async fn unresolved(
    c: &Connection,
    indexer: &str,
    media: &str,
    kind: &str,
    client: Option<&str>,
) -> Result<String, Error> {
    let cmd = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,selection_kind,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,?,?,100,100)",params![cmd.clone(),media,indexer,client,client.map(|_|1),kind]).await?;
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,selection_kind,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at)SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,selection_kind,?,'Release',?,?,?,'pending','[]',100,100 FROM rss_commands WHERE id=?",params![id.clone(),id.replace('-',"").repeat(2),vec![1u8;29],(media=="tv").then_some(1),(media=="movies").then_some(1),cmd]).await?;
    if media == "tv" {
        c.execute(
            "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
            [id.clone()],
        )
        .await?;
    }
    Ok(id)
}
async fn plan(c: &Connection, id: &str, clients: &[&str]) -> Result<(), Error> {
    let now = c
        .query(
            "SELECT max(200,coalesce(not_before,0)) FROM rss_candidates WHERE id=?",
            [id],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?;
    plan_at(c, id, clients, now).await
}
async fn plan_at(c: &Connection, id: &str, clients: &[&str], now: i64) -> Result<(), Error> {
    let plan: Vec<_> = clients
        .iter()
        .map(|client| serde_json::json!({"client_id":client,"client_revision":1}))
        .collect();
    c.execute("UPDATE rss_candidates SET selection_plan_json=?,selection_round=selection_round+1,payload_expires_at=coalesce(payload_expires_at,?+1800),updated_at=? WHERE id=?",params![serde_json::to_string(&plan)?,now,now,id]).await?;
    Ok(())
}
async fn select(c: &Connection, id: &str, client: &str, ordinal: i64) -> Result<(), Error> {
    c.execute("INSERT INTO rss_client_attempts(candidate_id,ordinal,selection_round,client_id,client_revision,state,created_at,updated_at)SELECT id,?,selection_round,?,1,'selected',updated_at,updated_at FROM rss_candidates WHERE id=?",params![ordinal,client,id]).await?;
    c.execute(
        "UPDATE rss_candidates SET active_attempt=?,client_id=?,client_revision=1 WHERE id=?",
        params![ordinal, client, id],
    )
    .await?;
    Ok(())
}
async fn prepare(
    c: &Connection,
    id: &str,
    ordinal: i64,
    media: &str,
    hash: &str,
) -> Result<(), Error> {
    let identity = identity(media, &[hash]);
    c.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=? WHERE candidate_id=? AND ordinal=?",params![identity.clone(),id,ordinal]).await?;
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        params![identity, id],
    )
    .await?;
    Ok(())
}
async fn dispatch(
    c: &Connection,
    id: &str,
    client: &str,
    ordinal: i64,
    hash: &str,
) -> Result<(), Error> {
    c.execute(
        "INSERT INTO rss_hash_claims(client_id,hash,candidate_id)VALUES(?,?,?)",
        params![client, hash, id],
    )
    .await?;
    c.execute(
        "UPDATE rss_client_attempts SET state='dispatching' WHERE candidate_id=? AND ordinal=?",
        params![id, ordinal],
    )
    .await?;
    c.execute("UPDATE rss_candidates SET status='submitting',comparison_facts_json='{\"version\":1}' WHERE id=?",[id]).await?;
    Ok(())
}

#[tokio::test]
async fn selection_attempt_guards_allow_only_proven_safe_tv_fallback() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let first = super::refresh_tests::provider(&c).await?;
    let second = super::refresh_tests::provider(&c).await?;
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    let id = unresolved(&c, &indexer, "tv", "default", None).await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET selection_strict=1 WHERE id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE rss_candidates SET selection_strict=1,error_code='download_client_unavailable',decision_reasons_json='[\"download_client_unavailable\"]',not_before=300 WHERE id=?",[id.clone()]).await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET selection_strict=0 WHERE id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    plan(&c, &id, &[&first, &second]).await?;
    assert!(plan(&c, &id, &[&second]).await.is_err());
    select(&c, &id, &first, 1).await?;
    assert!(select(&c, &id, &second, 2).await.is_err());
    assert!(
        c.execute(
            "DELETE FROM rss_candidate_episodes WHERE candidate_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM rss_client_attempts WHERE candidate_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    prepare(&c, &id, 1, "tv", &"a".repeat(40)).await?;
    assert!(
        c.execute(
            "UPDATE rss_client_attempts SET state='dispatching' WHERE candidate_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    dispatch(&c, &id, &first, 1, &"a".repeat(40)).await?;
    assert!(c.execute("UPDATE rss_candidates SET status='pending',client_id=NULL,client_revision=NULL,active_attempt=NULL,submission_identity_json=NULL,comparison_facts_json=NULL WHERE id=?",[id.clone()]).await.is_err());
    assert!(
        c.execute(
            "DELETE FROM rss_hash_claims WHERE candidate_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    let tx = c.transaction().await?;
    tx.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='refresh_failed' WHERE candidate_id=? AND ordinal=1",[id.clone()]).await?;
    tx.execute(
        "DELETE FROM rss_hash_claims WHERE candidate_id=?",
        [id.clone()],
    )
    .await?;
    tx.execute("UPDATE rss_candidates SET status='pending',client_id=NULL,client_revision=NULL,active_attempt=NULL,submission_identity_json=NULL,comparison_facts_json=NULL WHERE id=?",[id.clone()]).await?;
    tx.commit().await?;
    assert!(select(&c, &id, &first, 2).await.is_err());
    select(&c, &id, &second, 2).await?;
    let changed = identity("tv", &[&"a".repeat(40)]).replace(&"b".repeat(64), &"c".repeat(64));
    assert!(c.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=? WHERE candidate_id=? AND ordinal=2",params![changed,id.clone()]).await.is_err());
    prepare(&c, &id, 2, "tv", &"a".repeat(40)).await?;
    dispatch(&c, &id, &second, 2, &"a".repeat(40)).await?;
    let tx = c.transaction().await?;
    tx.execute("UPDATE rss_client_attempts SET state='unknown',error_code='submission_unknown' WHERE candidate_id=? AND ordinal=2",[id.clone()]).await?;
    tx.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,private_payload=NULL,error_code='submission_unknown',not_before=300 WHERE id=?",[id.clone()]).await?;
    tx.commit().await?;
    assert!(select(&c, &id, &first, 3).await.is_err());
    assert!(
        c.execute(
            "DELETE FROM rss_hash_claims WHERE candidate_id=?",
            [id.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='refresh_failed' WHERE candidate_id=? AND ordinal=2",[id.clone()]).await.is_err());
    assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 1);
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE private_payload IS NULL AND status='reconciling'").await?,1);
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn selection_scope_tags_intent_and_terminal_cleanup_are_enforced() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let client = super::refresh_tests::provider(&c).await?;
    let other = super::refresh_tests::provider(&c).await?;
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    c.execute_batch(
        "INSERT INTO tags(id,media_type,label)VALUES(1,'tv','same'),(2,'movies','same');",
    )
    .await?;
    c.execute(
        "INSERT INTO download_client_tags VALUES(?,'tv',1)",
        [client.clone()],
    )
    .await?;
    for sql in [
        "INSERT INTO download_client_tags VALUES(?,'tv',2)",
        "INSERT INTO download_client_tags VALUES(?,'tv',1)",
    ] {
        assert!(c.execute(sql, [client.clone()]).await.is_err());
    }
    assert!(
        c.execute(
            "INSERT INTO download_client_tags VALUES(?,'tv',1)",
            [indexer.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("DELETE FROM tags WHERE id=1", ()).await.is_err());
    let explicit = unresolved(&c, &indexer, "tv", "explicit", Some(&client)).await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET client_id=? WHERE id=?",
            params![other.clone(), explicit.clone()]
        )
        .await
        .is_err()
    );
    assert!(plan(&c, &explicit, &[&other]).await.is_err());
    plan(&c, &explicit, &[&client]).await?;
    select(&c, &explicit, &client, 1).await?;
    c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='provider_changed' WHERE candidate_id=?",[explicit.clone()]).await?;
    c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code='provider_changed' WHERE id=?",[explicit.clone()]).await?;
    c.execute("DELETE FROM rss_candidates WHERE id=?", [explicit])
        .await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM rss_client_attempts").await?,
        0
    );
    let movie_indexer = super::refresh_tests::search_indexer(&c, "movies").await?;
    let movie = unresolved(&c, &movie_indexer, "movies", "default", None).await?;
    assert!(plan(&c, &movie, &[&client, &other]).await.is_err());
    plan(&c, &movie, &[&client]).await?;
    select(&c, &movie, &client, 1).await?;
    prepare(&c, &movie, 1, "movies", &"c".repeat(40)).await?;
    dispatch(&c, &movie, &client, 1, &"c".repeat(40)).await?;
    let tx = c.transaction().await?;
    tx.execute(
        "UPDATE rss_client_attempts SET state='observed' WHERE candidate_id=?",
        [movie.clone()],
    )
    .await?;
    tx.execute("UPDATE rss_candidates SET status='observed',observed_hash=?,private_payload=NULL WHERE id=?",params!["c".repeat(40),movie.clone()]).await?;
    tx.commit().await?;
    assert!(select(&c, &movie, &other, 2).await.is_err());
    assert!(
        c.execute("DELETE FROM rss_hash_claims WHERE candidate_id=?", [movie])
            .await
            .is_err()
    );
    c.execute("WITH RECURSIVE n(i)AS(VALUES(3)UNION ALL SELECT i+1 FROM n WHERE i<202) INSERT INTO tags(id,media_type,label) SELECT i,'tv','tag-'||i FROM n",()).await?;
    c.execute("INSERT INTO download_client_tags(provider_id,media_type,tag_id) SELECT ?,'tv',id FROM tags WHERE id BETWEEN 3 AND 201",[client.clone()]).await?;
    assert!(
        c.execute(
            "INSERT INTO download_client_tags VALUES(?,'tv',202)",
            [client.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "DELETE FROM provider_scopes WHERE provider_id=? AND media_type='tv'",
        [client],
    )
    .await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM download_client_tags").await?,
        0
    );
    c.execute("DELETE FROM tags WHERE id=1", ()).await?;
    integrity(&c).await?;
    Ok(())
}

async fn safe_attempt(c: &Connection, id: &str, ordinal: i64) -> Result<(), Error> {
    c.execute("UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='provider_changed' WHERE candidate_id=? AND ordinal=?",params![id,ordinal]).await?;
    Ok(())
}
async fn clear_round(c: &Connection, id: &str, now: i64) -> Result<(), Error> {
    c.execute("UPDATE rss_candidates SET status='pending',selection_plan_json=NULL,active_attempt=NULL,client_id=NULL,client_revision=NULL,submission_identity_json=NULL,comparison_facts_json=NULL,selection_strict=1,error_code='download_client_unavailable',decision_reasons_json='[\"download_client_unavailable\"]',not_before=?+1,updated_at=? WHERE id=?",params![now,now,id]).await?;
    Ok(())
}
async fn search_default(c: &Connection, indexer: &str, mode: &str) -> Result<String, Error> {
    let command = uuid::Uuid::new_v4().to_string();
    let captured = serde_json::json!({"media_type":"tv","series_id":1,"episode_id":1,"tvdb_id":101,"title":"TV","season":1,"number":1,"series_type":"standard","use_scene_numbering":false});
    c.execute("INSERT INTO search_commands(id,mode,media_type,requested_episode_id,captured_target_json,indexer_id,indexer_revision,selection_kind,next_attempt_at,created_at)VALUES(?,?,'tv',1,?,?,1,'default',100,100)",params![command.clone(),mode,captured.to_string(),indexer]).await?;
    super::search_tests::start(c, &command).await?;
    let offer = super::search_tests::offer(c, &command, 0, 29, 0).await?;
    c.execute(
        "UPDATE search_commands SET fetch_complete=1 WHERE id=?",
        [command.clone()],
    )
    .await?;
    if mode == "interactive" {
        c.execute(
            "UPDATE search_commands SET status='succeeded',completed_at=101 WHERE id=?",
            [command],
        )
        .await?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO rss_candidates(id,search_result_id,media_type,indexer_id,indexer_revision,selection_kind,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at)SELECT ?,r.id,c.media_type,c.indexer_id,c.indexer_revision,c.selection_kind,r.fingerprint,r.title,r.private_payload,1,'pending','[]',100,100 FROM search_results r JOIN search_commands c ON c.id=r.command_id WHERE r.id=?",params![id.clone(),offer]).await?;
    c.execute(
        "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
        [id.clone()],
    )
    .await?;
    Ok(id)
}

#[tokio::test]
async fn selection_retry_rounds_retain_exact_unknown_and_observed_hash_ownership()
-> Result<(), Error> {
    for outcome in ["unknown", "observed"] {
        let scratch = Scratch::new();
        let path = scratch.0.join("db");
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        library(&c).await?;
        let client = super::refresh_tests::provider(&c).await?;
        let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
        let id = unresolved(&c, &indexer, "tv", "default", None).await?;
        let hash = "a".repeat(40);
        plan(&c, &id, &[&client]).await?;
        assert!(
            c.execute(
                "UPDATE rss_candidates SET series_id=NULL WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "DELETE FROM rss_candidate_episodes WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        select(&c, &id, &client, 1).await?;
        prepare(&c, &id, 1, "tv", &hash).await?;
        dispatch(&c, &id, &client, 1, &hash).await?;
        assert!(clear_round(&c, &id, 200).await.is_err());
        safe_attempt(&c, &id, 1).await?;
        // A safe result alone does not release existing ownership.
        assert!(clear_round(&c, &id, 200).await.is_err());
        c.execute(
            "DELETE FROM rss_hash_claims WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
        clear_round(&c, &id, 200).await?;
        assert!(plan_at(&c, &id, &[&client], 200).await.is_err());
        assert!(
            c.execute(
                "UPDATE rss_candidates SET payload_expires_at=2001 WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "UPDATE rss_candidates SET series_id=NULL WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "DELETE FROM rss_candidate_episodes WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        drop(c);
        drop(db);
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE selection_round=1 AND selection_strict=1 AND selection_plan_json IS NULL AND payload_expires_at=2000 AND length(private_payload)=29").await?,1);
        plan_at(&c, &id, &[&client], 201).await?;
        assert!(
            c.execute(
                "UPDATE rss_candidates SET selection_round=3 WHERE id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        select(&c, &id, &client, 2).await?;
        let changed = identity("tv", &[&hash]).replace(&"b".repeat(64), &"c".repeat(64));
        assert!(c.execute("UPDATE rss_client_attempts SET state='prepared',identity_json=? WHERE candidate_id=? AND ordinal=2",params![changed,id.clone()]).await.is_err());
        assert!(c.execute("UPDATE rss_client_attempts SET state='prepared',identity_json='{}' WHERE candidate_id=? AND ordinal=2",[id.clone()]).await.is_err());
        prepare(&c, &id, 2, "tv", &hash).await?;
        dispatch(&c, &id, &client, 2, &hash).await?;
        // Publication is one transaction: failure after journal settlement must undo it.
        c.execute("CREATE TRIGGER fail_selection_publication BEFORE UPDATE ON rss_candidates WHEN NEW.status='observed' BEGIN SELECT RAISE(ABORT,'injected observation publication failure'); END",()).await?;
        let tx = c.transaction().await?;
        tx.execute(
            "UPDATE rss_client_attempts SET state='observed' WHERE candidate_id=? AND ordinal=2",
            [id.clone()],
        )
        .await?;
        assert!(tx.execute("UPDATE rss_candidates SET status='observed',observed_hash=?,private_payload=NULL WHERE id=?",params![hash.clone(),id.clone()]).await.is_err());
        tx.rollback().await?;
        c.execute("DROP TRIGGER fail_selection_publication", ())
            .await?;
        assert_eq!(scalar(&c,"SELECT count(*) FROM rss_client_attempts a JOIN rss_candidates c ON c.id=a.candidate_id WHERE a.ordinal=2 AND a.state='dispatching' AND c.status='submitting'").await?,1);
        let tx = c.transaction().await?;
        tx.execute("UPDATE rss_client_attempts SET state=?,error_code=CASE WHEN ?='unknown' THEN 'submission_unknown' END WHERE candidate_id=? AND ordinal=2",params![outcome,outcome,id.clone()]).await?;
        if outcome == "unknown" {
            tx.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,private_payload=NULL,error_code='submission_unknown',not_before=300 WHERE id=?",[id.clone()]).await?;
        } else {
            tx.execute("UPDATE rss_candidates SET status='observed',observed_hash=?,private_payload=NULL WHERE id=?",params![hash.clone(),id.clone()]).await?;
        }
        tx.commit().await?;
        // Historical safe attempt1 must never authorize release of attempt2's claim.
        assert!(
            c.execute(
                "DELETE FROM rss_hash_claims WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(clear_round(&c, &id, 202).await.is_err());
        assert!(plan_at(&c, &id, &[&client], 203).await.is_err());
        assert!(select(&c, &id, &client, 3).await.is_err());
        for sql in [
            "UPDATE rss_candidates SET private_payload=zeroblob(29) WHERE id=?",
            "UPDATE rss_client_attempts SET selection_round=1 WHERE candidate_id=? AND ordinal=2",
            "UPDATE rss_client_attempts SET state='definitely_not_submitted',error_code='refresh_failed' WHERE candidate_id=? AND ordinal=2",
            "UPDATE rss_client_attempts SET identity_json=NULL WHERE candidate_id=? AND ordinal=1",
        ] {
            assert!(c.execute(sql, [id.clone()]).await.is_err(), "{sql}");
        }
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM rss_client_attempts").await?,
            2
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 1);
        integrity(&c).await?;
    }
    Ok(())
}

#[tokio::test]
async fn selection_retry_rounds_require_complete_safe_plan_and_automatic_origin()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let client = super::refresh_tests::provider(&c).await?;
    let other = super::refresh_tests::provider(&c).await?;
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    for mode in ["interactive", "automatic"] {
        let id = search_default(&c, &indexer, mode).await?;
        let park = "UPDATE rss_candidates SET selection_strict=1,error_code='download_client_unavailable',decision_reasons_json='[\"download_client_unavailable\"]',not_before=150 WHERE id=?";
        if mode == "interactive" {
            assert!(c.execute(park, [id.clone()]).await.is_err());
        } else {
            c.execute(park, [id.clone()]).await?;
        }
        plan(&c, &id, &[&client, &other]).await?;
        select(&c, &id, &client, 1).await?;
        assert!(clear_round(&c, &id, 200).await.is_err());
        // A stale pre-prepare client can be safely settled without inventing an identity.
        safe_attempt(&c, &id, 1).await?;
        assert!(clear_round(&c, &id, 200).await.is_err());
        c.execute("UPDATE rss_candidates SET client_id=NULL,client_revision=NULL,active_attempt=NULL WHERE id=?",[id.clone()]).await?;
        assert!(select(&c, &id, &client, 2).await.is_err());
        select(&c, &id, &other, 2).await?;
        safe_attempt(&c, &id, 2).await?;
        if mode == "interactive" {
            assert!(clear_round(&c, &id, 200).await.is_err());
        } else {
            clear_round(&c, &id, 200).await?;
            // A nullable deadline must not bypass SQL's three-valued boolean guards.
            assert!(c.execute("UPDATE rss_candidates SET selection_plan_json=?,selection_round=2,not_before=NULL,updated_at=200 WHERE id=?",params![serde_json::json!([{"client_id":client,"client_revision":1}]).to_string(),id.clone()]).await.is_err());
            plan_at(&c, &id, &[&client], 201).await?;
            select(&c, &id, &client, 3).await?;
        }
    }
    let explicit = unresolved(&c, &indexer, "tv", "explicit", Some(&client)).await?;
    plan(&c, &explicit, &[&client]).await?;
    select(&c, &explicit, &client, 1).await?;
    safe_attempt(&c, &explicit, 1).await?;
    assert!(clear_round(&c, &explicit, 200).await.is_err());
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn selection_retry_rounds_bound_total_attempts_and_absolute_payload_lifetime()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let client = super::refresh_tests::provider(&c).await?;
    let other = super::refresh_tests::provider(&c).await?;
    let indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    let id = unresolved(&c, &indexer, "tv", "default", None).await?;
    for ordinal in 1..=128 {
        let now = 199 + ordinal;
        if ordinal == 128 {
            assert!(plan_at(&c, &id, &[&client, &other], now).await.is_err());
        }
        plan_at(&c, &id, &[&client], now).await?;
        select(&c, &id, &client, ordinal).await?;
        safe_attempt(&c, &id, ordinal).await?;
        if ordinal < 128 {
            clear_round(&c, &id, now).await?;
        } else {
            assert!(clear_round(&c, &id, now).await.is_err());
        }
    }
    assert!(select(&c, &id, &other, 129).await.is_err());
    c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code='selection_limit' WHERE id=?",[id.clone()]).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM rss_client_attempts").await?,
        128
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE selection_round=128 AND payload_expires_at=2000 AND error_code='selection_limit' AND private_payload IS NULL").await?,1);
    // A separate captured release expires at the original deadline, even after waiting safely.
    let expiry_indexer = super::refresh_tests::search_indexer(&c, "tv").await?;
    let expired = unresolved(&c, &expiry_indexer, "tv", "default", None).await?;
    plan(&c, &expired, &[&client]).await?;
    select(&c, &expired, &client, 1).await?;
    safe_attempt(&c, &expired, 1).await?;
    clear_round(&c, &expired, 200).await?;
    assert!(
        c.execute(
            "UPDATE rss_candidates SET payload_expires_at=3800,updated_at=2000 WHERE id=?",
            [expired.clone()]
        )
        .await
        .is_err()
    );
    assert!(plan_at(&c, &expired, &[&client], 2000).await.is_err());
    c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,error_code='payload_expired',updated_at=2000 WHERE id=?",[expired.clone()]).await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE error_code='payload_expired' AND selection_round=1 AND payload_expires_at=2000 AND private_payload IS NULL").await?,1);
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn selection_local_storage_retry_preserves_same_attempt_across_reopen() -> Result<(), Error> {
    for (media, kind) in [("tv", "default"), ("movies", "explicit")] {
        let scratch = Scratch::new();
        let path = scratch.0.join("db");
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        library(&c).await?;
        let client = super::refresh_tests::provider(&c).await?;
        let alternate = super::refresh_tests::provider(&c).await?;
        let indexer = super::refresh_tests::search_indexer(&c, media).await?;
        let id = unresolved(
            &c,
            &indexer,
            media,
            kind,
            (kind == "explicit").then_some(client.as_str()),
        )
        .await?;
        let clients = if kind == "default" {
            vec![client.as_str(), alternate.as_str()]
        } else {
            vec![client.as_str()]
        };
        plan(&c, &id, &clients).await?;
        select(&c, &id, &client, 1).await?;
        let hash = "a".repeat(40);
        prepare(&c, &id, 1, media, &hash).await?;
        dispatch(&c, &id, &client, 1, &hash).await?;
        let retained = "SELECT id,command_id,search_result_id,selection_kind,selection_round,selection_plan_json,selection_strict,active_attempt,client_id,client_revision,submission_identity_json,series_id,movie_id,private_payload,payload_expires_at,created_at,attempts FROM rss_candidates ORDER BY id";
        let frozen = rows(&c, retained).await?;
        let mark = "UPDATE rss_client_attempts SET state='prepared',error_code='local_storage_retry',updated_at=400 WHERE candidate_id=? AND ordinal=1";
        let reset = "UPDATE rss_candidates SET status='pending',error_code='local_storage_retry',decision_reasons_json='[\"local_storage_retry\"]',not_before=401,comparison_facts_json=NULL,updated_at=400 WHERE id=?";
        assert!(c.execute(reset, [id.clone()]).await.is_err());
        assert!(
            c.execute(
                &mark.replace("updated_at=400", "updated_at=2000"),
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                &mark.replace("local_storage_retry", "refresh_failed"),
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "UPDATE rss_client_attempts SET state='prepared' WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "DELETE FROM rss_hash_claims WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );

        // A failed projection write must roll back the proof and claim release together.
        c.execute("CREATE TRIGGER fail_local_retry BEFORE UPDATE ON rss_candidates WHEN NEW.error_code='local_storage_retry' BEGIN SELECT RAISE(ABORT,'injected retry publication failure'); END", ()).await?;
        let before_candidate = rows(&c, "SELECT * FROM rss_candidates ORDER BY id").await?;
        let before_attempt = rows(
            &c,
            "SELECT * FROM rss_client_attempts ORDER BY candidate_id,ordinal",
        )
        .await?;
        let tx = c.transaction().await?;
        tx.execute(mark, [id.clone()]).await?;
        tx.execute(
            "DELETE FROM rss_hash_claims WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
        assert!(tx.execute(reset, [id.clone()]).await.is_err());
        tx.rollback().await?;
        c.execute("DROP TRIGGER fail_local_retry", ()).await?;
        assert_eq!(
            rows(&c, "SELECT * FROM rss_candidates ORDER BY id").await?,
            before_candidate
        );
        assert_eq!(
            rows(
                &c,
                "SELECT * FROM rss_client_attempts ORDER BY candidate_id,ordinal"
            )
            .await?,
            before_attempt
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 1);

        let tx = c.transaction().await?;
        tx.execute(mark, [id.clone()]).await?;
        // Even safe local evidence cannot reset the projection while ownership remains.
        assert!(tx.execute(reset, [id.clone()]).await.is_err());
        tx.execute(
            "DELETE FROM rss_hash_claims WHERE candidate_id=?",
            [id.clone()],
        )
        .await?;
        for replacement in [
            ("not_before=401", "not_before=NULL"),
            ("status='pending'", "status='pending',command_id=NULL"),
            ("not_before=401", "not_before=400"),
            ("updated_at=400", "updated_at=2000"),
            (
                "comparison_facts_json=NULL",
                "comparison_facts_json='{\"version\":1}'",
            ),
            (
                "status='pending'",
                "status='pending',payload_expires_at=2001",
            ),
            ("status='pending'", "status='pending',active_attempt=NULL"),
            ("status='pending'", "status='pending',selection_round=2"),
            (
                "status='pending'",
                "status='pending',selection_plan_json=NULL",
            ),
            (
                "status='pending'",
                "status='pending',submission_identity_json=NULL",
            ),
            (
                "status='pending'",
                "status='pending',private_payload=zeroblob(30)",
            ),
            ("status='pending'", "status='pending',client_revision=2"),
        ] {
            let sql = reset.replace(replacement.0, replacement.1);
            assert!(tx.execute(&sql, [id.clone()]).await.is_err(), "{sql}");
        }
        tx.execute(reset, [id.clone()]).await?;
        tx.commit().await?;
        assert_eq!(rows(&c, retained).await?, frozen);
        assert_eq!(scalar(&c, "SELECT count(*) FROM rss_hash_claims").await?, 0);
        drop(c);
        drop(db);
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(rows(&c, retained).await?, frozen);
        assert_eq!(scalar(&c,"SELECT count(*) FROM rss_client_attempts a JOIN rss_candidates c ON c.id=a.candidate_id WHERE a.ordinal=1 AND a.selection_round=1 AND a.state='prepared' AND a.error_code='local_storage_retry' AND c.status='pending' AND c.not_before=401 AND c.payload_expires_at=2000").await?,1);
        assert!(select(&c, &id, &alternate, 2).await.is_err());
        assert!(clear_round(&c, &id, 401).await.is_err());
        for sql in [
            "UPDATE rss_client_attempts SET error_code=NULL WHERE candidate_id=?",
            "UPDATE rss_client_attempts SET identity_json=NULL WHERE candidate_id=?",
            "UPDATE rss_candidates SET not_before=NULL WHERE id=?",
            "UPDATE rss_candidates SET not_before=400 WHERE id=?",
            "UPDATE rss_candidates SET status='prepared',not_before=NULL,updated_at=400 WHERE id=?",
        ] {
            assert!(c.execute(sql, [id.clone()]).await.is_err(), "{sql}");
        }
        c.execute("UPDATE rss_candidates SET status='prepared',not_before=NULL,error_code=NULL,decision_reasons_json='[]',updated_at=401 WHERE id=?", [id.clone()]).await?;
        c.execute(
            "INSERT INTO rss_hash_claims(client_id,hash,candidate_id)VALUES(?,?,?)",
            params![client, hash.clone(), id.clone()],
        )
        .await?;
        assert!(c.execute("UPDATE rss_client_attempts SET state='dispatching',updated_at=401 WHERE candidate_id=?", [id.clone()]).await.is_err());
        c.execute("UPDATE rss_client_attempts SET state='dispatching',error_code=NULL,updated_at=401 WHERE candidate_id=?", [id.clone()]).await?;
        c.execute("UPDATE rss_candidates SET status='submitting',comparison_facts_json='{\"version\":1}',updated_at=401 WHERE id=?", [id.clone()]).await?;
        assert_eq!(rows(&c, retained).await?, frozen);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM rss_client_attempts").await?,
            1
        );
        assert!(
            c.execute(
                "DELETE FROM rss_hash_claims WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        // The earlier local proof cannot authorize a rewind after a later irreversible outcome.
        let tx = c.transaction().await?;
        if media == "tv" {
            tx.execute("UPDATE rss_client_attempts SET state='unknown',error_code='submission_unknown' WHERE candidate_id=?", [id.clone()]).await?;
            tx.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,private_payload=NULL,error_code='submission_unknown' WHERE id=?", [id.clone()]).await?;
        } else {
            tx.execute(
                "UPDATE rss_client_attempts SET state='observed' WHERE candidate_id=?",
                [id.clone()],
            )
            .await?;
            tx.execute("UPDATE rss_candidates SET status='observed',observed_hash=?,private_payload=NULL WHERE id=?", params![hash,id.clone()]).await?;
        }
        tx.commit().await?;
        assert!(
            c.execute(
                &mark.replace("updated_at=400", "updated_at=402"),
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                &reset
                    .replace("updated_at=400", "updated_at=402")
                    .replace("not_before=401", "not_before=403"),
                [id.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "DELETE FROM rss_hash_claims WHERE candidate_id=?",
                [id.clone()]
            )
            .await
            .is_err()
        );
        integrity(&c).await?;
    }
    Ok(())
}
