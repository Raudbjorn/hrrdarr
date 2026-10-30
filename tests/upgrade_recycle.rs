//! Upgrade retirement into the recycle bin through real HTTP, the real command worker and the
//! downloaded-folder scan producer, both domains in one database. Series 1 and movie 1 share a
//! numeric ID and each domain's original is file ID 1. Temporary directories, databases and
//! 127.0.0.1:0 listeners only; no live service is contacted.
use hrrdarr::{commands, db::Database, metadata::MetadataClient, providers};
use serde_json::{Value, json};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use uuid::Uuid;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
// Same catalog/profile seed as the downloaded-scan command tests: 1080p WEB-DL (quality 7) is
// the only allowed quality, so a 720p HDTV (quality 3) original is replaceable.
const SEED: &str = "INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');INSERT INTO seasons(series_id,number)VALUES(1,0),(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,air_date)VALUES(1,1,1,1,'Pilot',45,47,'2026-09-24'),(2,1,1,2,'Next',45,48,'2026-09-25');INSERT INTO movie_metadata(id,title,year,runtime,original_language)VALUES(1,'Harbor',2020,100,1);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,0),(1,'tv',7,1,1),(2,'movies',3,0,0),(2,'movies',7,1,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',0,7,100,100,100,NULL),(2,'movies',0,7,100,100,100,2);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released');";
const REVISION: &str = "{\"version\":1,\"real\":0,\"is_repack\":false}";

fn wav() -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&16036u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.resize(16044, 0);
    wav
}
/// Distinct payloads so byte-for-byte assertions cannot be satisfied by another file.
fn payload(tag: u8) -> Vec<u8> {
    let mut bytes = wav();
    bytes.extend(std::iter::repeat_n(tag, 4096));
    bytes
}
fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

struct Harness {
    db: Arc<Database>,
    base: String,
    scratch: Scratch,
    _server: Server,
    worker: Option<commands::Runtime>,
}
impl Harness {
    async fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-upgrade-recycle-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        db.connect()
            .await
            .unwrap()
            .execute_batch(SEED)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = commands::router(db.clone())
            .merge(hrrdarr::import::router(db.clone()))
            .merge(hrrdarr::recycle_bin::router(db.clone()))
            .merge(hrrdarr::history::router(db.clone()));
        let server = Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        }));
        Self {
            db,
            base,
            scratch: Scratch(path),
            _server: server,
            worker: None,
        }
    }
    async fn start_worker(&mut self) {
        let (_, client) = providers::router_with_refresh(self.db.clone(), None);
        let metadata = Arc::new(
            MetadataClient::with_origins("http://127.0.0.1:1/", "http://127.0.0.1:1/").unwrap(),
        );
        self.worker = Some(
            commands::start_with_metadata(self.db.clone(), client, metadata)
                .await
                .unwrap(),
        );
    }
    async fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown().await;
        }
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.scratch.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    async fn count(&self, sql: &str, args: Vec<libsql::Value>) -> i64 {
        let c = self.db.connect().await.unwrap();
        let mut rows = c.query(sql, args).await.unwrap();
        match rows.next().await.unwrap().map(|r| r.get_value(0).unwrap()) {
            Some(libsql::Value::Integer(n)) => n,
            other => panic!("{sql} -> {other:?}"),
        }
    }
    async fn sql(&self, sql: &str) {
        self.db
            .connect()
            .await
            .unwrap()
            .execute_batch(sql)
            .await
            .unwrap();
    }
    /// Recorded file-delete events for one domain, oldest first.
    async fn file_delete_events(&self, scope: &str) -> Vec<Value> {
        let c = self.db.connect().await.unwrap();
        let mut rows = c
            .query(
                "SELECT subject_id,payload_json FROM notification_events WHERE kind='file_delete' AND scope=? ORDER BY sequence",
                [scope],
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            let payload: String = r.get(1).unwrap();
            out.push(json!({
                "subject_id": r.get::<Option<i64>>(0).unwrap(),
                "payload": serde_json::from_str::<Value>(&payload).unwrap(),
            }));
        }
        out
    }
    async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let mut request = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if !body.is_null() {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}")),
        )
    }
    async fn wait(&self, path: &str, done: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (code, v) = self.call("GET", path, Value::Null).await;
                assert_eq!(code, 200, "{v}");
                if done(&v) {
                    return v;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{path} did not reach the expected state"))
    }
}
fn text(v: &str) -> libsql::Value {
    libsql::Value::Text(v.into())
}

