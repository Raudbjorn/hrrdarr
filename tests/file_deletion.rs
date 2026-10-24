//! Explicit TV/movie file deletion, recycle-bin settings, CleanUpRecycleBin and hc.036 through
//! real HTTP routes and the owned command worker. Owned scratch directories and databases only.
use hrrdarr::{commands, db::Database, health, history, media_files, providers, recycle_bin};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("file deletion scratch cleanup failed: {error}");
        }
    }
}
struct App {
    base: String,
    client: reqwest::Client,
    db: Arc<Database>,
    server: tokio::task::JoinHandle<()>,
    runtime: Option<commands::Runtime>,
}
impl App {
    async fn start(db: Arc<Database>) -> Self {
        let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), None);
        let app = media_files::router(db.clone())
            .merge(recycle_bin::router(db.clone()))
            .merge(commands::router(db.clone()))
            .merge(health::router(db.clone()))
            .merge(history::router(db.clone()))
            .merge(provider_routes);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let runtime = commands::start(db.clone(), refresh).await.unwrap();
        Self {
            base,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            db,
            server,
            runtime: Some(runtime),
        }
    }
    async fn stop(mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
        self.server.abort();
        let _ = (&mut self.server).await;
    }
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        // The file-effect permit is process-wide; parallel tests retry only its busy refusal.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let mut request = self
                .client
                .request(method.clone(), format!("{}{path}", self.base));
            if let Some(body) = &body {
                request = request
                    .header("content-type", "application/json")
                    .body(body.to_string());
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let value: Value =
                serde_json::from_str(&response.text().await.unwrap()).unwrap_or(Value::Null);
            if status == 409
                && value["error"]["code"] == "import_busy"
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            return (status, value);
        }
    }
    async fn get(&self, path: &str) -> Value {
        let (status, body) = self.call(reqwest::Method::GET, path, None).await;
        assert_eq!(status, 200, "{path}: {body}");
        body
    }
    async fn delete(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::DELETE, path, Some(body)).await
    }
    async fn settings(&self, media: &str, path: Option<&Path>, days: i64) -> (u16, Value) {
        let current = self.get(&format!("/api/v1/{media}/recycle-bin")).await;
        self.call(
            reqwest::Method::PUT,
            &format!("/api/v1/{media}/recycle-bin"),
            Some(json!({"revision":current["revision"],"path":path.map(|p| p.to_str().unwrap()),"cleanup_days":days})),
        )
        .await
    }
    async fn scalar(&self, sql: &str) -> i64 {
        self.db
            .connect()
            .await
            .unwrap()
            .query(sql, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap()
    }
}
fn s(p: &Path) -> String {
    p.to_str().unwrap().to_owned()
}
/// Series 1 and movie 1 both own file ids 1 and 2 (colliding ids across domains). TV file 1 is a
/// multi-episode file shared by episodes 1 and 2.
async fn seed(db: &Database, root: &Path) -> Library {
    let tv = root.join("tv");
    let movies = root.join("movies");
    std::fs::create_dir_all(tv.join("Show/Season 1")).unwrap();
    std::fs::create_dir_all(movies.join("Film (2020)")).unwrap();
    std::fs::create_dir_all(movies.join("Other (2021)")).unwrap();
    let shared = tv.join("Show/Season 1/Show.S01E01E02.mkv");
    let single = tv.join("Show/Season 1/Show.S01E03.mkv");
    let film = movies.join("Film (2020)/Film.mkv");
    let other = movies.join("Other (2021)/Other.mkv");
    for (path, bytes) in [
        (&shared, &b"shared episode bytes"[..]),
        (&single, b"single episode bytes"),
        (&film, b"film bytes"),
        (&other, b"other film bytes"),
    ] {
        std::fs::write(path, bytes).unwrap();
    }
    let c = db.connect().await.unwrap();
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('tv',?1),('movies',?2)",
        libsql::params![s(&tv), s(&movies)],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'Show',?)",
        [s(&tv.join("Show"))],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO seasons VALUES(1,1,1);")
        .await
        .unwrap();
    c.execute(
        "INSERT INTO episode_files(id,series_id,path) VALUES(1,1,?1),(2,1,?2)",
        libsql::params![s(&shared), s(&single)],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'One',1),(2,1,1,2,'Two',1),(3,1,1,3,'Three',2);
        INSERT INTO file_metadata(media_type,episode_file_id,size) VALUES('tv',1,20),('tv',2,20);
        INSERT INTO movie_metadata(id,title) VALUES(1,'Film'),(2,'Other');").await.unwrap();
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(1,1,?1),(2,2,?2)",
        libsql::params![
            s(&movies.join("Film (2020)")),
            s(&movies.join("Other (2021)"))
        ],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,?1),(2,2,?2)",
        libsql::params![s(&film), s(&other)],
    )
    .await
    .unwrap();
    c.execute_batch(
        "INSERT INTO file_metadata(media_type,movie_file_id,size) VALUES('movies',1,10),('movies',2,16);",
    )
    .await
    .unwrap();
    Library {
        tv,
        movies,
        shared,
        single,
        film,
        other,
    }
}
struct Library {
    tv: PathBuf,
    movies: PathBuf,
    shared: PathBuf,
    single: PathBuf,
    film: PathBuf,
    other: PathBuf,
}
async fn sandbox(tag: &str) -> (Scratch, Arc<Database>, Library) {
    let root = std::env::temp_dir().join(format!(
        "hrrdarr-file-deletion-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    let scratch = Scratch(root.clone());
    let db = Arc::new(Database::open_local(root.join("library.db")).await.unwrap());
    let library = seed(&db, &root).await;
    (scratch, db, library)
}

#[tokio::test]
async fn recycle_both_domains_with_colliding_ids_keeps_bytes_layout_and_history() {
    let (scratch, db, lib) = sandbox("recycle").await;
    let bin = scratch.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let app = App::start(db.clone()).await;
    for media in ["tv", "movies"] {
        let (status, saved) = app.settings(media, Some(&bin), 7).await;
        assert_eq!(status, 200, "{saved}");
        assert_eq!(saved["path"], s(&bin));
    }
    // No implicit deletion: a body with an explicit mode is required.
    let (status, body) = app
        .call(reqwest::Method::DELETE, "/api/v1/tv/files/1", None)
        .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = app.delete("/api/v1/tv/files/1", json!({})).await;
    assert_eq!(status, 400, "{body}");
    // Permanent deletion is refused while a bin is configured.
    let (status, body) = app
        .delete("/api/v1/tv/files/1", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "recycle_bin_configured");
    assert!(lib.shared.exists());
    // TV file 1 (two episodes) is one deletion that detaches both episodes.
    let (status, tv) = app
        .delete("/api/v1/tv/files/1", json!({"mode":"recycle"}))
        .await;
    assert_eq!(status, 200, "{tv}");
    assert_eq!(tv["effect"], "recycled");
    assert_eq!(tv["media_type"], "tv");
    assert_eq!(
        tv["targets"],
        json!([{"media_type":"episode","id":1},{"media_type":"episode","id":2}])
    );
    let recycled = bin.join("Show/Season 1/Show.S01E01E02.mkv");
    assert_eq!(tv["recycle_path"], s(&recycled));
    assert!(!lib.shared.exists());
    assert_eq!(std::fs::read(&recycled).unwrap(), b"shared episode bytes");
    assert_eq!(
        app.scalar("SELECT count(*) FROM episodes WHERE episode_file_id IS NULL")
            .await,
        2
    );
    assert_eq!(app.scalar("SELECT count(*) FROM episode_files").await, 1);
    // The movie with the same numeric file id is untouched by the TV deletion.
    assert_eq!(
        app.scalar("SELECT count(*) FROM movie_files WHERE id=1")
            .await,
        1
    );
    assert!(lib.film.exists());
    let (status, body) = app
        .call(reqwest::Method::GET, "/api/v1/tv/files/1", None)
        .await;
    assert_eq!(status, 404, "deleted TV file must not be listed: {body}");
    let (status, movie) = app
        .delete("/api/v1/movies/files/1", json!({"mode":"recycle"}))
        .await;
    assert_eq!(status, 200, "{movie}");
    let recycled_film = bin.join("Film (2020)/Film.mkv");
    assert_eq!(movie["recycle_path"], s(&recycled_film));
    assert_eq!(std::fs::read(&recycled_film).unwrap(), b"film bytes");
    assert_eq!(movie["targets"], json!([{"media_type":"movie","id":1}]));
    // Name collision in the bin uses the upstream `_2` suffix and never overwrites.
    std::fs::write(&lib.film, b"replacement film").unwrap();
    let c = db.connect().await.unwrap();
    c.execute(
        "INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,?)",
        [s(&lib.film)],
    )
    .await
    .unwrap();
    drop(c);
    let (status, again) = app
        .delete("/api/v1/movies/files/1", json!({"mode":"recycle"}))
        .await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        again["recycle_path"],
        s(&bin.join("Film (2020)/Film_2.mkv"))
    );
    assert_eq!(std::fs::read(&recycled_film).unwrap(), b"film bytes");
    // History exposes typed deletion events per target, with the reason.
    let tv_history = app.get("/api/v1/history?media_type=tv").await;
    let deleted: Vec<&Value> = tv_history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["origin"] == "native_deletion")
        .collect();
    assert_eq!(deleted.len(), 2, "{tv_history}");
    for event in &deleted {
        assert_eq!(event["event_type"], "file_deleted");
        assert_eq!(event["reason"], "manual");
        assert_eq!(event["file"], json!({"media_type":"tv","id":1}));
        assert_eq!(event["deletion_id"], tv["id"]);
        assert_eq!(event["recycle_path"], s(&recycled));
    }
    let movie_history = app.get("/api/v1/history?movie_id=1").await;
    assert_eq!(movie_history["total"], 2);
    let by_episode = app.get("/api/v1/history?episode_id=2").await;
    assert_eq!(by_episode["total"], 1);
    let by_series = app.get("/api/v1/history?series_id=1&season=1").await;
    assert_eq!(by_series["total"], 2);
    app.stop().await;
    // Reopen: journal, history and recycled bytes persist.
    drop(db);
    let db = Arc::new(
        Database::open_local(scratch.0.join("library.db"))
            .await
            .unwrap(),
    );
    let app = App::start(db).await;
    assert_eq!(
        app.scalar("SELECT count(*) FROM file_deletions WHERE phase='complete'")
            .await,
        3
    );
    assert_eq!(app.get("/api/v1/history").await["total"], 4);
    assert!(lib.single.exists() && lib.other.exists());
    app.stop().await;
}

