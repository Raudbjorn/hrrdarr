//! Actual HTTP reconciliation without a worker: reads must neither claim nor execute imports.
use hrrdarr::{commands, db::Database, import};
use libsql::{Connection, params};
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn post(client: &reqwest::Client, base: &str, path: &str, body: Value) -> (u16, Value) {
    let response = client
        .post(format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    // Match the command API's response ceiling before parsing the test response.
    const RESPONSE_LIMIT_BYTES: usize = 1024 * 1024;
    assert!(bytes.len() <= RESPONSE_LIMIT_BYTES);
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn reconcile(
    client: &reqwest::Client,
    base: &str,
    c: &Connection,
    ids: &[Uuid],
) -> Vec<Value> {
    let version = scalar(c, "PRAGMA data_version").await;
    let (status, value) = post(
        client,
        base,
        "/api/v1/manual-import/commands/reconcile",
        json!({"operation_ids":ids}),
    )
    .await;
    assert_eq!(status, 200, "{value}");
    assert_eq!(
        scalar(c, "PRAGMA data_version").await,
        version,
        "Reconciliation must not write through another connection"
    );
    let rows = value.as_array().unwrap();
    assert_eq!(rows.len(), ids.len());
    for (row, id) in rows.iter().zip(ids) {
        assert_eq!(row["operation_id"], id.to_string());
        assert_eq!(row.as_object().unwrap().len(), 3);
    }
    rows.clone()
}
async fn preview(
    db: Arc<Database>,
    source: String,
    destination: String,
    movie: bool,
    id: i64,
) -> Uuid {
    import::preview(
        db,
        import::ImportInput::Typed(import::ManualImportRequest {
            target: if movie {
                hrrdarr::db::MediaTarget::Movie(id)
            } else {
                hrrdarr::db::MediaTarget::Episode(id)
            },
            source,
            destination,
            mode: import::Mode::Copy,
        }),
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn operation_reconciliation_is_bounded_read_only_and_independent_of_retained_history() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-manual-reconcile-{}", Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    for directory in ["tv", "movies", "incoming"] {
        std::fs::create_dir(scratch.0.join(directory)).unwrap();
    }
    let source = scratch.0.join("incoming/source.mkv");
    std::fs::write(&source, b"reconcile must not touch these bytes").unwrap();
    let metadata = std::fs::metadata(&source).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'TV',?)",
        [scratch.0.join("tv").to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO seasons(series_id,number) VALUES(1,1); INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'),(2,1,1,2,'Two'); INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');").await.unwrap();
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(1,1,?)",
        [scratch.0.join("movies").to_str().unwrap()],
    )
    .await
    .unwrap();
    let tv = preview(
        db.clone(),
        source.to_str().unwrap().into(),
        scratch.0.join("tv/one.mkv").to_str().unwrap().into(),
        false,
        1,
    )
    .await;
    let movie = preview(
        db.clone(),
        source.to_str().unwrap().into(),
        scratch.0.join("movies/one.mkv").to_str().unwrap().into(),
        true,
        1,
    )
    .await;
    let unrelated = preview(
        db.clone(),
        source.to_str().unwrap().into(),
        scratch.0.join("tv/two.mkv").to_str().unwrap().into(),
        false,
        2,
    )
    .await;
    let missing = Uuid::new_v4();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = commands::router(db.clone()).merge(import::router(db.clone()));
    let mut server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let rows = reconcile(&client, &base, &c, &[movie, missing, tv]).await;
    assert_eq!(
        rows[0]["operation"],
        serde_json::to_value(
            import::status(db.clone(), &movie.to_string())
                .await
                .unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        rows[2]["operation"],
        serde_json::to_value(import::status(db.clone(), &tv.to_string()).await.unwrap()).unwrap()
    );
    assert_eq!(
        rows[0]["operation"]["target"],
        json!({"media_type":"movie","id":1})
    );
    assert_eq!(
        rows[2]["operation"]["target"],
        json!({"media_type":"episode","id":1})
    );
    assert!(rows.iter().all(|row| row["active_command"].is_null()));
    assert!(rows[1]["operation"].is_null());
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM manual_import_commands").await,
        0
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_history").await, 0);
    assert!(!scratch.0.join("tv/one.mkv").exists());
    assert!(!scratch.0.join("movies/one.mkv").exists());

    // Malformed stored state is not a missing operation and must reject the entire snapshot.
    // stage_json={} obeys the storage JSON constraint but cannot deserialize as an import Stage.
    c.execute(
        "UPDATE import_journal SET stage_json='{}' WHERE operation_id=?",
        [tv.to_string()],
    )
    .await
    .unwrap();
    let version = scalar(&c, "PRAGMA data_version").await;
    let (status, failure) = post(
        &client,
        &base,
        "/api/v1/manual-import/commands/reconcile",
        json!({"operation_ids":[movie,tv,missing]}),
    )
    .await;
    assert_eq!(status, 503, "{failure}");
    assert!(failure.get("error").is_some());
    assert_eq!(scalar(&c, "PRAGMA data_version").await, version);
    c.execute(
        "UPDATE import_journal SET stage_json=NULL WHERE operation_id=?",
        [tv.to_string()],
    )
    .await
    .unwrap();
    // Exercise the response bound with 100 actual canonical operations, not just small null rows.
    let mut normal_ids = vec![tv, movie, unrelated];
    for index in 0..97 {
        let is_movie = index % 2 == 0;
        let directory = if is_movie { "movies" } else { "tv" };
        normal_ids.push(
            preview(
                db.clone(),
                source.to_str().unwrap().into(),
                scratch
                    .0
                    .join(directory)
                    .join(format!("bounded-{index}.mkv"))
                    .to_str()
                    .unwrap()
                    .into(),
                is_movie,
                1,
            )
            .await,
        );
    }
    let normal_rows = reconcile(&client, &base, &c, &normal_ids).await;
    assert_eq!(normal_rows.len(), 100);
    assert!(
        normal_rows
            .iter()
            .all(|row| row["operation"].is_object() && row["active_command"].is_null())
    );

    let invalid = [
        json!({"operation_ids":[]}),
        json!({"operation_ids":[tv,tv]}),
        json!({"operation_ids":["not-a-uuid"]}),
        json!({"operation_ids":[null]}),
        json!({"operation_ids":[tv],"priority":"high"}),
        json!({}),
        json!({"operation_ids":(0..101).map(|_|Uuid::new_v4()).collect::<Vec<_>>()}),
    ];
    for body in invalid {
        let version = scalar(&c, "PRAGMA data_version").await;
        let (status, _) = post(
            &client,
            &base,
            "/api/v1/manual-import/commands/reconcile",
            body,
        )
        .await;
        assert_eq!(status, 400);
        assert_eq!(scalar(&c, "PRAGMA data_version").await, version);
    }
    assert_eq!(
        post(
            &client,
            &base,
            "/api/v1/manual-import/commands/reconcile?unknown=1",
            json!({"operation_ids":[tv]})
        )
        .await
        .0,
        400
    );
    let response = client
        .post(format!("{base}/api/v1/manual-import/commands/reconcile"))
        .header("content-type", "application/json")
        .body("{".to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 400);
    let response = client
        .post(format!("{base}/api/v1/manual-import/commands/reconcile"))
        .header("content-type", "application/json")
        .body(format!(
            "{{\"operation_ids\":[\"{}\"]}}{}",
            tv,
            " ".repeat(8192)
        ))
        .send()
        .await
        .unwrap();
    assert!(
        !response.status().is_success(),
        "Oversized body must be rejected"
    );
    let max_ids = (0..100).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
    assert!(
        reconcile(&client, &base, &c, &max_ids)
            .await
            .iter()
            .all(|row| row["operation"].is_null() && row["active_command"].is_null())
    );

    let (status, admitted) = post(
        &client,
        &base,
        "/api/v1/manual-import/commands",
        json!({"operation_ids":[tv,movie],"priority":"normal"}),
    )
    .await;
    assert_eq!(status, 202, "{admitted}");
    let tv_command = admitted[0]["id"].as_str().unwrap();
    let movie_command = admitted[1]["id"].as_str().unwrap();
    let rows = reconcile(&client, &base, &c, &[tv, movie]).await;
    assert_eq!(rows[0]["active_command"], admitted[0]);
    assert_eq!(rows[1]["active_command"], admitted[1]);
    c.execute(
        "UPDATE manual_import_commands SET status='running',attempts=1,started_at=0 WHERE id=?",
        [tv_command],
    )
    .await
    .unwrap();
    assert_eq!(
        reconcile(&client, &base, &c, &[tv]).await[0]["active_command"]["status"],
        "running"
    );
    c.execute(
        "UPDATE manual_import_commands SET status='retry_wait',error_code='import_busy' WHERE id=?",
        [tv_command],
    )
    .await
    .unwrap();
    assert_eq!(
        reconcile(&client, &base, &c, &[tv]).await[0]["active_command"]["status"],
        "retry_wait"
    );
    let (status, cancelled) = post(
        &client,
        &base,
        &format!("/api/v1/manual-import/commands/{tv_command}/cancel"),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "{cancelled}");
    // Retry-wait requires an error; cancellation must clear it atomically while retaining attempts.
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["error_code"], Value::Null);
    assert_eq!(cancelled["attempts"], 1);
    assert!(reconcile(&client, &base, &c, &[tv]).await[0]["active_command"].is_null());
    // Direct same-operation execution can complete while its queued wrapper still exists.
    // Reconciliation reports both current facts without claiming or settling the wrapper.
    assert_eq!(
        import::execute(db.clone(), &movie.to_string())
            .await
            .unwrap()
            .status,
        "complete"
    );
    let rows = reconcile(&client, &base, &c, &[movie]).await;
    assert_eq!(rows[0]["operation"]["status"], "complete");
    assert_eq!(rows[0]["active_command"]["status"], "queued");
    c.execute("UPDATE manual_import_commands SET status='failed',completed_at=0,error_code='interrupted' WHERE id=?", [movie_command]).await.unwrap();
    assert!(reconcile(&client, &base, &c, &[movie]).await[0]["active_command"].is_null());
    let known = client
        .get(format!(
            "{base}/api/v1/manual-import/commands/{movie_command}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        known.status().as_u16(),
        200,
        "Known terminal command detail remains independently available"
    );

    // Actual retained history exceeds the global list's offset ceiling. Every row follows legal
    // queued->cancelled transitions; terminal history must never hide a requested current command.
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    for _ in 0..1100 {
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO manual_import_commands(id,batch_id,operation_id,status,priority,next_attempt_at,created_at) VALUES(?,?,?,'queued',0,0,0)", params![id.clone(),Uuid::new_v4().to_string(),unrelated.to_string()]).await.unwrap();
        tx.execute(
            "UPDATE manual_import_commands SET status='cancelled',completed_at=0 WHERE id=?",
            [id],
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let (status, new_command) = post(
        &client,
        &base,
        "/api/v1/manual-import/commands",
        json!({"operation_ids":[tv],"priority":"high"}),
    )
    .await;
    assert_eq!(status, 202, "{new_command}");
    assert!(scalar(&c, "SELECT count(*) FROM manual_import_commands").await > 1024);
    assert_eq!(
        reconcile(&client, &base, &c, &[movie, tv, missing]).await[1]["active_command"],
        new_command[0]
    );
    // Simulate legitimate terminal-history pruning only; operation/journal authority survives.
    c.execute(
        "DELETE FROM manual_import_commands WHERE id IN (?,?)",
        params![tv_command, movie_command],
    )
    .await
    .unwrap();
    let rows = reconcile(&client, &base, &c, &[movie, tv, missing]).await;
    assert_eq!(rows[0]["operation"]["status"], "complete");
    assert!(rows[0]["active_command"].is_null());
    assert_eq!(rows[1]["active_command"], new_command[0]);
    assert!(rows[2]["operation"].is_null() && rows[2]["active_command"].is_null());
    let mut plan = c.query("EXPLAIN QUERY PLAN SELECT id,batch_id,operation_id,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code FROM manual_import_commands WHERE operation_id=? AND status IN ('queued','retry_wait','running') LIMIT 2", [tv.to_string()]).await.unwrap();
    let mut indexed = false;
    while let Some(row) = plan.next().await.unwrap() {
        let detail: String = row.get(3).unwrap();
        indexed |= detail.contains("SEARCH") && detail.contains("manual_import_commands_active");
        assert!(!detail.contains("SCAN manual_import_commands"), "{detail}");
    }
    assert!(
        indexed,
        "Current-command lookup must use the partial active index"
    );
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"reconcile must not touch these bytes"
    );
    let after = std::fs::metadata(&source).unwrap();
    assert_eq!(
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec()
        ),
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec()
        )
    );
    assert!(!scratch.0.join("tv/one.mkv").exists());
    assert!(!scratch.0.join("tv/two.mkv").exists());
    assert_eq!(
        std::fs::read(scratch.0.join("movies/one.mkv")).unwrap(),
        b"reconcile must not touch these bytes"
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM import_history").await,
        1,
        "Only the explicitly executed movie may import"
    );
    drop(plan);
    server.0.abort();
    let _ = (&mut server.0).await;
}