#[tokio::test]
async fn scan_upgrades_recycle_each_original_once_in_both_domains_and_expire_by_cleanup() {
    let mut h = Harness::new().await;
    // An enabled file-delete scope for both domains: the retirement of a replaced original is
    // recorded as a file delete, because the original leaves the library without a row delete.
    h.sql(
        "INSERT INTO notifications(id,name,implementation,enabled,revision,settings_json,secret_names,created_at,updated_at)\
         VALUES('11111111-1111-4111-8111-111111111111','Upgrade','webhook',1,1,'{}','[]',100,100);\
         INSERT INTO notification_scopes(notification_id,media_type,on_file_delete,on_file_delete_for_upgrade)\
         VALUES('11111111-1111-4111-8111-111111111111','tv',1,1),\
                ('11111111-1111-4111-8111-111111111111','movies',1,1);",
    )
    .await;
    let tv_root = h.dir("lib/tv");
    let movie_root = h.dir("lib/movie");
    let c = h.db.connect().await.unwrap();
    c.execute(
        "UPDATE series SET path=? WHERE id=1",
        [tv_root.to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE movies SET path=? WHERE id=1",
        [movie_root.to_str().unwrap()],
    )
    .await
    .unwrap();
    // One lower-quality TV original serves both episodes; the movie original shares file ID 1.
    let old_tv = tv_root.join("Season 1/Harbor.S01E01-E02.720p.HDTV.mkv");
    let old_movie = movie_root.join("Harbor.2020.720p.HDTV.mkv");
    write(&old_tv, &payload(10));
    write(&old_movie, &payload(20));
    c.execute(
        "INSERT INTO episode_files(id,series_id,path)VALUES(1,1,?)",
        [old_tv.to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',1,3,?)", [REVISION]).await.unwrap();
    c.execute_batch("UPDATE episodes SET episode_file_id=1 WHERE id IN (1,2);")
        .await
        .unwrap();
    c.execute(
        "INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,?)",
        [old_movie.to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute("INSERT INTO file_metadata(media_type,movie_file_id,quality_id,revision_json)VALUES('movies',1,3,?)", [REVISION]).await.unwrap();
    let inodes = [
        std::fs::metadata(&old_tv).unwrap().ino(),
        std::fs::metadata(&old_movie).unwrap().ino(),
    ];
    // One shared bin outside every library folder, configured per domain through the API.
    let bin = h.dir("bin");
    for domain in ["tv", "movies"] {
        let (code, current) = h
            .call("GET", &format!("/api/v1/{domain}/recycle-bin"), Value::Null)
            .await;
        assert_eq!(code, 200, "{current}");
        let (code, saved) = h
            .call(
                "PUT",
                &format!("/api/v1/{domain}/recycle-bin"),
                json!({"revision":current["revision"],"path":bin,"cleanup_days":7}),
            )
            .await;
        assert_eq!(code, 200, "{saved}");
    }
    // Release-named folders, as a download client would produce them.
    let tv_source = h
        .dir("tv-source/Harbor.S01E01-E02.1080p.WEB-DL")
        .join("Harbor.S01E01-E02.1080p.WEB-DL.flv");
    let movie_source = h
        .dir("movie-source/Harbor.2020.1080p.WEB-DL")
        .join("Harbor.2020.1080p.WEB-DL.flv");
    write(&tv_source, &payload(30));
    write(&movie_source, &payload(31));
    let (tv_id, movie_id) = (Uuid::new_v4(), Uuid::new_v4());
    for (domain, id, source) in [
        ("tv", tv_id, tv_source.parent().unwrap()),
        ("movies", movie_id, movie_source.parent().unwrap()),
    ] {
        let (code, body) = h
            .call(
                "POST",
                &format!("/api/v1/{domain}/downloaded-scan-commands"),
                json!({"id":id,"path":source,"mode":"copy","priority":"normal"}),
            )
            .await;
        assert_eq!(code, 202, "{body}");
    }
    h.start_worker().await;
    for (domain, id) in [("tv", tv_id), ("movies", movie_id)] {
        let done = h
            .wait(
                &format!("/api/v1/{domain}/downloaded-scan-commands/{id}"),
                |v| v["phase"] == "settled",
            )
            .await;
        assert_eq!(
            (done["status"].as_str(), done["outcome"].as_str()),
            (Some("succeeded"), Some("success")),
            "{done}"
        );
        assert_eq!(done["counts"]["imported"], 1, "{done}");
    }
    // The replaced originals leave the library by retirement, not by a row delete: the enabled
    // scope records one file delete per domain with the Upgrade reason and the original path.
    let tv_events = h.file_delete_events("tv").await;
    assert_eq!(tv_events.len(), 1, "{tv_events:?}");
    assert_eq!(tv_events[0]["subject_id"], 1, "{tv_events:?}");
    assert_eq!(
        tv_events[0]["payload"]["reason"], "Upgrade",
        "{tv_events:?}"
    );
    assert_eq!(
        tv_events[0]["payload"]["path"],
        old_tv.to_str().unwrap(),
        "{tv_events:?}"
    );
    let movie_events = h.file_delete_events("movies").await;
    assert_eq!(movie_events.len(), 1, "{movie_events:?}");
    assert_eq!(movie_events[0]["subject_id"], 1, "{movie_events:?}");
    assert_eq!(
        movie_events[0]["payload"]["reason"], "Upgrade",
        "{movie_events:?}"
    );
    assert_eq!(
        movie_events[0]["payload"]["path"],
        old_movie.to_str().unwrap(),
        "{movie_events:?}"
    );
    // New files are in the library; each original is in the bin under its owner folder layout,
    // as the same inode, and no library copy or recovery artifact remains.
    assert_eq!(
        std::fs::read(tv_root.join("Harbor.S01E01-E02.1080p.WEB-DL.flv")).unwrap(),
        payload(30)
    );
    assert_eq!(
        std::fs::read(movie_root.join("Harbor.2020.1080p.WEB-DL.flv")).unwrap(),
        payload(31)
    );
    let recycled_tv = bin.join("tv/Season 1/Harbor.S01E01-E02.720p.HDTV.mkv");
    let recycled_movie = bin.join("movie/Harbor.2020.720p.HDTV.mkv");
    for (path, bytes, inode) in [
        (&recycled_tv, payload(10), inodes[0]),
        (&recycled_movie, payload(20), inodes[1]),
    ] {
        assert_eq!(std::fs::read(path).unwrap(), bytes, "{path:?}");
        assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    }
    assert!(!old_tv.exists() && !old_movie.exists());
    for dir in [tv_root.join("Season 1"), movie_root.clone()] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap().path();
            if entry
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(".hrrdarr-replaced-")
            {
                assert!(
                    !entry.join("original").exists(),
                    "artifact must be empty after recycling: {entry:?}"
                );
            }
        }
    }
    // The shared TV original is one journal row and one transfer, with one history event per
    // episode it served; the movie row is typed separately despite the equal numeric IDs.
    assert_eq!(
        h.count("SELECT count(*) FROM upgrade_recycles WHERE media_type='tv' AND old_file_id=1 AND phase='complete' AND transfer='rename'", vec![]).await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM upgrade_recycles WHERE media_type='movies' AND old_file_id=1 AND phase='complete' AND transfer='rename'", vec![]).await,
        1
    );
    assert_eq!(
        h.count("SELECT count(DISTINCT recycle_id) FROM upgrade_recycle_history WHERE media_type='episode' AND episode_id IN (1,2) AND file_id=1 AND recycle_path=?", vec![text(recycled_tv.to_str().unwrap())]).await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM upgrade_recycle_history WHERE media_type='episode'",
            vec![]
        )
        .await,
        2
    );
    assert_eq!(
        h.count("SELECT count(*) FROM upgrade_recycle_history WHERE media_type='movie' AND movie_id=1 AND episode_id IS NULL AND recycle_path=?", vec![text(recycled_movie.to_str().unwrap())]).await,
        1
    );
    let (code, tv_history) = h
        .call("GET", "/api/v1/history?media_type=tv", Value::Null)
        .await;
    assert_eq!(code, 200, "{tv_history}");
    let recycles: Vec<&Value> = tv_history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["origin"] == "native_deletion")
        .collect();
    assert_eq!(recycles.len(), 2, "{tv_history}");
    assert!(recycles.iter().all(|e| e["reason"] == "upgrade"
        && e["deletion_id"] == recycles[0]["deletion_id"]
        && e["file"] == json!({"media_type":"tv","id":1})));
    let (_, movie_history) = h
        .call("GET", "/api/v1/history?media_type=movies", Value::Null)
        .await;
    let movie_events: Vec<&Value> = movie_history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["origin"] == "native_deletion")
        .collect();
    assert_eq!(movie_events.len(), 1, "{movie_history}");
    assert_eq!(
        movie_events[0]["target"],
        json!({"media_type":"movie","id":1})
    );
    assert_eq!(movie_events[0]["reason"], "upgrade");

    // Recycled originals are subject to the existing CleanUpRecycleBin retention.
    let expired = SystemTime::now() - Duration::from_secs(8 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&recycled_tv)
        .unwrap()
        .set_modified(expired)
        .unwrap();
    let (code, command) = h
        .call(
            "POST",
            "/api/v1/recycle-bin/cleanup-commands",
            json!({"target":{"media_type":"tv"},"priority":"normal"}),
        )
        .await;
    assert_eq!(code, 202, "{command}");
    let cleaned = h
        .wait(
            &format!(
                "/api/v1/recycle-bin/cleanup-commands/{}",
                command["id"].as_str().unwrap()
            ),
            |v| v["status"] == "succeeded",
        )
        .await;
    assert_eq!(cleaned["outcome"], "cleaned", "{cleaned}");
    assert!(cleaned["files_removed"].as_i64().unwrap() >= 1, "{cleaned}");
    assert!(!recycled_tv.exists(), "expired recycled original removed");
    assert_eq!(
        std::fs::read(&recycled_movie).unwrap(),
        payload(20),
        "unexpired recycled original kept"
    );
    // History is immutable evidence; cleanup does not rewrite it.
    assert_eq!(
        h.count("SELECT count(*) FROM upgrade_recycle_history", vec![])
            .await,
        3
    );
    h.stop_worker().await;
}