#[tokio::test]
async fn permanent_deletion_requires_explicit_mode_and_no_bin() {
    let (scratch, db, lib) = sandbox("permanent").await;
    let app = App::start(db).await;
    let (status, body) = app
        .delete("/api/v1/movies/files/2", json!({"mode":"recycle"}))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "recycle_bin_not_configured");
    assert!(lib.other.exists());
    let (status, body) = app
        .delete("/api/v1/movies/files/2", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["effect"], "deleted");
    assert_eq!(body["recycle_path"], Value::Null);
    assert!(!lib.other.exists());
    assert_eq!(
        app.scalar("SELECT count(*) FROM movie_files WHERE id=2")
            .await,
        0
    );
    // An already-missing file: only the record and associations are removed (upstream).
    std::fs::remove_file(&lib.single).unwrap();
    let (status, body) = app
        .delete("/api/v1/tv/files/2", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["effect"], "already_missing");
    assert_eq!(body["targets"], json!([{"media_type":"episode","id":3}]));
    // Unknown domain and wrong-domain ids never resolve across domains.
    let (status, _) = app
        .delete("/api/v1/music/files/1", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 400);
    let (status, body) = app
        .delete("/api/v1/movies/files/2", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 404, "{body}");
    assert!(lib.shared.exists() && lib.film.exists());
    let _ = scratch;
    app.stop().await;
}

