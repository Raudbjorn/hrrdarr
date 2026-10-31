//! Rename planning, execution, journal recovery, events and HTTP surface (cmd.011/012, api.044).
//! Real files in a private scratch directory, a real database and the real command worker.
use axum::Router;
use hrrdarr::{commands, db::Database, naming, providers, rename::events};
use libsql::{Connection, params};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
type R<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-rename-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn s(&self, rel: &str) -> String {
        format!("{}/{rel}", self.0.display())
    }
    fn write(&self, rel: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rel.as_bytes()).unwrap();
    }
    fn exists(&self, rel: &str) -> bool {
        self.0.join(rel).exists()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Api {
    origin: String,
    task: tokio::task::JoinHandle<()>,
    http: reqwest::Client,
    client: providers::RefreshClient,
}
impl Api {
    async fn start(db: Arc<Database>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (p, client) = providers::router_with_refresh(db.clone(), None);
        let router = Router::new()
            .merge(p)
            .merge(naming::router(db.clone()))
            .merge(commands::router(db));
        Self {
            origin,
            task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            client,
        }
    }
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.origin));
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    async fn ok(&self, method: &str, path: &str, body: Option<Value>, expect: u16) -> Value {
        let (status, v) = self.call(method, path, body).await;
        assert_eq!(status, expect, "{method} {path}: {v}");
        v
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn naming_tv(
    c: &Connection,
    enabled: bool,
    std: &str,
    season: Option<&str>,
    style: Option<i64>,
) {
    c.execute("UPDATE naming_settings SET rename_enabled=?,standard_episode_format=?,season_folder_format=?,specials_folder_format='Specials',series_folder_format='{Series Title}',multi_episode_style=?,revision=revision+1 WHERE domain='tv'", params![i64::from(enabled), std, season, style]).await.unwrap();
}
async fn naming_movies(c: &Connection, enabled: bool, std: &str, folder: Option<&str>) {
    c.execute("UPDATE naming_settings SET rename_enabled=?,standard_movie_format=?,movie_folder_format=?,revision=revision+1 WHERE domain='movies'", params![i64::from(enabled), std, folder]).await.unwrap();
}
async fn revision(c: &Connection, d: &str) -> i64 {
    c.query("SELECT revision FROM naming_settings WHERE domain=?", [d])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn seed_series(c: &Connection, id: i64, title: &str, path: &str, season_folder: Option<i64>) {
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(?,?,?)",
        params![id, title, path],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO library_settings(media_type,series_id,series_type,season_folder) VALUES('tv',?,'standard',?)", params![id, season_folder]).await.unwrap();
}
async fn seed_episode(
    c: &Connection,
    id: i64,
    series: i64,
    season: i64,
    number: i64,
    title: &str,
    file: Option<i64>,
) {
    c.execute(
        "INSERT OR IGNORE INTO seasons(series_id,number) VALUES(?,?)",
        params![series, season],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(?,?,?,?,?,?)", params![id, series, season, number, title, file]).await.unwrap();
}
async fn seed_file(c: &Connection, id: i64, series: i64, path: &str) {
    c.execute(
        "INSERT INTO episode_files(id,series_id,path) VALUES(?,?,?)",
        params![id, series, path],
    )
    .await
    .unwrap();
}
async fn seed_movie(c: &Connection, id: i64, title: &str, path: &str, file: Option<(i64, &str)>) {
    c.execute(
        "INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(?,?,?,2020)",
        params![id + 1000, id + 9000, title],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(?,?,?)",
        params![id, id + 1000, path],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO library_settings(media_type,movie_id) VALUES('movies',?)",
        [id],
    )
    .await
    .unwrap();
    if let Some((fid, p)) = file {
        c.execute(
            "INSERT INTO movie_files(id,movie_id,path) VALUES(?,?,?)",
            params![fid, id, p],
        )
        .await
        .unwrap();
    }
}
async fn path_of(c: &Connection, table: &str, id: i64) -> String {
    c.query(&format!("SELECT path FROM {table} WHERE id=?"), [id])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn wait(api: &Api, id: &str) -> Value {
    for _ in 0..200 {
        let v = api
            .ok("GET", &format!("/api/v1/rename/commands/{id}"), None, 200)
            .await;
        if matches!(
            v["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("command did not finish")
}
async fn command(api: &Api, body: Value) -> Value {
    let created = api
        .ok("POST", "/api/v1/rename/commands", Some(body), 202)
        .await;
    wait(api, created["id"].as_str().unwrap()).await
}
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn setup() -> R<(
    tokio::sync::MutexGuard<'static, ()>,
    Scratch,
    Arc<Database>,
    Connection,
    Api,
)> {
    // The import/file-operation permit is process-wide; tests using the worker must not overlap.
    let lock = SERIAL.lock().await;
    let s = Scratch::new();
    std::fs::create_dir(s.0.join("db"))?;
    let db = Arc::new(Database::open_local(s.0.join("db/h")).await?);
    let c = db.connect().await?;
    let api = Api::start(db.clone()).await;
    Ok((lock, s, db, c, api))
}
fn worker(db: &Arc<Database>, api: &Api) -> impl std::future::Future<Output = commands::Runtime> {
    let (db, client) = (db.clone(), api.client.clone());
    async move { commands::start(db, client).await.unwrap() }
}

#[tokio::test]
async fn tv_preview_groups_files_places_season_folders_and_reports_collisions() -> R {
    let (_lock, s, _db, c, api) = setup().await?;
    seed_series(&c, 1, "Show", &s.s("lib/Show"), Some(1)).await;
    s.write("lib/Show/old one.mkv");
    s.write("lib/Show/old one.en.srt");
    s.write("lib/Show/pair.mkv");
    s.write("lib/Show/orphan.mkv");
    s.write("lib/Show/Season 02/Show - S02E01-Pilot.mkv");
    s.write("lib/Show/special.mkv");
    s.write("lib/Show/taken.mkv");
    s.write("lib/Show/Season 01/Show - S01E04-Four.mkv");
    seed_file(&c, 1, 1, &s.s("lib/Show/old one.mkv")).await;
    seed_file(&c, 2, 1, &s.s("lib/Show/pair.mkv")).await;
    seed_file(&c, 3, 1, &s.s("lib/Show/orphan.mkv")).await;
    seed_file(&c, 4, 1, &s.s("lib/Show/Season 02/Show - S02E01-Pilot.mkv")).await;
    seed_file(&c, 5, 1, &s.s("lib/Show/special.mkv")).await;
    seed_file(&c, 6, 1, &s.s("lib/Show/taken.mkv")).await;
    seed_file(&c, 7, 1, &s.s("lib/Show/Season 01/Show - S01E04-Four.mkv")).await;
    seed_episode(&c, 1, 1, 1, 1, "One", Some(1)).await;
    seed_episode(&c, 2, 1, 1, 2, "Two", Some(2)).await;
    seed_episode(&c, 3, 1, 1, 3, "Three", Some(2)).await;
    seed_episode(&c, 4, 1, 2, 1, "Pilot", Some(4)).await;
    seed_episode(&c, 5, 1, 0, 1, "Special", Some(5)).await;
    seed_episode(&c, 6, 1, 1, 4, "Four", Some(7)).await;
    seed_episode(&c, 7, 1, 1, 5, "Five", Some(6)).await;
    s.write("lib/Show/Season 01/Show - S01E05-Five.mkv");
    naming_tv(
        &c,
        true,
        "{Series Title} - S{season:00}E{episode:00}-{Episode Title}",
        Some("Season {season:00}"),
        Some(4),
    )
    .await;
    let before: Vec<_> = std::fs::read_dir(s.0.join("lib/Show"))?
        .map(|e| e.unwrap().file_name())
        .collect();
    let v = api
        .ok("GET", "/api/v1/tv/rename-preview?series_id=1", None, 200)
        .await;
    assert_eq!(v["rename_enabled"], true);
    assert_eq!(v["files_considered"], 7);
    let items = v["items"].as_array().unwrap();
    let by = |id: i64| items.iter().find(|i| i["episode_file_id"] == id).unwrap();
    assert_eq!(by(1)["new_path"], "Season 01/Show - S01E01-One.mkv");
    assert_eq!(by(1)["existing_path"], "old one.mkv");
    // Two episodes in one physical file: one item with the complete numbering, range style.
    assert_eq!(
        by(2)["new_path"],
        "Season 01/Show - S01E02-03-Two + Three.mkv"
    );
    assert_eq!(by(2)["episode_numbers"], json!([2, 3]));
    assert_eq!(by(3)["status"], "unavailable");
    assert_eq!(by(3)["reasons"], json!(["no_linked_episodes"]));
    assert_eq!(by(5)["new_path"], "Specials/Show - S00E01-Special.mkv");
    assert_eq!(by(6)["status"], "blocked");
    assert_eq!(by(6)["reasons"], json!(["destination_exists"]));
    assert!(
        items.iter().all(|i| i["episode_file_id"] != 4),
        "already correct file is unchanged"
    );
    assert_eq!(v["unchanged_count"], 2);
    assert_eq!(v["blocked_count"], 1);
    // Season filter and bulk selector, read-only.
    let season = api
        .ok(
            "GET",
            "/api/v1/tv/rename-preview?series_id=1&season=2",
            None,
            200,
        )
        .await;
    assert_eq!(season["items"].as_array().unwrap().len(), 0);
    api.ok("GET", "/api/v1/tv/rename-preview?series_ids=1", None, 200)
        .await;
    for bad in [
        "",
        "?series_id=0",
        "?series_id=1,2",
        "?movie_ids=1",
        "?series_id=1&series_ids=1",
        "?series_id=x",
        "?series_ids=1,1",
    ] {
        api.ok("GET", &format!("/api/v1/tv/rename-preview{bad}"), None, 400)
            .await;
    }
    api.ok("GET", "/api/v1/tv/rename-preview?series_id=99", None, 404)
        .await;
    let after: Vec<_> = std::fs::read_dir(s.0.join("lib/Show"))?
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        before.len(),
        after.len(),
        "preview never changes the filesystem"
    );
    assert_eq!(
        path_of(&c, "episode_files", 1).await,
        s.s("lib/Show/old one.mkv")
    );
    Ok(())
}

#[tokio::test]
async fn rename_files_moves_media_and_sidecars_commits_db_and_event_then_is_idempotent() -> R {
    let (_lock, s, db, c, api) = setup().await?;
    seed_series(&c, 1, "Show", &s.s("lib/Show"), Some(1)).await;
    s.write("lib/Show/old one.mkv");
    s.write("lib/Show/old one.en.srt");
    s.write("lib/Show/old one.nfo");
    s.write("lib/Show/old one2.srt");
    s.write("lib/Show/two.mkv");
    seed_file(&c, 1, 1, &s.s("lib/Show/old one.mkv")).await;
    seed_file(&c, 2, 1, &s.s("lib/Show/two.mkv")).await;
    seed_episode(&c, 1, 1, 1, 1, "One", Some(1)).await;
    seed_episode(&c, 2, 1, 1, 2, "Two", Some(2)).await;
    naming_tv(
        &c,
        true,
        "{Series Title} - S{season:00}E{episode:00}",
        Some("Season {season:00}"),
        None,
    )
    .await;
    let rev = revision(&c, "tv").await;
    // Stale revision fence.
    let (st, _) = api.call("POST", "/api/v1/rename/commands", Some(json!({"name":"rename_files","target":{"media_type":"tv","series_ids":[1],"file_ids":[1]},"expected_naming_revision":rev-1,"priority":"normal"}))).await;
    assert_eq!(st, 409);
    let runtime = worker(&db, &api).await;
    let done = command(&api, json!({"name":"rename_files","target":{"media_type":"tv","series_ids":[1],"file_ids":[1]},"expected_naming_revision":rev,"priority":"normal"})).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["items_renamed"], 1);
    assert_eq!(done["items"][0]["state"], "committed");
    assert_eq!(done["items"][0]["sidecar_count"], 2);
    assert!(s.exists("lib/Show/Season 01/Show - S01E01.mkv"));
    assert!(s.exists("lib/Show/Season 01/Show - S01E01.en.srt"));
    assert!(s.exists("lib/Show/Season 01/Show - S01E01.nfo"));
    assert!(!s.exists("lib/Show/old one.mkv") && !s.exists("lib/Show/old one.en.srt"));
    assert!(
        s.exists("lib/Show/old one2.srt"),
        "a different stem is not a sidecar"
    );
    assert!(s.exists("lib/Show/two.mkv"), "unselected file untouched");
    assert_eq!(
        path_of(&c, "episode_files", 1).await,
        s.s("lib/Show/Season 01/Show - S01E01.mkv")
    );
    // Durable, domain-typed event with the same transaction.
    let ev = api
        .ok("GET", "/api/v1/rename/events?media_type=tv", None, 200)
        .await;
    assert_eq!(ev["total"], 1);
    assert_eq!(ev["items"][0]["event_type"], "rename");
    assert_eq!(ev["items"][0]["owner"], json!({"media_type":"tv","id":1}));
    assert_eq!(ev["items"][0]["sidecar_count"], 2);
    let pending = events::pending(&c, 10).await?;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        events::mark_dispatched(&c, &[pending[0].id.clone()]).await?,
        1
    );
    assert_eq!(events::pending(&c, 10).await?.len(), 0);
    // Rename the rest: previously renamed file now counts as unchanged; rerun is a no-op.
    let all = command(&api, json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(all["items_renamed"], 1);
    assert_eq!(all["items_unchanged"], 1);
    let again = command(&api, json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(
        (
            again["items_renamed"].as_i64(),
            again["items_unchanged"].as_i64()
        ),
        (Some(0), Some(2))
    );
    assert_eq!(again["status"], "succeeded");
    assert!(s.exists("lib/Show/Season 01/Show - S01E02.mkv"));
    // A foreign file ID is not accepted for this owner.
    let (st, _) = api.call("POST", "/api/v1/rename/commands", Some(json!({"name":"rename_files","target":{"media_type":"tv","series_ids":[1],"file_ids":[99]},"priority":"normal"}))).await;
    assert_eq!(st, 404);
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn collisions_and_unsafe_paths_fail_the_item_without_partial_state() -> R {
    let (_lock, s, db, c, api) = setup().await?;
    seed_series(&c, 1, "Show", &s.s("lib/Show"), Some(1)).await;
    s.write("lib/Show/a.mkv");
    s.write("lib/Show/a.srt");
    s.write("lib/Show/b.mkv");
    s.write("lib/Show/Season 01/Show - S01E02.srt");
    s.write("lib/Show/c.mkv");
    s.write("outside/c-real.mkv");
    seed_file(&c, 1, 1, &s.s("lib/Show/a.mkv")).await;
    seed_file(&c, 2, 1, &s.s("lib/Show/b.mkv")).await;
    seed_file(&c, 3, 1, &s.s("lib/Show/c.mkv")).await;
    seed_episode(&c, 1, 1, 1, 1, "One", Some(1)).await;
    seed_episode(&c, 2, 1, 1, 2, "Two", Some(2)).await;
    seed_episode(&c, 3, 1, 1, 3, "Three", Some(3)).await;
    naming_tv(
        &c,
        true,
        "{Series Title} - S{season:00}E{episode:00}",
        Some("Season {season:00}"),
        None,
    )
    .await;
    // b's sidecar destination is occupied: the whole group is refused, nothing moved.
    s.write("lib/Show/b.srt");
    // c's season directory is replaced by a symlink after planning input exists.
    std::fs::remove_dir_all(s.0.join("lib/Show/Season 01")).ok();
    s.write("lib/Show/Season 01/Show - S01E02.srt");
    let runtime = worker(&db, &api).await;
    let done = command(&api, json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    let item = |file: i64| {
        done["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["file_id"] == file)
            .unwrap()
            .clone()
    };
    assert_eq!(item(2)["state"], "failed");
    assert_eq!(item(2)["error_code"], "destination_exists");
    assert!(
        s.exists("lib/Show/b.mkv") && s.exists("lib/Show/b.srt"),
        "refused group is unmoved"
    );
    assert_eq!(path_of(&c, "episode_files", 2).await, s.s("lib/Show/b.mkv"));
    assert_eq!(item(1)["state"], "committed");
    assert_eq!(item(3)["state"], "committed");
    assert_eq!(done["items_failed"], 1);
    // A symlinked destination directory is refused and nothing is written through it.
    runtime.shutdown().await;
    std::fs::remove_dir_all(s.0.join("lib/Show/Season 01"))?;
    std::os::unix::fs::symlink(s.0.join("outside"), s.0.join("lib/Show/Season 01"))?;
    let runtime = worker(&db, &api).await;
    s.write("lib/Show/d.mkv");
    seed_file(&c, 4, 1, &s.s("lib/Show/d.mkv")).await;
    seed_episode(&c, 4, 1, 1, 4, "Four", Some(4)).await;
    let done = command(&api, json!({"name":"rename_files","target":{"media_type":"tv","series_ids":[1],"file_ids":[4]},"priority":"normal"})).await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["items"][0]["error_code"], "path_unsafe");
    assert!(s.exists("lib/Show/d.mkv"));
    assert!(!s.0.join("outside/Show - S01E04.mkv").exists());
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn case_only_rename_and_movie_files_with_folder_rename() -> R {
    let (_lock, s, db, c, api) = setup().await?;
    seed_movie(
        &c,
        1,
        "Big Film",
        &s.s("lib/old folder"),
        Some((1, &s.s("lib/old folder/nested/big film.MKV"))),
    )
    .await;
    s.write("lib/old folder/nested/big film.MKV");
    s.write("lib/old folder/nested/big film.en.srt");
    seed_series(&c, 1, "Show", &s.s("lib/show"), Some(0)).await;
    s.write("lib/show/ep.mkv");
    seed_file(&c, 1, 1, &s.s("lib/show/ep.mkv")).await;
    seed_episode(&c, 1, 1, 1, 1, "One", Some(1)).await;
    naming_movies(
        &c,
        true,
        "{Movie Title}",
        Some("{Movie Title} ({Release Year})"),
    )
    .await;
    naming_tv(&c, true, "ep", None, None).await;
    let runtime = worker(&db, &api).await;
    // Movie file preview (existing route) and execution agree on the filename.
    let preview = api
        .ok(
            "GET",
            "/api/v1/movies/rename-preview?movie_ids=1",
            None,
            200,
        )
        .await;
    assert_eq!(preview["items"][0]["new_path"], "Big Film.MKV");
    let done = command(&api, json!({"name":"rename_movie","target":{"media_type":"movies","movie_ids":[1]},"priority":"normal"})).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(s.exists("lib/old folder/Big Film.MKV") && s.exists("lib/old folder/Big Film.en.srt"));
    assert_eq!(
        path_of(&c, "movie_files", 1).await,
        s.s("lib/old folder/Big Film.MKV")
    );
    // Folder preview and rename: files follow the folder in the same transaction.
    let fp = api
        .ok(
            "GET",
            "/api/v1/movies/rename-folder-preview?movie_ids=1",
            None,
            200,
        )
        .await;
    assert_eq!(fp["items"][0]["new_path"], s.s("lib/Big Film (2020)"));
    let done = command(&api, json!({"name":"rename_movie_folder","target":{"media_type":"movies","movie_ids":[1]},"priority":"normal"})).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(s.exists("lib/Big Film (2020)/Big Film.MKV") && !s.exists("lib/old folder"));
    assert_eq!(path_of(&c, "movies", 1).await, s.s("lib/Big Film (2020)"));
    assert_eq!(
        path_of(&c, "movie_files", 1).await,
        s.s("lib/Big Film (2020)/Big Film.MKV")
    );
    // Case-only series folder rename ("show" -> "Show").
    let done = command(&api, json!({"name":"rename_series_folder","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(path_of(&c, "series", 1).await, s.s("lib/Show"));
    assert_eq!(
        path_of(&c, "episode_files", 1).await,
        s.s("lib/Show/ep.mkv")
    );
    let names: Vec<_> = std::fs::read_dir(s.0.join("lib"))?
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.contains(&"Show".to_owned()) && !names.contains(&"show".to_owned()));
    // Case-only file rename.
    naming_tv(&c, true, "EP", None, None).await;
    let done = command(&api, json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(done["items_renamed"], 1, "{done}");
    assert!(std::fs::read_dir(s.0.join("lib/Show"))?.any(|e| e.unwrap().file_name() == "EP.mkv"));
    let ev = api.ok("GET", "/api/v1/rename/events", None, 200).await;
    assert_eq!(ev["total"], 4);
    assert!(
        ev["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "folder" && e["owner"]["media_type"] == "movies")
    );
    runtime.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn admission_validation_cancel_and_duplicate_protection() -> R {
    let (_lock, s, _db, c, api) = setup().await?;
    seed_series(&c, 1, "Show", &s.s("lib/Show"), None).await;
    seed_movie(&c, 1, "Same Id", &s.s("lib/Movie"), None).await;
    let post = |body: Value| api.call("POST", "/api/v1/rename/commands", Some(body));
    for body in [
        json!({"name":"rename_series","target":{"media_type":"movies","movie_ids":[1]},"priority":"normal"}),
        json!({"name":"rename_movie","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"}),
        json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1],"movie_ids":[1]},"priority":"normal"}),
        json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[]},"priority":"normal"}),
        json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1,1]},"priority":"normal"}),
        json!({"name":"rename_files","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"}),
        json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1],"file_ids":[1]},"priority":"normal"}),
        json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal","surprise":1}),
    ] {
        assert_eq!(post(body.clone()).await.0, 400, "{body}");
    }
    assert_eq!(post(json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[7]},"priority":"normal"})).await.0, 404);
    // Equal numeric IDs in different domains are distinct targets.
    let tv = post(json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(tv.0, 202);
    let movie = post(json!({"name":"rename_movie","target":{"media_type":"movies","movie_ids":[1]},"priority":"normal"})).await;
    assert_eq!(movie.0, 202);
    assert_eq!(post(json!({"name":"rename_series_folder","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await.0, 409);
    let id = tv.1["id"].as_str().unwrap();
    let cancelled = api
        .ok(
            "POST",
            &format!("/api/v1/rename/commands/{id}/cancel"),
            None,
            200,
        )
        .await;
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(post(json!({"name":"rename_series_folder","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await.0, 202);
    let page = api
        .ok(
            "GET",
            "/api/v1/rename/commands?media_type=movies",
            None,
            200,
        )
        .await;
    assert_eq!(page["total"], 1);
    Ok(())
}

#[tokio::test]
async fn interrupted_groups_roll_forward_or_back_from_the_journal_at_startup() -> R {
    let (_lock, s, db, c, api) = setup().await?;
    seed_series(&c, 1, "Show", &s.s("lib/Show"), Some(0)).await;
    for n in 1..=2 {
        s.write(&format!("lib/Show/old{n}.mkv"));
        s.write(&format!("lib/Show/old{n}.srt"));
        seed_file(&c, n, 1, &s.s(&format!("lib/Show/old{n}.mkv"))).await;
        seed_episode(&c, n, 1, 1, n, "T", Some(n)).await;
    }
    naming_tv(&c, true, "x", None, None).await;
    c.execute("INSERT INTO rename_commands(id,name,media_type,priority,status,attempts,next_attempt_at,created_at) VALUES('00000000-0000-4000-8000-000000000001','rename_series','tv',0,'queued',0,0,0)", ()).await?;
    c.execute("INSERT INTO rename_command_targets(command_id,ordinal,series_id) VALUES('00000000-0000-4000-8000-000000000001',0,1)", ()).await?;
    use std::os::unix::fs::MetadataExt;
    let ident = |rel: &str| {
        let m = std::fs::metadata(s.0.join(rel)).unwrap();
        json!({"dev": m.dev() as i64, "ino": m.ino() as i64})
    };
    // Group 1: crashed after the media file moved, before the sidecar. Roll forward.
    let (i1m, i1s) = (ident("lib/Show/old1.mkv"), ident("lib/Show/old1.srt"));
    std::fs::rename(s.0.join("lib/Show/old1.mkv"), s.0.join("lib/Show/new1.mkv"))?;
    // Group 2: new name was taken by an unrelated file after the crash. Anomaly -> roll back.
    let (i2m, i2s) = (ident("lib/Show/old2.mkv"), ident("lib/Show/old2.srt"));
    std::fs::rename(s.0.join("lib/Show/old2.mkv"), s.0.join("lib/Show/new2.mkv"))?;
    s.write("lib/Show/old2.mkv.placeholder");
    std::fs::write(s.0.join("lib/Show/old2.mkv"), b"intruder")?;
    for (n, m, sc) in [(1, i1m, i1s), (2, i2m, i2s)] {
        c.execute("INSERT INTO rename_items(command_id,kind,series_id,file_id,old_path,new_path,identity_json,sidecars_json,state) VALUES('00000000-0000-4000-8000-000000000001','file',1,?,?,?,?,?,'planned')",
            params![n, s.s(&format!("lib/Show/old{n}.mkv")), s.s(&format!("lib/Show/new{n}.mkv")), m.to_string(), json!([{"old": s.s(&format!("lib/Show/old{n}.srt")), "new": s.s(&format!("lib/Show/new{n}.srt")), "ident": sc}]).to_string()]).await?;
    }
    // Follow real transitions so the fixture is terminal and only startup recovery can finish it.
    c.execute("UPDATE rename_commands SET status='running',attempts=1,started_at=1 WHERE id='00000000-0000-4000-8000-000000000001'", ()).await?;
    c.execute("UPDATE rename_commands SET status='failed',completed_at=2,error_code='interrupted' WHERE id='00000000-0000-4000-8000-000000000001'", ()).await?;
    let runtime = worker(&db, &api).await;
    let states: Vec<(i64, String, Option<String>)> = {
        let mut rows = c
            .query(
                "SELECT file_id,state,error_code FROM rename_items ORDER BY id",
                (),
            )
            .await?;
        let mut v = vec![];
        while let Some(r) = rows.next().await? {
            v.push((r.get(0)?, r.get(1)?, r.get(2)?));
        }
        v
    };
    assert_eq!(states[0], (1, "committed".into(), None), "{states:?}");
    assert!(
        s.exists("lib/Show/new1.mkv")
            && s.exists("lib/Show/new1.srt")
            && !s.exists("lib/Show/old1.srt")
    );
    assert_eq!(
        path_of(&c, "episode_files", 1).await,
        s.s("lib/Show/new1.mkv")
    );
    // A collision prevents rollback; retain the planned journal until that obstacle is removed.
    assert_eq!(
        states[1],
        (2, "planned".into(), Some("rollback_incomplete".into()))
    );
    assert_eq!(
        path_of(&c, "episode_files", 2).await,
        s.s("lib/Show/old2.mkv")
    );
    assert!(s.exists("lib/Show/old2.srt") && !s.exists("lib/Show/new2.srt"));
    assert_eq!(std::fs::read(s.0.join("lib/Show/old2.mkv"))?, b"intruder");
    runtime.shutdown().await;
    // Repeated recovery attempts neither discard the journal nor inflate terminal item failures.
    worker(&db, &api).await.shutdown().await;
    let failures: i64 = c
        .query("SELECT items_failed FROM rename_commands", ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert_eq!(failures, 0);
    let policy = commands::housekeeping::Policy {
        retention_seconds: 1000,
        min_keep: 0,
        ..Default::default()
    };
    let pruned = commands::housekeeping::prune(&db, &policy, 2_000_000_000).await?;
    assert_eq!(
        pruned.deleted_from("rename_commands", "tv"),
        0,
        "pending journal protects terminal history"
    );

    // Start while blocked, then remove the obstacle: a newly enqueued command must recover its
    // terminal predecessor, without another restart or resetting the predecessor's status.
    let runtime = worker(&db, &api).await;
    std::fs::remove_file(s.0.join("lib/Show/old2.mkv"))?;
    naming_tv(&c, false, "x", None, None).await;
    let retry = command(&api, json!({"name":"rename_series","target":{"media_type":"tv","series_ids":[1]},"priority":"normal"})).await;
    assert_eq!(
        retry["status"], "succeeded",
        "disabled naming leaves recovered files unchanged instead of planning another move"
    );
    assert_eq!(retry["items_unchanged"], 2);
    assert_eq!(retry["items_renamed"], 0);
    runtime.shutdown().await;
    assert_eq!(
        path_of(&c, "episode_files", 2).await,
        s.s("lib/Show/new2.mkv")
    );
    assert!(s.exists("lib/Show/new2.mkv") && s.exists("lib/Show/new2.srt"));
    assert!(!s.exists("lib/Show/old2.mkv") && !s.exists("lib/Show/old2.srt"));
    let committed: i64 = c
        .query(
            "SELECT count(*) FROM rename_items WHERE state='committed' AND error_code IS NULL",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert_eq!(committed, 2);
    let row = c
        .query("SELECT items_renamed,items_failed FROM rename_commands WHERE id='00000000-0000-4000-8000-000000000001'", ())
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!((row.get::<i64>(0)?, row.get::<i64>(1)?), (2, 0));
    let pruned = commands::housekeeping::prune(&db, &policy, 2_000_000_000).await?;
    assert_eq!(
        pruned.deleted_from("rename_commands", "tv"),
        2,
        "settled journals no longer pin either command's history"
    );
    Ok(())
}

/// Older binaries settled an unrestorable group as `failed`/`rollback_incomplete` and counted it
/// as a failed item. These rows are reopened only by an explicit, validated request; the media
/// recovery itself is still the worker's journal recovery.
#[tokio::test]
async fn legacy_failed_rollback_journals_reopen_only_on_explicit_request() -> R {
    use std::os::unix::fs::MetadataExt;
    const TV_CMD: &str = "00000000-0000-4000-8000-0000000000a1";
    const MOVIE_CMD: &str = "00000000-0000-4000-8000-0000000000a2";
    let (_lock, s, db, c, api) = setup().await?;
    // Series 1 and movie 1 deliberately share a numeric ID.
    seed_series(&c, 1, "Show", &s.s("lib/Show"), Some(0)).await;
    seed_movie(
        &c,
        1,
        "Film",
        &s.s("lib/Film"),
        Some((1, &s.s("lib/Film/old.mkv"))),
    )
    .await;
    for n in 1..=5 {
        s.write(&format!("lib/Show/old{n}.mkv"));
        seed_file(&c, n, 1, &s.s(&format!("lib/Show/old{n}.mkv"))).await;
    }
    s.write("lib/Show/old1.srt");
    s.write("lib/Film/old.mkv");
    let ident = |rel: &str| {
        let m = std::fs::metadata(s.0.join(rel)).unwrap();
        json!({"dev": m.dev() as i64, "ino": m.ino() as i64})
    };
    for (id, name, media) in [
        (TV_CMD, "rename_series", "tv"),
        (MOVIE_CMD, "rename_movie", "movies"),
    ] {
        c.execute("INSERT INTO rename_commands(id,name,media_type,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,0,'queued',0,0,0)", params![id, name, media]).await?;
        c.execute(
            &format!(
                "INSERT INTO rename_command_targets(command_id,ordinal,{}) VALUES(?,0,1)",
                if media == "tv" {
                    "series_id"
                } else {
                    "movie_id"
                }
            ),
            [id],
        )
        .await?;
    }
    // 1: media moved, sidecar not yet (partial). 2: media moved, unrelated file at the old name.
    // 3: nothing moved (layout verifiably original). 4: unrelated failure, not the legacy class.
    // 5: invalid identity evidence. Movie 1: media moved.
    let (i1m, i1s) = (ident("lib/Show/old1.mkv"), ident("lib/Show/old1.srt"));
    std::fs::rename(s.0.join("lib/Show/old1.mkv"), s.0.join("lib/Show/new1.mkv"))?;
    let i2m = ident("lib/Show/old2.mkv");
    std::fs::rename(s.0.join("lib/Show/old2.mkv"), s.0.join("lib/Show/new2.mkv"))?;
    std::fs::write(s.0.join("lib/Show/old2.mkv"), b"intruder")?;
    let i3m = ident("lib/Show/old3.mkv");
    let i4m = ident("lib/Show/old4.mkv");
    let imm = ident("lib/Film/old.mkv");
    std::fs::rename(s.0.join("lib/Film/old.mkv"), s.0.join("lib/Film/new.mkv"))?;
    let tv = |n: i64, ident: &Value, sidecars: Value, state: &str, code: &str| {
        let (old, new) = (
            s.s(&format!("lib/Show/old{n}.mkv")),
            s.s(&format!("lib/Show/new{n}.mkv")),
        );
        (
            n,
            old,
            new,
            ident.to_string(),
            sidecars.to_string(),
            state.to_owned(),
            code.to_owned(),
        )
    };
    let side1 =
        json!([{"old": s.s("lib/Show/old1.srt"), "new": s.s("lib/Show/new1.srt"), "ident": i1s}]);
    for (n, old, new, identity, sidecars, state, code) in [
        tv(1, &i1m, side1, "failed", "rollback_incomplete"),
        tv(2, &i2m, json!([]), "failed", "rollback_incomplete"),
        tv(3, &i3m, json!([]), "failed", "rollback_incomplete"),
        tv(4, &i4m, json!([]), "failed", "destination_exists"),
        tv(
            5,
            &json!({"unexpected": true}),
            json!([]),
            "failed",
            "rollback_incomplete",
        ),
    ] {
        c.execute("INSERT INTO rename_items(command_id,kind,series_id,file_id,old_path,new_path,identity_json,sidecars_json,state,error_code) VALUES(?,'file',1,?,?,?,?,?,?,?)",
            params![TV_CMD, n, old, new, identity, sidecars, state, code]).await?;
    }
    c.execute("INSERT INTO rename_items(command_id,kind,movie_id,file_id,old_path,new_path,identity_json,sidecars_json,state,error_code) VALUES(?,'file',1,1,?,?,?,'[]','failed','rollback_incomplete')",
        params![MOVIE_CMD, s.s("lib/Film/old.mkv"), s.s("lib/Film/new.mkv"), imm.to_string()]).await?;
    // Valid transitions; the old binary counted each incomplete attempt as one failed item.
    for (id, failed) in [(TV_CMD, 5), (MOVIE_CMD, 1)] {
        c.execute(
            "UPDATE rename_commands SET status='running',attempts=1,started_at=1 WHERE id=?",
            [id],
        )
        .await?;
        c.execute(
            "UPDATE rename_commands SET items_failed=? WHERE id=?",
            params![failed, id],
        )
        .await?;
        c.execute("UPDATE rename_commands SET status='failed',completed_at=2,error_code='items_failed' WHERE id=?", [id]).await?;
    }
    let ids: Vec<i64> = {
        let mut rows = c
            .query("SELECT id FROM rename_items ORDER BY id", ())
            .await?;
        let mut v = vec![];
        while let Some(r) = rows.next().await? {
            v.push(r.get(0)?);
        }
        v
    };
    let (item1, item2, item3, item4, item5, movie) =
        (ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]);
    let snapshot = || async {
        let mut rows = c
            .query(
                "SELECT state,coalesce(error_code,'') FROM rename_items ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut v = vec![];
        while let Some(r) = rows.next().await.unwrap() {
            v.push((r.get::<String>(0).unwrap(), r.get::<String>(1).unwrap()));
        }
        v
    };
    let counters = |id: &'static str| {
        let c = c.clone();
        async move {
            let r = c
                .query(
                    "SELECT items_renamed,items_failed,status FROM rename_commands WHERE id=?",
                    [id],
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (
                r.get::<i64>(0).unwrap(),
                r.get::<i64>(1).unwrap(),
                r.get::<String>(2).unwrap(),
            )
        }
    };
    let before = snapshot().await;
    let policy = commands::housekeeping::Policy {
        retention_seconds: 1000,
        min_keep: 0,
        ..Default::default()
    };

    // The failed legacy class pins its command's history, but no new binary recovers it by itself.
    assert_eq!(
        commands::housekeeping::prune(&db, &policy, 2_000_000_000)
            .await?
            .deleted_from("rename_commands", "tv"),
        0
    );
    worker(&db, &api).await.shutdown().await;
    assert_eq!(
        snapshot().await,
        before,
        "startup recovery ignores historical failed rows"
    );

    // Inventory lists exactly the legacy class (not item 4), typed per domain.
    let page = api
        .ok("GET", "/api/v1/rename/legacy-recovery", None, 200)
        .await;
    assert_eq!(page["total"], 5);
    let listed: Vec<i64> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item_id"].as_i64().unwrap())
        .collect();
    assert_eq!(listed, vec![item1, item2, item3, item5, movie]);
    let tv_only = api
        .ok(
            "GET",
            "/api/v1/rename/legacy-recovery?media_type=movies",
            None,
            200,
        )
        .await;
    assert_eq!(tv_only["total"], 1);
    assert_eq!(tv_only["items"][0]["media_type"], "movies");

    // Dry run reports effects and changes nothing; the flag has no default.
    let post = |dry: bool, items: Vec<i64>| json!({"dry_run": dry, "item_ids": items});
    api.ok(
        "POST",
        "/api/v1/rename/legacy-recovery",
        Some(json!({"item_ids": [item1]})),
        400,
    )
    .await;
    let dry = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(true, vec![item1, item2, item3, item4, item5, movie])),
            200,
        )
        .await;
    let by = |report: &Value, id: i64| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == id)
            .unwrap()
            .clone()
    };
    let one = by(&dry, item1);
    assert_eq!(
        (
            one["layout"].as_str(),
            one["action"].as_str(),
            one["items_failed_delta"].as_i64(),
            one["command_items_failed"].as_i64()
        ),
        (Some("partial"), Some("roll_forward"), Some(-1), Some(5))
    );
    assert_eq!(one["sidecars"][0]["new_path"], s.s("lib/Show/new1.srt"));
    assert_eq!(one["reopened"], false);
    assert_eq!(
        (
            by(&dry, item2)["layout"].as_str(),
            by(&dry, item2)["refusal"].as_str()
        ),
        (Some("conflict"), Some("layout_conflict"))
    );
    assert_eq!(
        (
            by(&dry, item3)["layout"].as_str(),
            by(&dry, item3)["action"].as_str()
        ),
        (Some("at_source"), Some("settle_restored"))
    );
    assert_eq!(by(&dry, item4)["refusal"], "not_legacy");
    assert_eq!(by(&dry, item5)["refusal"], "journal_invalid");
    let film = by(&dry, movie);
    assert_eq!(
        (
            film["media_type"].as_str(),
            film["owner_id"].as_i64(),
            film["layout"].as_str()
        ),
        (Some("movies"), Some(1), Some("moved"))
    );
    assert_eq!(snapshot().await, before);
    assert_eq!(counters(TV_CMD).await, (0, 5, "failed".into()));
    assert!(!s.exists("lib/Show/new1.srt") && s.exists("lib/Show/new1.mkv"));

    // Unknown IDs reject the whole request before any write.
    api.ok(
        "POST",
        "/api/v1/rename/legacy-recovery",
        Some(post(false, vec![item1, 999_999])),
        404,
    )
    .await;
    assert_eq!(snapshot().await, before);

    // Explicit admission of the movie alone cannot touch the series that shares its ID.
    let applied = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(false, vec![movie])),
            200,
        )
        .await;
    assert_eq!(by(&applied, movie)["reopened"], true);
    assert_eq!(counters(MOVIE_CMD).await, (0, 0, "failed".into()));
    assert_eq!(counters(TV_CMD).await, (0, 5, "failed".into()));
    let rerun = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(false, vec![movie])),
            200,
        )
        .await;
    assert_eq!(
        (
            by(&rerun, movie)["refusal"].as_str(),
            by(&rerun, movie)["reopened"].clone()
        ),
        (Some("not_legacy"), json!(false))
    );
    assert_eq!(
        counters(MOVIE_CMD).await,
        (0, 0, "failed".into()),
        "a rerun cannot decrement twice"
    );

    // The series items: refused ones stay untouched, the verified-original one settles without a
    // media change, the partial one reopens.
    let applied = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(false, vec![item1, item2, item3, item4, item5])),
            200,
        )
        .await;
    assert_eq!(
        [item1, item2, item3, item4, item5].map(|i| by(&applied, i)["reopened"].as_bool().unwrap()),
        [true, false, true, false, false]
    );
    let after = snapshot().await;
    assert_eq!(after[0], ("planned".into(), "rollback_incomplete".into()));
    assert_eq!(after[1], ("failed".into(), "rollback_incomplete".into()));
    assert_eq!(
        after[2],
        ("rolled_back".into(), "legacy_rollback_verified".into())
    );
    assert_eq!(after[3], ("failed".into(), "destination_exists".into()));
    assert_eq!(after[4], ("failed".into(), "rollback_incomplete".into()));
    assert_eq!(counters(TV_CMD).await, (0, 4, "failed".into()));
    // Persisting the transition touched no media.
    assert!(
        !s.exists("lib/Show/new1.srt")
            && s.exists("lib/Show/old1.srt")
            && s.exists("lib/Show/new1.mkv")
    );
    // Reopened journals are protected as current recovery handles.
    assert_eq!(
        commands::housekeeping::prune(&db, &policy, 2_000_000_000)
            .await?
            .deleted_from("rename_commands", "movies"),
        0
    );

    // The worker's existing journal recovery finishes both reopened groups.
    worker(&db, &api).await.shutdown().await;
    assert!(s.exists("lib/Show/new1.srt") && !s.exists("lib/Show/old1.srt"));
    assert_eq!(
        path_of(&c, "episode_files", 1).await,
        s.s("lib/Show/new1.mkv")
    );
    assert_eq!(path_of(&c, "movie_files", 1).await, s.s("lib/Film/new.mkv"));
    assert_eq!(
        path_of(&c, "episode_files", 3).await,
        s.s("lib/Show/old3.mkv")
    );
    assert_eq!(counters(MOVIE_CMD).await, (1, 0, "failed".into()));
    assert_eq!(counters(TV_CMD).await, (1, 4, "failed".into()));
    let events: i64 = c
        .query("SELECT count(*) FROM rename_events", ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert_eq!(
        events, 2,
        "one event per recovered rename, none for the settled item"
    );
    worker(&db, &api).await.shutdown().await;
    assert_eq!(
        counters(TV_CMD).await,
        (1, 4, "failed".into()),
        "repeated recovery is idempotent"
    );

    // The collision stays a refusal until the unrelated file is removed; then it is admissible.
    let blocked = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(true, vec![item2])),
            200,
        )
        .await;
    assert_eq!(by(&blocked, item2)["refusal"], "layout_conflict");
    assert_eq!(std::fs::read(s.0.join("lib/Show/old2.mkv"))?, b"intruder");
    std::fs::remove_file(s.0.join("lib/Show/old2.mkv"))?;
    let applied = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(false, vec![item2])),
            200,
        )
        .await;
    assert_eq!(by(&applied, item2)["reopened"], true);
    worker(&db, &api).await.shutdown().await;
    assert_eq!(
        path_of(&c, "episode_files", 2).await,
        s.s("lib/Show/new2.mkv")
    );
    assert_eq!(counters(TV_CMD).await, (2, 3, "failed".into()));

    // A command that is still active for the owner blocks reopening; a counter that cannot be
    // decremented is reported, not made negative.
    c.execute(
        "UPDATE rename_commands SET items_failed=0 WHERE id=?",
        [TV_CMD],
    )
    .await?;
    c.execute("UPDATE rename_items SET new_path=?,state='failed',error_code='rollback_incomplete' WHERE id=?", params![s.s("lib/Show/new4.mkv"), item4]).await?;
    std::fs::rename(s.0.join("lib/Show/old4.mkv"), s.0.join("lib/Show/new4.mkv"))?;
    let stuck = api
        .ok(
            "POST",
            "/api/v1/rename/legacy-recovery",
            Some(post(false, vec![item4])),
            200,
        )
        .await;
    assert_eq!(
        (
            by(&stuck, item4)["refusal"].as_str(),
            by(&stuck, item4)["layout"].as_str()
        ),
        (Some("counter_inconsistent"), Some("moved"))
    );
    assert_eq!(counters(TV_CMD).await.1, 0);

    // Resolved journals no longer pin history; the unresolved legacy rows still do.
    assert_eq!(
        commands::housekeeping::prune(&db, &policy, 2_000_000_000)
            .await?
            .deleted_from("rename_commands", "movies"),
        1
    );
    assert_eq!(
        commands::housekeeping::prune(&db, &policy, 2_000_000_000)
            .await?
            .deleted_from("rename_commands", "tv"),
        0
    );
    Ok(())
}
