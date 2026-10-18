//! Integration coverage for `src/commands/manual_import.rs`'s HTTP surface, run entirely through
//! real HTTP + a real worker (`commands::start_with_metadata`), never by calling
//! `crate::import::execute`/`manual_import::run` directly. Everything here shares one process
//! with the single global execution permit `src/import/mod.rs` guards
//! (`EXECUTING`/`ACTIVE_OPERATION`), so -- exactly like `tests/download_processing.rs` -- this
//! file contains exactly one `#[tokio::test]` that runs every scenario in sequence; two
//! concurrently-running `#[tokio::test]` functions in this same binary would otherwise collide on
//! that permit the same way the module's own removed unit tests did.
use axum::{
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{
    commands, db::Database, episodes, history, import, library, media_files,
    metadata::MetadataClient, providers, remote_paths,
};
use libsql::TransactionBehavior;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Server {
    async fn stop(mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn serve(app: axum::Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}"))
        },
    )
}

// A single generic metadata mock: any /shows/en/{tvdb_id} or /movie/{tmdb_id} is answered with a
// distinct one-episode show / one-file movie, so every scenario can mint as many independent
// library targets as it needs without a shared fixture.
async fn metadata_remote(uri: axum::http::Uri) -> Response {
    let path = uri.path();
    if let Some(rest) = path.strip_prefix("/shows/en/") {
        let id: i64 = rest.parse().expect("tvdb id");
        return axum::Json(json!({
            "tvdbId": id, "title": format!("Show{id}"),
            "seasons": [{"seasonNumber":1}],
            "episodes": [{"tvdbId":id*100+1,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":30,"airDateUtc":"2020-01-01T00:00:00Z"}]
        }))
        .into_response();
    }
    if let Some(rest) = path.strip_prefix("/movie/") {
        let id: i64 = rest.parse().expect("tmdb id");
        return axum::Json(
            json!({"tmdbId":id,"title":format!("Movie{id}"),"year":2020,"runtime":90,"digitalRelease":"2020-01-01T00:00:00Z"}),
        )
        .into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected metadata request").into_response()
}
fn manual_import_router(db: Arc<Database>, metadata: Arc<MetadataClient>) -> axum::Router {
    library::router(db.clone())
        .merge(library::metadata_router(db.clone(), metadata))
        .merge(episodes::router(db.clone()))
        .merge(import::router(db.clone()))
        .merge(commands::router(db.clone()))
        .merge(media_files::router(db.clone()))
        .merge(history::router(db))
}

fn media_json(kind: &str, id: i64) -> Value {
    json!({"media_type": kind, "id": id})
}
async fn add_series(base: &str, scratch: &Path, tvdb_id: i64) -> (PathBuf, i64) {
    let root = scratch.join(format!("tv-{tvdb_id}"));
    std::fs::create_dir(&root).unwrap();
    let (code, v) = request(
        base,
        "POST",
        "/api/v1/tv/series/lookup",
        json!({"tvdb_id":tvdb_id,"path":root,"settings":{"quality_profile_id":1,"series_type":"standard","use_scene_numbering":false,"monitored":true}}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let series_id = v["id"].as_i64().unwrap();
    let (code, episodes) = request(
        base,
        "GET",
        &format!("/api/v1/episodes?series_id={series_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{episodes}");
    (root, episodes["items"][0]["id"].as_i64().unwrap())
}
async fn add_movie(base: &str, scratch: &Path, tmdb_id: i64) -> (PathBuf, i64) {
    let root = scratch.join(format!("movie-{tmdb_id}"));
    std::fs::create_dir(&root).unwrap();
    let (code, v) = request(
        base,
        "POST",
        "/api/v1/movies/lookup",
        json!({"tmdb_id":tmdb_id,"path":root,"settings":{"quality_profile_id":2,"minimum_availability":"released","monitored":true}}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    (root, v["id"].as_i64().unwrap())
}
async fn preview_op(
    base: &str,
    target: Value,
    source: &Path,
    destination: &Path,
    content: &[u8],
) -> String {
    std::fs::write(source, content).unwrap();
    let (code, v) = request(
        base,
        "POST",
        "/api/v1/imports",
        json!({"target":target,"source":source,"destination":destination,"mode":"copy"}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    v["id"].as_str().unwrap().to_owned()
}
async fn submit_batch(base: &str, operation_ids: Vec<String>, priority: &str) -> (u16, Value) {
    request(
        base,
        "POST",
        "/api/v1/manual-import/commands",
        json!({"operation_ids":operation_ids,"priority":priority}),
    )
    .await
}
async fn wait_command(base: &str, id: &str, status: &str, timeout: Duration) -> Value {
    tokio::time::timeout(timeout, async {
        loop {
            let (code, v) = request(
                base,
                "GET",
                &format!("/api/v1/manual-import/commands/{id}"),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            if v["status"] == "failed" && status != "failed" {
                panic!("unexpected failure: {v}")
            }
            if v["status"] == "cancelled" && status != "cancelled" {
                panic!("unexpected cancellation: {v}")
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for status={status} on command {id}"))
}
fn credential_key() -> Arc<providers::CredentialKey> {
    Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap())
}

struct Ctx {
    scratch: Scratch,
    db: Arc<Database>,
    base: String,
    api: Server,
    upstream: Server,
    metadata: Arc<MetadataClient>,
}
impl Ctx {
    async fn finish(self) {
        self.api.stop().await;
        self.upstream.stop().await;
    }
}
async fn ctx_metadata_only(name: &str) -> Ctx {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-manual-import-cmd-{name}-{}",
        Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (origin, upstream) = serve(axum::Router::new().fallback(metadata_remote)).await;
    let metadata = Arc::new(
        MetadataClient::with_origins(&format!("{origin}/"), &format!("{origin}/")).unwrap(),
    );
    let (base, api) = serve(manual_import_router(db.clone(), metadata.clone())).await;
    db.connect()
        .await
        .unwrap()
        .execute_batch("INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');")
        .await
        .unwrap();
    Ctx {
        scratch,
        db,
        base,
        api,
        upstream,
        metadata,
    }
}

// Scenarios 1 (real end-to-end success, both domains) and 5 (equal numeric TV/movie IDs in one
// batch): a single fresh series and movie naturally collide at id=1/id=1, and this submits both
// as one batch through the real HTTP command routes and a real worker tick.
async fn end_to_end_success_and_equal_ids() {
    let ctx = ctx_metadata_only("e2e").await;
    let (tv_root, episode_id) = add_series(&ctx.base, &ctx.scratch.0, 101).await;
    let (movie_root, movie_id) = add_movie(&ctx.base, &ctx.scratch.0, 201).await;
    assert_eq!(episode_id, 1);
    assert_eq!(
        movie_id, 1,
        "colliding native IDs must retain typed targets"
    );
    let op_tv = preview_op(
        &ctx.base,
        media_json("episode", episode_id),
        &ctx.scratch.0.join("src-tv.mkv"),
        &tv_root.join("Episode.mkv"),
        b"tv-bytes",
    )
    .await;
    let op_movie = preview_op(
        &ctx.base,
        media_json("movie", movie_id),
        &ctx.scratch.0.join("src-movie.mkv"),
        &movie_root.join("Movie.mkv"),
        b"movie-bytes",
    )
    .await;
    let (_, client) = providers::router_with_refresh(ctx.db.clone(), Some(credential_key()));
    let runtime = commands::start_with_metadata(ctx.db.clone(), client, ctx.metadata.clone())
        .await
        .unwrap();
    let (code, created) =
        submit_batch(&ctx.base, vec![op_tv.clone(), op_movie.clone()], "normal").await;
    assert_eq!(code, 202, "{created}");
    let arr = created.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let batch_id = arr[0]["batch_id"].as_str().unwrap().to_owned();
    assert_eq!(
        arr[1]["batch_id"], batch_id,
        "batch shares one id across the whole submission"
    );
    let id_tv = arr.iter().find(|c| c["operation_id"] == op_tv).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id_movie = arr.iter().find(|c| c["operation_id"] == op_movie).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_command(&ctx.base, &id_tv, "succeeded", Duration::from_secs(10)).await;
    wait_command(&ctx.base, &id_movie, "succeeded", Duration::from_secs(10)).await;
    assert_eq!(
        std::fs::read(tv_root.join("Episode.mkv")).unwrap(),
        b"tv-bytes"
    );
    assert_eq!(
        std::fs::read(movie_root.join("Movie.mkv")).unwrap(),
        b"movie-bytes"
    );
    let (_, status_tv) = request(
        &ctx.base,
        "GET",
        &format!("/api/v1/imports/{op_tv}"),
        Value::Null,
    )
    .await;
    assert_eq!(status_tv["status"], "complete");
    let (_, status_movie) = request(
        &ctx.base,
        "GET",
        &format!("/api/v1/imports/{op_movie}"),
        Value::Null,
    )
    .await;
    assert_eq!(status_movie["status"], "complete");
    // Ordinary completed manual operations still fail readiness; ownership precedence must
    // not weaken the existing journal guard or create another command.
    for operation in [&op_tv, &op_movie] {
        let (code, rejected) = submit_batch(&ctx.base, vec![operation.clone()], "normal").await;
        assert_eq!(code, 409, "{rejected}");
        assert_eq!(
            rejected["error"]["code"], "manual_import_not_ready",
            "{rejected}"
        );
    }

    let (_, list) = request(
        &ctx.base,
        "GET",
        &format!("/api/v1/manual-import/commands?batch_id={batch_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(list["total"], 2);
    runtime.shutdown().await;
    ctx.finish().await;
}

// Scenario 2: a batch of 3 survives a restart (fresh Arc<Database>, fresh worker) mid-flight.
// Already-succeeded commands are never reclaimed/re-executed (unchanged completed_at and
// destination mtime); the still-queued one resumes and completes after the restart.
async fn restart_mid_batch_preserves_completed_and_resumes_rest() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-manual-import-cmd-restart-{}",
        Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (origin, upstream) = serve(axum::Router::new().fallback(metadata_remote)).await;
    let metadata = Arc::new(
        MetadataClient::with_origins(&format!("{origin}/"), &format!("{origin}/")).unwrap(),
    );
    let (base, api) = serve(manual_import_router(db.clone(), metadata.clone())).await;
    db.connect()
        .await
        .unwrap()
        .execute_batch("INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');")
        .await
        .unwrap();

    let (root1, ep1) = add_series(&base, &scratch.0, 161).await;
    let (root2, ep2) = add_series(&base, &scratch.0, 162).await;
    let (mroot, mid) = add_movie(&base, &scratch.0, 261).await;
    let entries: Vec<(String, PathBuf, Vec<u8>)> = vec![
        (
            preview_op(
                &base,
                media_json("episode", ep1),
                &scratch.0.join("r1.mkv"),
                &root1.join("e1.mkv"),
                b"one",
            )
            .await,
            root1.join("e1.mkv"),
            b"one".to_vec(),
        ),
        (
            preview_op(
                &base,
                media_json("episode", ep2),
                &scratch.0.join("r2.mkv"),
                &root2.join("e2.mkv"),
                b"two",
            )
            .await,
            root2.join("e2.mkv"),
            b"two".to_vec(),
        ),
        (
            preview_op(
                &base,
                media_json("movie", mid),
                &scratch.0.join("r3.mkv"),
                &mroot.join("m.mkv"),
                b"three",
            )
            .await,
            mroot.join("m.mkv"),
            b"three".to_vec(),
        ),
    ];

    let operation_ids: Vec<String> = entries.iter().map(|(op, _, _)| op.clone()).collect();
    let (code, created) = submit_batch(&base, operation_ids, "normal").await;
    assert_eq!(code, 202, "{created}");
    let command_ids: Vec<String> = created
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();

    // A future queued rescan excludes this movie's import from worker claims. Polling
    // alone can miss the entire two-completed/one-queued window. Use this existing
    // eligibility gate without bypassing the immutable manual-command transition guard.
    let held_rescan_id = Uuid::new_v4().to_string();
    db.connect()
        .await
        .unwrap()
        .execute(
            "INSERT INTO rescan_commands(id,media_type,movie_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,'movies',?,0,'queued',0,9007199254740991,0)",
            libsql::params![held_rescan_id.clone(), mid],
        )
        .await
        .unwrap();

    let (_, client) = providers::router_with_refresh(db.clone(), Some(credential_key()));
    let runtime = commands::start_with_metadata(db.clone(), client, metadata.clone())
        .await
        .unwrap();

    let settled = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut rows = Vec::new();
            for id in &command_ids {
                let (_, v) = request(
                    &base,
                    "GET",
                    &format!("/api/v1/manual-import/commands/{id}"),
                    Value::Null,
                )
                .await;
                rows.push(v);
            }
            let succeeded: Vec<Value> = rows
                .iter()
                .filter(|v| v["status"] == "succeeded")
                .cloned()
                .collect();
            let others: Vec<Value> = rows
                .iter()
                .filter(|v| v["status"] != "succeeded")
                .cloned()
                .collect();
            if succeeded.len() == 2 && others.len() == 1 && others[0]["status"] == "queued" {
                return (succeeded, others[0]["id"].as_str().unwrap().to_owned());
            }
            assert!(rows.iter().all(|v| v["status"] != "failed"), "{rows:?}");
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("first two commands did not complete while the third was held ineligible");
    let (succeeded, pending_command_id) = settled;

    let mut before = std::collections::HashMap::new();
    for v in &succeeded {
        let command_id = v["id"].as_str().unwrap().to_owned();
        let operation_id = v["operation_id"].as_str().unwrap();
        let (_, dest, content) = entries
            .iter()
            .find(|(op, _, _)| op == operation_id)
            .unwrap();
        assert_eq!(&std::fs::read(dest).unwrap(), content);
        before.insert(
            command_id,
            (
                v["completed_at"].clone(),
                std::fs::metadata(dest).unwrap().modified().unwrap(),
            ),
        );
    }

    runtime.shutdown().await;
    api.stop().await;
    assert_eq!(
        Arc::strong_count(&db),
        1,
        "all runtime/API database owners released before reopen"
    );
    drop(db);

    let db2 = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    // Clear the persisted eligibility gate after reopening, before worker restart.
    // Queued -> cancelled is a normal legal rescan transition; the import remains queued.
    db2.connect()
        .await
        .unwrap()
        .execute(
            "UPDATE rescan_commands SET status='cancelled',completed_at=0 WHERE id=? AND status='queued'",
            [held_rescan_id],
        )
        .await
        .unwrap();
    let (base2, api2) = serve(manual_import_router(db2.clone(), metadata.clone())).await;
    let (_, client2) = providers::router_with_refresh(db2.clone(), Some(credential_key()));
    let runtime2 = commands::start_with_metadata(db2.clone(), client2, metadata.clone())
        .await
        .unwrap();

    for (command_id, (completed_at, mtime)) in &before {
        let (_, v) = request(
            &base2,
            "GET",
            &format!("/api/v1/manual-import/commands/{command_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(
            v["status"], "succeeded",
            "already-succeeded command must not be reprocessed: {v}"
        );
        assert_eq!(
            &v["completed_at"], completed_at,
            "completion timestamp must not move on restart"
        );
        let operation_id = v["operation_id"].as_str().unwrap();
        let (_, dest, content) = entries
            .iter()
            .find(|(op, _, _)| op == operation_id)
            .unwrap();
        assert_eq!(&std::fs::read(dest).unwrap(), content);
        assert_eq!(
            &std::fs::metadata(dest).unwrap().modified().unwrap(),
            mtime,
            "destination must not be rewritten on restart"
        );
    }

    wait_command(
        &base2,
        &pending_command_id,
        "succeeded",
        Duration::from_secs(15),
    )
    .await;
    let (_, final_list) =
        request(&base2, "GET", "/api/v1/manual-import/commands", Value::Null).await;
    assert!(
        final_list["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] == "succeeded"),
        "{final_list}"
    );

    runtime2.shutdown().await;
    api2.stop().await;
    upstream.stop().await;
}

// Scenario 3: a `manual_import_commands` row left `running` while its `import_journal` is still
// non-terminal must never be touched by a per-tick `recover(..,"storage_error")` sweep, and must
// settle purely from the journal reaching a terminal state -- never from elapsed time, and never
// by `run()`/`execute()` being invoked a second time (this row is never `queued`/`retry_wait`, so
// `claim()` cannot select it either).
//
// NOT a genuine >45s wall-clock wait past the worker's real step timeout: no hook exists anywhere
// in `src/import` to make a real transfer slow, and this file may only add `tests/manual_import_commands.rs`
// (not a hook inside `src/import`, which the module's own docs say this needs). SQLite's
// `busy_timeout` is hard-coded to 5s in `Database::open_local` (`src/db/mod.rs`), so lock
// contention cannot fake a 45s+ stall either. Instead, the exact durable postcondition a real
// step-timeout drop leaves behind -- a `running` row over a non-terminal, error-free journal, with
// the real transfer still to complete -- is reproduced directly: the row is inserted as `queued`
// then updated to `running` inside one write transaction (matching `claim()`'s own UPDATE
// statement), so the live worker's own `claim()`/`recover()` can never observe the intermediate
// `queued` state (transactions are atomic to other connections). Everything downstream of that
// point -- the per-tick `recover()` calls that must ignore it, the real `execute()` call that
// finishes the transfer, and the next tick's `recover()` that must settle it -- is exercised with
// zero simulation, through real HTTP and a real running worker.
async fn recover_settles_a_running_row_only_from_terminal_journal_not_elapsed_time() {
    let ctx = ctx_metadata_only("timeout").await;
    let (root, episode_id) = add_series(&ctx.base, &ctx.scratch.0, 131).await;
    let source = ctx.scratch.0.join("slow-source.mkv");
    let destination = root.join("Slow.mkv");
    let op = preview_op(
        &ctx.base,
        media_json("episode", episode_id),
        &source,
        &destination,
        b"slow-transfer-bytes",
    )
    .await;

    let (_, client) = providers::router_with_refresh(ctx.db.clone(), Some(credential_key()));
    let runtime = commands::start_with_metadata(ctx.db.clone(), client, ctx.metadata.clone())
        .await
        .unwrap();

    let c = ctx.db.connect().await.unwrap();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let command_id = Uuid::new_v4().to_string();
    let batch_id = Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    tx.execute(
        "INSERT INTO manual_import_commands(id,batch_id,operation_id,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,0,'queued',0,?,?)",
        libsql::params![command_id.clone(), batch_id, op.clone(), now, now],
    )
    .await
    .unwrap();
    tx.execute(
        "UPDATE manual_import_commands SET status='running',attempts=1,started_at=?,error_code=NULL WHERE id=?",
        libsql::params![now, command_id.clone()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    drop(c);

    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(900)).await;
        let (code, v) = request(
            &ctx.base,
            "GET",
            &format!("/api/v1/manual-import/commands/{command_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(
            v["status"], "running",
            "a genuinely in-flight row must never be reset by a per-tick sweep: {v}"
        );
        assert!(v["error_code"].is_null());
    }

    let (code, v) = request(
        &ctx.base,
        "POST",
        &format!("/api/v1/imports/{op}/execute"),
        json!({}),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["status"], "complete");
    assert_eq!(std::fs::read(&destination).unwrap(), b"slow-transfer-bytes");

    let settled = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, v) = request(
                &ctx.base,
                "GET",
                &format!("/api/v1/manual-import/commands/{command_id}"),
                Value::Null,
            )
            .await;
            if v["status"] == "succeeded" {
                return v;
            }
            assert_ne!(v["status"], "failed", "{v}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("recover() did not settle the terminal journal within the expected number of ticks");
    assert!(settled["completed_at"].is_number());

    runtime.shutdown().await;
    ctx.finish().await;
}

// Scenario 4: a batch mixing a normal success with a deterministic, already-recognized execute()
// failure (the source file vanishing between preview and execution -- ENOENT inside
// `fs::child`, surfaced as `filesystem_error`). The sibling's failure must not block or abort the
// succeeding one, and both are queryable via the batch list endpoint.
async fn mixed_batch_success_and_failure_are_independent() {
    let ctx = ctx_metadata_only("mixed").await;
    let (root_ok, ep_ok) = add_series(&ctx.base, &ctx.scratch.0, 141).await;
    let (root_fail, ep_fail) = add_series(&ctx.base, &ctx.scratch.0, 142).await;
    let src_ok = ctx.scratch.0.join("ok.mkv");
    let src_fail = ctx.scratch.0.join("fail.mkv");
    let op_ok = preview_op(
        &ctx.base,
        media_json("episode", ep_ok),
        &src_ok,
        &root_ok.join("ok.mkv"),
        b"ok-bytes",
    )
    .await;
    let op_fail = preview_op(
        &ctx.base,
        media_json("episode", ep_fail),
        &src_fail,
        &root_fail.join("fail.mkv"),
        b"fail-bytes",
    )
    .await;
    std::fs::remove_file(&src_fail).unwrap();

    let (_, client) = providers::router_with_refresh(ctx.db.clone(), Some(credential_key()));
    let runtime = commands::start_with_metadata(ctx.db.clone(), client, ctx.metadata.clone())
        .await
        .unwrap();
    let (code, created) =
        submit_batch(&ctx.base, vec![op_ok.clone(), op_fail.clone()], "normal").await;
    assert_eq!(code, 202, "{created}");
    let arr = created.as_array().unwrap();
    let id_ok = arr.iter().find(|c| c["operation_id"] == op_ok).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id_fail = arr.iter().find(|c| c["operation_id"] == op_fail).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let batch_id = arr[0]["batch_id"].as_str().unwrap().to_owned();

    let ok = wait_command(&ctx.base, &id_ok, "succeeded", Duration::from_secs(10)).await;
    let failed = wait_command(&ctx.base, &id_fail, "failed", Duration::from_secs(10)).await;
    assert_eq!(failed["error_code"], "filesystem_error", "{failed}");
    assert!(ok["error_code"].is_null());
    assert_eq!(std::fs::read(root_ok.join("ok.mkv")).unwrap(), b"ok-bytes");

    let (_, list) = request(
        &ctx.base,
        "GET",
        &format!("/api/v1/manual-import/commands?batch_id={batch_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(list["total"], 2);
    let statuses: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["status"].as_str().unwrap().to_owned())
        .collect();
    assert!(statuses.contains(&"succeeded".to_owned()));
    assert!(statuses.contains(&"failed".to_owned()));

    runtime.shutdown().await;
    ctx.finish().await;
}

// Scenario 6's dedicated mock: a minimal, TV-only torznab+qbittorrent happy path (trimmed from
// `tests/download_processing.rs`'s `remote()`), just enough to reach a real
// `rss_candidate_imports` row with `operation_id` set, which is what
// `validate_admission()`/the `manual_import_commands_admit` trigger check.
#[derive(Default)]
struct OwnedRemote {
    items: Mutex<Vec<Value>>,
    completed: AtomicBool,
    endpoint: Mutex<String>,
}
const OWNED_HASH: &str = "6666666666666666666666666666666666666666";
async fn owned_remote(
    State(s): State<Arc<OwnedRemote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    _body: axum::body::Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/shows/en/171" {
        return axum::Json(json!({"tvdbId":171,"title":"Owned","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":9001,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":30,"airDateUtc":"2020-01-01T00:00:00Z"}]})).into_response();
    }
    if uri.path().ends_with("torrents/files") {
        let files = vec![
            json!({"index":0,"name":"batch/Owned.S01E01.1080p.WEB-DL.mkv","size":1024,"progress":1.0,"priority":1}),
        ];
        return axum::Json(json!(files)).into_response();
    }
    if uri.path().ends_with("torrents/properties") {
        return axum::Json(json!({"save_path":"/remote","total_size":1024,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
    }
    if uri.path() == "/api" || uri.path() == "/" {
        if q.get("t").is_some_and(|v| v == "caps") {
            return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/></searching><categories><category id="5000"><subcat id="5030"/></category></categories></caps>"#.into_response();
        }
        let date = "Mon, 01 Jan 2024 12:00:00 +0000";
        let link = format!(
            "{}/torrent?passkey=OWNED_LOCATOR",
            s.endpoint.lock().unwrap()
        );
        return format!(
            r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>Owned.S01E01.1080p.WEB-DL</title><guid>OWNED_GUID-{OWNED_HASH}</guid><pubDate>{date}</pubDate><link>{link}</link><torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{OWNED_HASH}"/><torznab:attr name="category" value="5030"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#
        )
        .into_response();
    }
    if uri.path().ends_with("webapiVersion") {
        return "2.8.4".into_response();
    }
    if uri.path().ends_with("preferences") {
        return axum::Json(json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if uri.path().ends_with("categories") {
        return axum::Json(json!({"tv":{"savePath":"/remote/tv"}})).into_response();
    }
    if uri.path().ends_with("torrents/info") {
        let items = s.items.lock().unwrap().clone();
        let items: Vec<Value> = items
            .into_iter()
            .map(|mut item| {
                if s.completed.load(Ordering::SeqCst) {
                    item["state"] = json!("uploading");
                    item["progress"] = json!(1.0);
                    item["amount_left"] = json!(0);
                    item["size"] = json!(1024);
                    item["save_path"] = json!("/remote");
                    item["content_path"] = json!("/remote/Owned.S01E01.1080p.WEB-DL.mkv");
                }
                item
            })
            .collect();
        return axum::Json(items).into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        s.items.lock().unwrap().push(json!({"hash":OWNED_HASH,"infohash_v1":OWNED_HASH,"infohash_v2":"","category":"tv","name":"owned item","state":"downloading","progress":0.5,"size":1024,"amount_left":512,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""}));
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response()
}

// Scenario 6: an operation already owned by the automated download-import pipeline (a real
// `rss_candidate_imports` row with `operation_id` set, produced through a real Add -> RSS ->
// grab -> completed-download flow) is rejected at submission with a clean 409, not silently
// accepted then stuck.
async fn operation_owned_by_download_rejected_at_submission() {
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "hrrdarr-manual-import-cmd-owned-{}",
        Uuid::new_v4()
    )));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(OwnedRemote::default());
    let (origin, upstream) = serve(
        axum::Router::new()
            .fallback(owned_remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        MetadataClient::with_origins(&format!("{origin}/"), &format!("{origin}/")).unwrap(),
    );
    let (provider_router, client) =
        providers::router_with_refresh(db.clone(), Some(credential_key()));
    let router = provider_router
        .merge(commands::router(db.clone()))
        .merge(library::router(db.clone()))
        .merge(library::metadata_router(db.clone(), metadata.clone()))
        .merge(episodes::router(db.clone()))
        .merge(remote_paths::router(db.clone()))
        .merge(import::router(db.clone()));
    let (base, api) = serve(router).await;
    let c = db.connect().await.unwrap();
    c.execute_batch("UPDATE quality_definitions SET min_size=0; INSERT INTO quality_profiles VALUES(1,'tv','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(1,'tv',7,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,7,0,0,1,NULL); INSERT INTO release_delay_policies VALUES('tv',0,0,0);").await.unwrap();

    let (code, indexer) = request(
        &base,
        "POST",
        "/api/v1/providers",
        json!({"name":"owned-indexer","enabled":true,"priority":1,"settings":{"implementation":"torznab","endpoint":origin,"tv":{"categories":[5030],"anime_categories":[]},"movies":null}}),
    )
    .await;
    assert_eq!(code, 201, "{indexer}");
    let (code, download) = request(
        &base,
        "POST",
        "/api/v1/providers",
        json!({"name":"owned-client","enabled":true,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":origin,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":null}}),
    )
    .await;
    assert_eq!(code, 201, "{download}");

    let root = scratch.0.join("tv");
    let source = scratch.0.join("source-tv");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(source.join("batch")).unwrap();
    let (code, v) = request(
        &base,
        "POST",
        "/api/v1/tv/series/lookup",
        json!({"tvdb_id":171,"path":root,"settings":{"quality_profile_id":1,"series_type":"standard","use_scene_numbering":false,"monitored":true}}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let (code, v) = request(
        &base,
        "POST",
        "/api/v1/tv/remote-path-mappings",
        json!({"host":"127.0.0.1","remote_path":"/remote","local_path":source}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    // Provider creation now installs an inherited policy. Preserve this scenario's
    // explicit policy edit, using its current CAS revision instead of claiming absence.
    let policy_path = format!(
        "/api/v1/download-processing/policies/{}/tv",
        download["id"].as_str().unwrap()
    );
    let (code, policy) = request(&base, "GET", &policy_path, Value::Null).await;
    assert_eq!(code, 200, "{policy}");
    assert_eq!(policy["enabled_override"], Value::Null);
    assert_eq!(policy["enabled"], true);
    assert_eq!(policy["mode"], "copy");
    let (code, v) = request(
        &base,
        "PUT",
        &policy_path,
        json!({"provider_revision":download["revision"],"revision":policy["revision"],"enabled":true,"mode":"copy"}),
    )
    .await;
    assert_eq!(code, 200, "{v}");

    let runtime = commands::start_with_metadata(db.clone(), client, metadata.clone())
        .await
        .unwrap();

    let (code, rss) = request(
        &base,
        "POST",
        "/api/v1/rss/commands",
        json!({"target":{"media_type":"tv","indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":download["id"],"client_revision":download["revision"]},"priority":"normal"}),
    )
    .await;
    assert_eq!(code, 202, "{rss}");
    let rss_id = rss["id"].as_str().unwrap().to_owned();
    let done = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, v) = request(
                &base,
                "GET",
                &format!("/api/v1/rss/commands/{rss_id}"),
                Value::Null,
            )
            .await;
            if v["status"] == "succeeded" {
                return v;
            }
            assert_ne!(v["status"], "failed", "{v}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("rss grab did not settle");
    assert_eq!(done["observed"], 1, "{done}");
    std::fs::write(
        source.join("batch").join("Owned.S01E01.1080p.WEB-DL.mkv"),
        vec![1u8; 1024],
    )
    .unwrap();

    let (code, candidates) = request(
        &base,
        "GET",
        &format!("/api/v1/rss/candidates?command_id={rss_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200, "{candidates}");
    let receipt = candidates["items"][0]["id"].clone();
    state.completed.store(true, Ordering::SeqCst);

    let (code, v) = request(
        &base,
        "POST",
        "/api/v1/download-processing",
        json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":"tv","receipt_ids":[receipt]}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let owned = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, v) = request(&base, "GET", &path, Value::Null).await;
            if !v["operation_id"].is_null() {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("owned import never produced an operation");
    let operation_id = owned["operation_id"].as_str().unwrap().to_owned();

    let (code, rejected) = submit_batch(&base, vec![operation_id.clone()], "normal").await;
    assert_eq!(code, 409, "{rejected}");
    assert_eq!(
        rejected["error"]["code"], "manual_import_owned_by_download",
        "{rejected}"
    );
    // Ownership remains the rejection reason after the automated worker leaves preview.
    // Waiting for a terminal journal makes this regression independent of polling speed.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, value) = request(&base, "GET", &path, Value::Null).await;
            if value["import_phase"] == "complete" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("owned import did not complete");
    let (code, rejected) = submit_batch(&base, vec![operation_id.clone()], "normal").await;
    assert_eq!(code, 409, "{rejected}");
    assert_eq!(
        rejected["error"]["code"], "manual_import_owned_by_download",
        "{rejected}"
    );
    assert_eq!(
        c.query(
            "SELECT count(*) FROM manual_import_commands WHERE operation_id=?",
            [operation_id]
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap(),
        0
    );

    drop(c);
    runtime.shutdown().await;
    api.stop().await;
    upstream.stop().await;
}

// Scenario 7: a direct `POST /api/v1/imports/{id}/execute` and a manual-import command submission
// (plus a real worker tick) racing over the same operation must always converge on exactly one
// real transfer and exactly one `import_history` row, regardless of which side wins
// `crate::import`'s single global execution permit.
async fn coexistence_race_produces_exactly_one_transfer() {
    let ctx = ctx_metadata_only("race").await;
    let (root, episode_id) = add_series(&ctx.base, &ctx.scratch.0, 151).await;
    let source = ctx.scratch.0.join("race.mkv");
    let destination = root.join("Race.mkv");
    let op = preview_op(
        &ctx.base,
        media_json("episode", episode_id),
        &source,
        &destination,
        b"race-bytes",
    )
    .await;

    let (_, client) = providers::router_with_refresh(ctx.db.clone(), Some(credential_key()));
    let runtime = commands::start_with_metadata(ctx.db.clone(), client, ctx.metadata.clone())
        .await
        .unwrap();

    let (code, created) = submit_batch(&ctx.base, vec![op.clone()], "normal").await;
    assert_eq!(code, 202, "{created}");
    let command_id = created[0]["id"].as_str().unwrap().to_owned();

    let base = ctx.base.clone();
    let op_direct = op.clone();
    let direct = tokio::spawn(async move {
        request(
            &base,
            "POST",
            &format!("/api/v1/imports/{op_direct}/execute"),
            json!({}),
        )
        .await
    });
    let (direct_code, direct_body) = direct.await.unwrap();
    assert!(
        direct_code == 200 || direct_code == 409,
        "direct execute must either win the permit or cleanly lose it: {direct_body}"
    );

    wait_command(&ctx.base, &command_id, "succeeded", Duration::from_secs(10)).await;
    assert_eq!(std::fs::read(&destination).unwrap(), b"race-bytes");

    let c = ctx.db.connect().await.unwrap();
    let count: i64 = c
        .query(
            "SELECT count(*) FROM import_history WHERE operation_id=?",
            [op.clone()],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        count, 1,
        "exactly one committed transfer for the raced operation"
    );
    drop(c);

    runtime.shutdown().await;
    ctx.finish().await;
}

#[tokio::test]
async fn manual_import_commands_http() {
    end_to_end_success_and_equal_ids().await;
    restart_mid_batch_preserves_completed_and_resumes_rest().await;
    recover_settles_a_running_row_only_from_terminal_journal_not_elapsed_time().await;
    mixed_batch_success_and_failure_are_independent().await;
    operation_owned_by_download_rejected_at_submission().await;
    coexistence_race_produces_exactly_one_transfer().await;
}