#[tokio::test]
async fn bulk_is_bounded_atomic_in_validation_and_root_failsafe_refuses() {
    let (scratch, db, lib) = sandbox("bulk").await;
    let bin = scratch.0.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let app = App::start(db).await;
    assert_eq!(app.settings("tv", Some(&bin), 7).await.0, 200);
    let ids: Vec<i64> = (1..=51).collect();
    let (status, body) = app
        .delete(
            "/api/v1/tv/files/bulk",
            json!({"file_ids":ids,"mode":"recycle"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = app
        .delete(
            "/api/v1/tv/files/bulk",
            json!({"file_ids":[1,1],"mode":"recycle"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    // One missing id fails the whole request before any filesystem effect.
    let (status, body) = app
        .delete(
            "/api/v1/tv/files/bulk",
            json!({"file_ids":[1,2,99],"mode":"recycle"}),
        )
        .await;
    assert_eq!(status, 404, "{body}");
    assert!(lib.shared.exists() && lib.single.exists());
    assert_eq!(app.scalar("SELECT count(*) FROM file_deletions").await, 0);
    let (status, batch) = app
        .delete(
            "/api/v1/tv/files/bulk",
            json!({"file_ids":[2,1],"mode":"recycle"}),
        )
        .await;
    assert_eq!(status, 200, "{batch}");
    let ids: Vec<i64> = batch["deletions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["file_id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![2, 1]);
    assert_eq!(app.scalar("SELECT count(*) FROM episode_files").await, 0);
    assert_eq!(
        app.scalar("SELECT count(DISTINCT batch_id) FROM file_deletions")
            .await,
        1
    );
    // Root-folder failsafe: a root with no subfolders looks unmounted; nothing is deleted.
    let empty_root = scratch.0.join("empty-root");
    std::fs::create_dir(&empty_root).unwrap();
    let c = app.db.connect().await.unwrap();
    c.execute(
        "UPDATE root_folders SET path=? WHERE media_type='movies'",
        [s(&empty_root)],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE movies SET path=? WHERE id=1",
        [s(&empty_root.join("Film (2020)"))],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE movie_files SET path=? WHERE id=1",
        [s(&empty_root.join("Film (2020)/Film.mkv"))],
    )
    .await
    .unwrap();
    drop(c);
    let (status, body) = app
        .delete("/api/v1/movies/files/1", json!({"mode":"permanent"}))
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "library_root_empty");
    assert_eq!(
        app.scalar("SELECT count(*) FROM movie_files WHERE id=1")
            .await,
        1
    );
    let _ = (lib.movies, lib.tv);
    app.stop().await;
}

#[tokio::test]
async fn settings_validate_paths_overlap_revision_and_both_domain_isolation() {
    let (scratch, db, lib) = sandbox("settings").await;
    let app = App::start(db).await;
    let tv = app.get("/api/v1/tv/recycle-bin").await;
    assert_eq!(
        tv,
        json!({"media_type":"tv","revision":1,"path":null,"cleanup_days":7})
    );
    for (path, code) in [
        (json!("relative/bin"), 400),
        (json!("/a/../b"), 400),
        (json!("/"), 400),
        (json!(s(&lib.tv)), 409),
        (json!(s(&lib.tv.join("Show/inside"))), 409),
        (json!(s(&scratch.0)), 409),
    ] {
        let (status, body) = app
            .call(
                reqwest::Method::PUT,
                "/api/v1/tv/recycle-bin",
                Some(json!({"revision":1,"path":path,"cleanup_days":7})),
            )
            .await;
        assert_eq!(status, code, "{path}: {body}");
    }
    let (status, body) = app
        .call(
            reqwest::Method::PUT,
            "/api/v1/tv/recycle-bin",
            Some(json!({"revision":1,"cleanup_days":7})),
        )
        .await;
    assert_eq!(status, 400, "path is required: {body}");
    let (status, body) = app
        .call(
            reqwest::Method::PUT,
            "/api/v1/tv/recycle-bin",
            Some(json!({"revision":1,"path":null,"cleanup_days":36501})),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    // Normalized, saved without requiring existence (Health reports a missing bin).
    let bin = scratch.0.join("bins/tv");
    let (status, saved) = app
        .call(
            reqwest::Method::PUT,
            "/api/v1/tv/recycle-bin",
            Some(json!({"revision":1,"path":format!("{}//", s(&bin)),"cleanup_days":0})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["path"], s(&bin));
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["cleanup_days"], 0);
    let (status, body) = app
        .call(
            reqwest::Method::PUT,
            "/api/v1/tv/recycle-bin",
            Some(json!({"revision":1,"path":null,"cleanup_days":7})),
        )
        .await;
    assert_eq!(status, 409, "stale revision: {body}");
    assert_eq!(body["error"]["code"], "recycle_bin_conflict");
    assert_eq!(
        app.get("/api/v1/movies/recycle-bin").await["path"],
        Value::Null
    );
    let (status, body) = app.settings("movies", Some(&bin.join("nested")), 7).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "recycle_bin_nested");
    // Shared bins use one retention because cleanup has no domain marker in the path.
    let (status, body) = app.settings("movies", Some(&bin), 7).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "recycle_bin_retention_conflict");
    assert_eq!(app.settings("movies", Some(&bin), 0).await.0, 200);
    // Inverse guard: a new library declaration inside the bin is refused by storage.
    let c = app.db.connect().await.unwrap();
    assert!(
        c.execute(
            "INSERT INTO root_folders(media_type,path) VALUES('movies',?)",
            [s(&bin.join("library"))]
        )
        .await
        .is_err()
    );
    drop(c);
    // Null clears the bin.
    let (status, cleared) = app.settings("tv", None, 7).await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(cleared["path"], Value::Null);
    app.stop().await;
}

fn age(path: &Path, days: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(days * 86_400 + 60))
        .unwrap();
}
async fn wait_command(app: &App, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let command = app
                .get(&format!("/api/v1/recycle-bin/cleanup-commands/{id}"))
                .await;
            if ["succeeded", "failed", "cancelled"].contains(&command["status"].as_str().unwrap()) {
                return command;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("cleanup command must settle")
}

#[tokio::test]
async fn cleanup_command_removes_only_expired_files_inside_the_bin() {
    let (scratch, db, _lib) = sandbox("cleanup").await;
    let bin = scratch.0.join("bin");
    let outside = scratch.0.join("outside");
    std::fs::create_dir_all(bin.join("Show/Season 1")).unwrap();
    std::fs::create_dir_all(bin.join("Fresh")).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let old = bin.join("Show/Season 1/old.mkv");
    let fresh = bin.join("Fresh/new.mkv");
    let outside_old = outside.join("old.mkv");
    for path in [&old, &fresh, &outside_old] {
        std::fs::write(path, b"bytes").unwrap();
    }
    age(&old, 8);
    age(&outside_old, 30);
    std::os::unix::fs::symlink(&outside_old, bin.join("expired-link.mkv")).unwrap();
    std::os::unix::fs::symlink(&outside, bin.join("outside-dir")).unwrap();
    let app = App::start(db).await;
    // Not configured: succeeds with an explicit outcome and touches nothing.
    let (status, queued) = app
        .call(
            reqwest::Method::POST,
            "/api/v1/recycle-bin/cleanup-commands",
            Some(json!({"target":{"media_type":"tv"},"priority":"high"})),
        )
        .await;
    assert_eq!(status, 202, "{queued}");
    let done = wait_command(&app, queued["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["outcome"], "not_configured");
    assert!(old.exists());
    assert_eq!(app.settings("tv", Some(&bin), 0).await.0, 200);
    let (_, queued) = app
        .call(
            reqwest::Method::POST,
            "/api/v1/recycle-bin/cleanup-commands",
            Some(json!({"target":{"media_type":"tv"},"priority":"normal"})),
        )
        .await;
    let done = wait_command(&app, queued["id"].as_str().unwrap()).await;
    assert_eq!(done["outcome"], "disabled", "{done}");
    assert!(old.exists());
    assert_eq!(app.settings("tv", Some(&bin), 7).await.0, 200);
    let (_, queued) = app
        .call(
            reqwest::Method::POST,
            "/api/v1/recycle-bin/cleanup-commands",
            Some(json!({"target":{"media_type":"tv"},"priority":"normal"})),
        )
        .await;
    let done = wait_command(&app, queued["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["outcome"], "cleaned");
    assert_eq!(done["origin"], "manual");
    assert_eq!(done["files_removed"], 1);
    assert_eq!(done["folders_removed"], 2);
    assert_eq!(
        done["entries_skipped"], 2,
        "symlinks are kept, never followed"
    );
    assert_eq!(done["bin_path"], s(&bin));
    assert!(!old.exists());
    assert!(fresh.exists());
    assert!(outside_old.exists(), "nothing outside the bin is removed");
    assert!(bin.join("expired-link.mkv").symlink_metadata().is_ok());
    // The movie domain's command list is separate.
    let movies = app
        .get("/api/v1/recycle-bin/cleanup-commands?media_type=movies")
        .await;
    assert_eq!(movies["total"], 0);
    // Scheduled: a due domain gets one automatic command; the next run moves a day ahead.
    let aged = bin.join("Fresh/aged.mkv");
    std::fs::write(&aged, b"bytes").unwrap();
    age(&aged, 9);
    let c = app.db.connect().await.unwrap();
    c.execute_batch("UPDATE recycle_bin_settings SET next_cleanup_at=0 WHERE media_type='tv';")
        .await
        .unwrap();
    drop(c);
    let automatic = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let page = app
                .get("/api/v1/recycle-bin/cleanup-commands?media_type=tv&status=succeeded")
                .await;
            if let Some(found) = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["origin"] == "automatic")
            {
                return found.clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("scheduled cleanup must run");
    assert_eq!(automatic["files_removed"], 1, "{automatic}");
    assert!(!aged.exists());
    assert!(
        app.scalar(
            "SELECT next_cleanup_at-unixepoch() FROM recycle_bin_settings WHERE media_type='tv'"
        )
        .await
            > 86_000
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM recycle_bin_cleanup_commands WHERE origin='automatic'")
            .await,
        1
    );
    // A missing bin fails visibly without retries.
    std::fs::remove_dir_all(&bin).unwrap();
    let (_, queued) = app
        .call(
            reqwest::Method::POST,
            "/api/v1/recycle-bin/cleanup-commands",
            Some(json!({"target":{"media_type":"tv"},"priority":"normal"})),
        )
        .await;
    let done = wait_command(&app, queued["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed", "{done}");
    assert_eq!(done["error_code"], "recycle_bin_unavailable");
    app.stop().await;
}

async fn recycle_check(app: &App, scope: &str, generation_after: i64) -> (Value, Vec<Value>) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = app
                .get(&format!("/api/v1/health?scope={scope}&limit=16"))
                .await;
            let check = snapshot["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["identity"]["check_key"] == "recycle_bin")
                .cloned()
                .unwrap();
            if check["evaluation"] == "current"
                && check["observed_generation"].as_i64().unwrap() >= generation_after
            {
                let issues = snapshot["issues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|i| i["identity"]["check_key"] == "recycle_bin")
                    .cloned()
                    .collect();
                return (check, issues);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("recycle bin health check must be evaluated")
}
async fn run_health(app: &App, scope: &str) {
    let (status, body) = app
        .call(
            reqwest::Method::POST,
            "/api/v1/health/commands",
            Some(json!({"scope":scope,"priority":"high"})),
        )
        .await;
    assert!(matches!(status, 200 | 202), "{body}");
}

#[tokio::test]
async fn recycle_bin_health_reports_missing_bin_and_restores_per_domain() {
    let (scratch, db, _lib) = sandbox("health").await;
    let bin = scratch.0.join("bin");
    let app = App::start(db).await;
    let (check, issues) = recycle_check(&app, "tv", 0).await;
    assert!(issues.is_empty(), "unset bin is healthy: {check}");
    // Configured but missing: Error with the upstream compatibility type, TV only.
    assert_eq!(app.settings("tv", Some(&bin), 7).await.0, 200);
    let generation = app
        .scalar("SELECT generation FROM health_checks WHERE scope='tv' AND check_key='recycle_bin'")
        .await;
    run_health(&app, "tv").await;
    let (_, issues) = recycle_check(&app, "tv", generation).await;
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0]["severity"], "error");
    assert_eq!(issues[0]["reason"], "recycle_bin_missing");
    assert_eq!(issues[0]["compatibility_type"], "RecyclingBinCheck");
    assert!(
        issues[0]["wiki_url"]
            .as_str()
            .unwrap()
            .ends_with("#cannot-write-recycle-bin")
    );
    run_health(&app, "movies").await;
    let (_, movie_issues) = recycle_check(&app, "movies", 0).await;
    assert!(movie_issues.is_empty());
    // Creating the folder restores health on the next evaluation.
    std::fs::create_dir(&bin).unwrap();
    let generation = app
        .scalar("SELECT generation FROM health_checks WHERE scope='tv' AND check_key='recycle_bin'")
        .await;
    run_health(&app, "tv").await;
    let (_, issues) = recycle_check(&app, "tv", generation).await;
    assert!(issues.is_empty(), "{issues:?}");
    // The probe leaves nothing in the bin.
    assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 0);
    app.stop().await;
}
