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
    c.execute("UPDATE rename_commands SET status='failed',attempts=1,started_at=1,completed_at=2,error_code='interrupted' WHERE id='00000000-0000-4000-8000-000000000001'", ()).await.ok();
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
    assert_eq!(states[1].0, 2);
    assert_ne!(states[1].1, "committed", "{states:?}");
    assert_eq!(
        path_of(&c, "episode_files", 2).await,
        s.s("lib/Show/old2.mkv")
    );
    assert!(s.exists("lib/Show/old2.srt") && !s.exists("lib/Show/new2.srt"));
    assert_eq!(std::fs::read(s.0.join("lib/Show/old2.mkv"))?, b"intruder");
    runtime.shutdown().await;
    Ok(())
}