/// One bin shared by both domains has no per-entry domain marker, so the longest retention wins
/// and a domain that disables cleanup disables it for the whole path.
#[tokio::test]
async fn shared_recycle_bin_keeps_the_longest_retention_and_zero_disables_cleanup() {
    let mut h = Harness::new().await;
    let bin = h.dir("bin");
    for (domain, days) in [("tv", 7), ("movies", 30)] {
        let (code, current) = h
            .call("GET", &format!("/api/v1/{domain}/recycle-bin"), Value::Null)
            .await;
        assert_eq!(code, 200, "{current}");
        let (code, saved) = h
            .call(
                "PUT",
                &format!("/api/v1/{domain}/recycle-bin"),
                json!({"revision":current["revision"],"path":bin,"cleanup_days":days}),
            )
            .await;
        assert_eq!(code, 200, "{saved}");
    }
    // Older than the TV retention, younger than the movie retention.
    let artifact = bin.join("tv/Season 1/old.mkv");
    write(&artifact, &payload(1));
    let expired = SystemTime::now() - Duration::from_secs(10 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&artifact)
        .unwrap()
        .set_modified(expired)
        .unwrap();
    h.start_worker().await;
    let (code, command) = h
        .call(
            "POST",
            "/api/v1/recycle-bin/cleanup-commands",
            json!({"target":{"media_type":"tv"},"priority":"normal"}),
        )
        .await;
    assert_eq!(code, 202, "{command}");
    let cleaned = h
        .wait(
            &format!(
                "/api/v1/recycle-bin/cleanup-commands/{}",
                command["id"].as_str().unwrap()
            ),
            |v| v["status"] == "succeeded",
        )
        .await;
    assert_eq!(cleaned["outcome"], "cleaned", "{cleaned}");
    assert_eq!(cleaned["cleanup_days"], 30, "{cleaned}");
    assert_eq!(cleaned["files_removed"], 0, "{cleaned}");
    assert!(
        artifact.exists(),
        "the longer movie retention keeps the shared file"
    );
    // A domain that disables cleanup disables it for the shared path.
    let (code, current) = h
        .call("GET", "/api/v1/movies/recycle-bin", Value::Null)
        .await;
    assert_eq!(code, 200, "{current}");
    let (code, saved) = h
        .call(
            "PUT",
            "/api/v1/movies/recycle-bin",
            json!({"revision":current["revision"],"path":bin,"cleanup_days":0}),
        )
        .await;
    assert_eq!(code, 200, "{saved}");
    let (code, command) = h
        .call(
            "POST",
            "/api/v1/recycle-bin/cleanup-commands",
            json!({"target":{"media_type":"tv"},"priority":"normal"}),
        )
        .await;
    assert_eq!(code, 202, "{command}");
    let cleaned = h
        .wait(
            &format!(
                "/api/v1/recycle-bin/cleanup-commands/{}",
                command["id"].as_str().unwrap()
            ),
            |v| v["status"] == "succeeded",
        )
        .await;
    assert_eq!(cleaned["outcome"], "disabled", "{cleaned}");
    assert_eq!(cleaned["cleanup_days"], 0, "{cleaned}");
    assert!(artifact.exists(), "cleanup is disabled for the shared path");
    h.stop_worker().await;
}
