//! Real HTTP + real worker coverage of downloaded-folder scan commands: phase A discovery and
//! the phase B sequential import continuation. Everything runs against temporary directories
//! and databases and 127.0.0.1:0 listeners; no live service is contacted. Each test owns its
//! database and worker. Library roots under `/fictional` do not exist, so tests that keep them
//! observe a measured prepare refusal instead of a transfer.
use hrrdarr::{commands, db::Database, metadata::MetadataClient, providers};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
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
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
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
        let path = std::env::temp_dir().join(format!("hrrdarr-scan-http-{}", Uuid::new_v4()));
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
        let app = commands::router(db.clone()).merge(hrrdarr::import::router(db.clone()));
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
    /// Real library roots: series 1, movie 1 and movie 2 move under the scratch directory.
    async fn library(&self) -> (PathBuf, PathBuf) {
        let tv = self.dir("lib/tv");
        let movie = self.dir("lib/movie");
        let orbit = self.dir("lib/orbit");
        let c = self.db.connect().await.unwrap();
        c.execute(
            "UPDATE series SET path=? WHERE id=1",
            [tv.to_str().unwrap()],
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE movies SET path=? WHERE id=1",
            [movie.to_str().unwrap()],
        )
        .await
        .unwrap();
        c.execute(
            "UPDATE movies SET path=? WHERE id=2",
            [orbit.to_str().unwrap()],
        )
        .await
        .unwrap();
        (tv, movie)
    }
    async fn scalar(&self, sql: &str, args: Vec<libsql::Value>) -> Option<libsql::Value> {
        let c = self.db.connect().await.unwrap();
        let mut rows = c.query(sql, args).await.unwrap();
        rows.next().await.unwrap().map(|r| r.get_value(0).unwrap())
    }
    async fn count(&self, sql: &str, args: Vec<libsql::Value>) -> i64 {
        match self.scalar(sql, args).await {
            Some(libsql::Value::Integer(n)) => n,
            other => panic!("{sql} -> {other:?}"),
        }
    }
    async fn settled(&self, domain: &str, id: Uuid) -> Value {
        self.wait(domain, id, |v| v["phase"] == "settled").await
    }
    /// Polls the children page until the predicate holds for the named child.
    async fn wait_child(
        &self,
        domain: &str,
        id: Uuid,
        relative: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let page = self.children(domain, id, "?limit=100").await;
                if let Some(child) = by_path(&page).get(relative) {
                    if done(child) {
                        return child.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("scan child did not reach the expected state")
    }
    async fn resume(&self, domain: &str, id: Uuid, child: &Value, body: Value) -> (u16, Value) {
        self.call(
            "POST",
            &format!(
                "/api/v1/{domain}/downloaded-scan-commands/{id}/children/{}/resume",
                child["id"].as_str().unwrap()
            ),
            body,
        )
        .await
    }
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.scratch.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
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
    async fn admit(&self, domain: &str, id: Uuid, path: &Path, mode: &str) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/api/v1/{domain}/downloaded-scan-commands"),
            json!({"id":id,"path":path,"mode":mode,"priority":"normal"}),
        )
        .await
    }
    async fn get(&self, domain: &str, id: Uuid) -> (u16, Value) {
        self.call(
            "GET",
            &format!("/api/v1/{domain}/downloaded-scan-commands/{id}"),
            Value::Null,
        )
        .await
    }
    async fn children(&self, domain: &str, id: Uuid, query: &str) -> Value {
        let (code, page) = self
            .call(
                "GET",
                &format!("/api/v1/{domain}/downloaded-scan-commands/{id}/children{query}"),
                Value::Null,
            )
            .await;
        assert_eq!(code, 200, "{page}");
        page
    }
    /// Polls the real worker until the predicate holds for the command detail.
    async fn wait(&self, domain: &str, id: Uuid, done: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (code, v) = self.get(domain, id).await;
                assert_eq!(code, 200, "{v}");
                if done(&v) {
                    return v;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("scan command did not reach the expected state")
    }
}
// Series 1 and movie 1 deliberately share a numeric ID; movie 2 serves single-file sources.
const SEED: &str = "INSERT INTO series(id,title,path)VALUES(1,'Harbor','/fictional/tv');INSERT INTO seasons(series_id,number)VALUES(1,0),(1,1);INSERT INTO episodes(id,series_id,season,number,title,runtime,absolute_episode_number,air_date)VALUES(1,1,1,1,'Pilot',45,47,'2026-09-24'),(2,1,1,2,'Next',45,48,'2026-09-25');INSERT INTO movie_metadata(id,title,year,runtime,original_language)VALUES(1,'Harbor',2020,100,1),(2,'Orbit',2021,100,1);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/fictional/movie'),(2,2,'/fictional/orbit');INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD');INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,0),(1,'tv',7,1,1),(2,'movies',3,0,0),(2,'movies',7,1,1);INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',0,7,100,100,100,NULL),(2,'movies',0,7,100,100,100,2);INSERT INTO library_settings(media_type,series_id,quality_profile_id,series_type,use_scene_numbering)VALUES('tv',1,1,'standard',0);INSERT INTO library_settings(media_type,movie_id,quality_profile_id,minimum_availability)VALUES('movies',1,2,'released'),('movies',2,2,'released');";
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
fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn by_path(page: &Value) -> std::collections::BTreeMap<String, Value> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["relative_path"].as_str().unwrap().to_owned(), c.clone()))
        .collect()
}

#[tokio::test]
async fn both_domains_with_colliding_ids_publish_one_complete_immutable_inventory_each() {
    let mut h = Harness::new().await;
    // TV folder: nested accepted file, a duplicate for the same episode, and several rejections.
    let tv = h.dir("tv-source").join("Harbor.S01.1080p.WEB-DL");
    write(&tv.join("Harbor.S01E01.1080p.WEB-DL.flv"), &wav());
    write(&tv.join("dupe/Harbor.S01E01.1080p.WEB-DL.flv"), &wav());
    write(
        &tv.join("nested/deep/Harbor.S01E02.1080p.WEB-DL.flv"),
        &wav(),
    );
    write(&tv.join("Other.Show.S01E01.1080p.WEB-DL.flv"), &wav());
    write(&tv.join("readme.nfo"), b"text");
    let tv_bytes = std::fs::read(tv.join("nested/deep/Harbor.S01E02.1080p.WEB-DL.flv")).unwrap();
    // Movie folder with the same numeric owner ID as the series.
    let movie = h.dir("movie-source").join("Harbor.2020.1080p.WEB-DL");
    write(&movie.join("Harbor.2020.1080p.WEB-DL.flv"), &wav());
    write(&movie.join("Harbor.1999.1080p.WEB-DL.flv"), &wav());
    write(&movie.join("notes.txt"), b"text");
    // Movie file source (not a folder).
    let single = h.dir("single-source").join("Orbit.2021.1080p.WEB-DL.flv");
    write(&single, &wav());

    let (tv_id, movie_id, file_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (code, body) = h.admit("tv", tv_id, &tv, "auto").await;
    assert_eq!(code, 202, "{body}");
    assert_eq!(body["phase"], "queued");
    assert_eq!(h.admit("movies", movie_id, &movie, "copy").await.0, 202);
    assert_eq!(h.admit("movies", file_id, &single, "move").await.0, 202);
    h.start_worker().await;

    // The import continuation runs right after publication. The /fictional library roots do
    // not exist, so every selected child is refused at prepare (a measured pre-journal
    // failure with no operation) and the parent settles succeeded/unsuccessful. Changed from
    // the phase A expectation (running/awaiting_import) because phase B now consumes parked
    // parents; the published inventory facts asserted below are unchanged.
    let tv_done = h.settled("tv", tv_id).await;
    let movie_done = h.settled("movies", movie_id).await;
    let file_done = h.settled("movies", file_id).await;
    for done in [&tv_done, &movie_done, &file_done] {
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["outcome"], "unsuccessful");
        assert_eq!(done["attempts"], 1);
        assert_eq!(done["inventory_revision"], 1);
        assert_eq!(done["source"], "found");
        assert_eq!(done["counts"]["imported"], 0);
        assert_eq!(done["counts"]["ready"], 0);
        assert_eq!(done["counts"]["recovery_required"], 0);
    }
    // TV: file entries plus directories are all children; exactly the two best files are ready.
    let page = h.children("tv", tv_id, "?limit=100").await;
    let children = by_path(&page);
    assert_eq!(page["total"], 8, "{page}");
    assert_eq!(tv_done["counts"]["entries"], 8);
    assert_eq!(tv_done["counts"]["failed"], 2);
    assert_eq!(tv_done["counts"]["rejected"], 6);
    let deep = &children["nested/deep/Harbor.S01E02.1080p.WEB-DL.flv"];
    assert_eq!(deep["disposition"], "failed");
    assert_eq!(deep["error_code"], "hc021_root_missing_prepare");
    assert_eq!(deep["operation_id"], Value::Null);
    assert_eq!(deep["recovery_required"], false);
    assert_eq!(deep["selected"], true);
    assert_eq!(deep["series_id"], 1);
    assert_eq!(deep["movie_id"], Value::Null);
    assert_eq!(deep["member_count"], 1);
    assert_eq!(deep["resolved_mode"], "move"); // untracked auto moves
    assert_eq!(
        deep["destination"],
        "/fictional/tv/Harbor.S01E02.1080p.WEB-DL.flv"
    );
    assert!(matches!(
        deep["observation"].as_str(),
        Some("measured" | "unavailable")
    ));
    // The tied duplicate resolves deterministically before any effect, not by directory order.
    assert_eq!(
        children["dupe/Harbor.S01E01.1080p.WEB-DL.flv"]["selected"],
        true
    );
    assert_eq!(
        children["Harbor.S01E01.1080p.WEB-DL.flv"]["reasons"],
        json!(["superseded_by_better_file"])
    );
    assert_eq!(
        children["readme.nfo"]["reasons"],
        json!(["unsupported_file_type"])
    );
    assert_eq!(
        children["Other.Show.S01E01.1080p.WEB-DL.flv"]["reasons"],
        json!(["no_library_match"])
    );
    assert_eq!(children["nested"]["kind"], "directory");
    assert_eq!(children["nested"]["disposition"], "rejected");

    // Movies: ID 1 stays a movie; the folder's year-mismatched file and sidecar are rejected.
    let page = h.children("movies", movie_id, "?limit=100").await;
    let children = by_path(&page);
    let accepted = &children["Harbor.2020.1080p.WEB-DL.flv"];
    assert_eq!(accepted["disposition"], "failed");
    assert_eq!(accepted["selected"], true);
    assert_eq!(accepted["movie_id"], 1);
    assert_eq!(accepted["series_id"], Value::Null);
    assert_eq!(accepted["resolved_mode"], "copy");
    assert_eq!(
        accepted["destination"],
        "/fictional/movie/Harbor.2020.1080p.WEB-DL.flv"
    );
    assert_eq!(accepted["member_count"], 0);
    assert_eq!(
        children["Harbor.1999.1080p.WEB-DL.flv"]["disposition"],
        "rejected"
    );
    assert_eq!(
        children["notes.txt"]["reasons"],
        json!(["unsupported_file_type"])
    );
    assert_eq!(movie_done["counts"]["failed"], 1);
    // Single file source: one child whose relative path is its own name.
    let page = h.children("movies", file_id, "").await;
    assert_eq!(page["total"], 1);
    let only = &page["items"][0];
    assert_eq!(only["relative_path"], "Orbit.2021.1080p.WEB-DL.flv");
    assert_eq!(only["movie_id"], 2);
    assert_eq!(only["source_path"], single.to_str().unwrap());
    assert_eq!(only["resolved_mode"], "move");

    // Domain isolation: the same UUID is not visible or cancellable from the other domain.
    assert_eq!(h.get("movies", tv_id).await.0, 404);
    assert_eq!(h.get("tv", movie_id).await.0, 404);
    assert_eq!(
        h.call(
            "POST",
            &format!("/api/v1/movies/downloaded-scan-commands/{tv_id}/cancel"),
            Value::Null
        )
        .await
        .0,
        404
    );
    assert_eq!(
        h.call(
            "GET",
            &format!("/api/v1/movies/downloaded-scan-commands/{tv_id}/children"),
            Value::Null
        )
        .await
        .0,
        404
    );
    // Lists are per-domain pages.
    let (_, listed) = h
        .call(
            "GET",
            "/api/v1/movies/downloaded-scan-commands",
            Value::Null,
        )
        .await;
    assert_eq!(
        (listed["total"].as_i64(), listed["limit"].as_i64()),
        (Some(2), Some(25))
    );
    let (_, listed) = h
        .call(
            "GET",
            "/api/v1/tv/downloaded-scan-commands?limit=1",
            Value::Null,
        )
        .await;
    assert_eq!(listed["items"][0]["id"], json!(tv_id));

    // The published inventory is immutable: files that appear later are not discovered, and
    // restarting the worker neither resets the parent nor re-enumerates.
    write(&tv.join("Harbor.S01E02.1080p.WEB-DL.flv"), &wav());
    h.stop_worker().await;
    h.start_worker().await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let after = h.children("tv", tv_id, "?limit=100").await;
    assert_eq!(after["total"], 8);
    assert!(!by_path(&after).contains_key("Harbor.S01E02.1080p.WEB-DL.flv"));
    let (_, same) = h.get("tv", tv_id).await;
    // Continuations add scheduling generations, never attempts; terminal proof is immutable.
    assert_eq!(
        (same["status"].as_str(), same["attempts"].as_i64()),
        (Some("succeeded"), Some(1))
    );
    assert!(same["run_generation"].as_i64().unwrap() > 1);
    // No transfer: every source byte and path is untouched.
    assert_eq!(
        std::fs::read(tv.join("nested/deep/Harbor.S01E02.1080p.WEB-DL.flv")).unwrap(),
        tv_bytes
    );
    assert!(single.exists() && movie.join("Harbor.2020.1080p.WEB-DL.flv").exists());
    assert!(!Path::new("/fictional").exists());

    // Cancel on a settled parent is an idempotent no-op that cannot rewrite terminal proof.
    // (The parked-parent cancel path is covered deterministically by a unit test.)
    let (code, unchanged) = h
        .call(
            "POST",
            &format!("/api/v1/tv/downloaded-scan-commands/{tv_id}/cancel"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200, "{unchanged}");
    assert_eq!(
        (
            unchanged["status"].as_str(),
            unchanged["cancel_requested"].as_bool()
        ),
        (Some("succeeded"), Some(false))
    );
    h.stop_worker().await;
}

#[tokio::test]
async fn nonexistent_empty_and_unusable_sources_settle_without_children_or_effects() {
    let mut h = Harness::new().await;
    let missing = h
        .scratch
        .0
        .join("never-created")
        .join("Harbor.S01E01.1080p.WEB-DL.flv");
    let empty = h.dir("empty-release");
    let deep = (0..34).fold(h.dir("deep"), |p, i| p.join(format!("d{i}")));
    std::fs::create_dir_all(&deep).unwrap();
    let denied = h.dir("denied-release");
    write(
        &denied.join("private/Harbor.S01E01.1080p.WEB-DL.flv"),
        &wav(),
    );
    std::fs::set_permissions(
        denied.join("private"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let really_denied = std::fs::read_dir(denied.join("private")).is_err();
    let (missing_id, empty_id, deep_id, denied_id) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    for (domain, id, path) in [
        ("tv", missing_id, &missing),
        ("movies", empty_id, &empty),
        ("tv", deep_id, &h.scratch.0.join("deep")),
        ("tv", denied_id, &denied),
    ] {
        assert_eq!(h.admit(domain, id, path, "auto").await.0, 202);
    }
    h.start_worker().await;
    let terminal = |v: &Value| v["phase"] == "settled";
    // Valid nonexistent path: a successful execution with an unsuccessful outcome, no retry.
    let done = h.wait("tv", missing_id, terminal).await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["outcome"].as_str(),
            done["source"].as_str(),
            done["attempts"].as_i64(),
            done["error_code"].as_str()
        ),
        (
            Some("succeeded"),
            Some("unsuccessful"),
            Some("missing"),
            Some(1),
            None
        )
    );
    assert_eq!(done["counts"]["entries"], 0);
    let done = h.wait("movies", empty_id, terminal).await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["outcome"].as_str(),
            done["source"].as_str()
        ),
        (Some("succeeded"), Some("unsuccessful"), Some("found"))
    );
    // A depth cap violation publishes nothing and is not retried (it cannot succeed later).
    let done = h.wait("tv", deep_id, terminal).await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["error_code"].as_str(),
            done["attempts"].as_i64()
        ),
        (Some("failed"), Some("scan_inventory_limit"), Some(1))
    );
    assert_eq!(done["inventory_revision"], 0);
    assert_eq!(done["source"], Value::Null);
    assert_eq!(h.children("tv", deep_id, "").await["total"], 0);
    // An unreadable subdirectory is a permission failure, never an empty or partial inventory.
    let done = h.wait("tv", denied_id, terminal).await;
    if really_denied {
        assert_eq!(
            (done["status"].as_str(), done["error_code"].as_str()),
            (Some("failed"), Some("scan_source_denied"))
        );
        assert_eq!(done["inventory_revision"], 0);
        assert_eq!(h.children("tv", denied_id, "").await["total"], 0);
    } else {
        eprintln!("privileged runner can read mode 000; denied-access behavior not established");
    }
    std::fs::set_permissions(
        denied.join("private"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    h.stop_worker().await;
}

#[tokio::test]
async fn admission_is_idempotent_strict_and_bounded() {
    let h = Harness::new().await;
    let source = h.dir("strict").join("Harbor.S01E01.1080p.WEB-DL.flv");
    let id = Uuid::new_v4();
    let (code, first) = h.admit("tv", id, &source, "copy").await;
    assert_eq!(code, 202, "{first}");
    assert_eq!(
        (
            first["mode"].as_str(),
            first["priority"].as_str(),
            first["status"].as_str(),
            first["tracking"].as_str()
        ),
        (
            Some("copy"),
            Some("normal"),
            Some("queued"),
            Some("untracked")
        )
    );
    // Exact replay returns the existing command without a second row or capacity use.
    let (code, replay) = h.admit("tv", id, &source, "copy").await;
    assert_eq!((code, &replay["id"]), (200, &first["id"]));
    assert_eq!(replay["created_at"], first["created_at"]);
    let count = |h: &Harness| {
        let db = h.db.clone();
        async move {
            db.connect()
                .await
                .unwrap()
                .query("SELECT count(*) FROM downloaded_scan_commands", ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap()
        }
    };
    assert_eq!(count(&h).await, 1);
    // Any differing field, or the other domain, is a mismatch.
    let other = h.dir("strict-other").join("x.flv");
    for (domain, path, mode, priority) in [
        ("tv", &other, "copy", "normal"),
        ("tv", &source, "move", "normal"),
        ("tv", &source, "copy", "high"),
        ("movies", &source, "copy", "normal"),
    ] {
        let (code, body) = h
            .call(
                "POST",
                &format!("/api/v1/{domain}/downloaded-scan-commands"),
                json!({"id":id,"path":path,"mode":mode,"priority":priority}),
            )
            .await;
        assert_eq!(code, 409, "{body}");
        assert_eq!(body["error"]["code"], "downloaded_scan_conflict");
    }
    assert_eq!(count(&h).await, 1);
    // Strict validation: nothing below may create a row.
    let long = format!("/{}", "a".repeat(4096));
    let base = |patch: Value| {
        let mut v =
            json!({"id":Uuid::new_v4(),"path":"/ok/Harbor.flv","mode":"auto","priority":"normal"});
        for (k, x) in patch.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let tracked = |provider: &str, remote: &str| json!({"provider_id":provider,"remote_id":remote});
    let bad = [
        base(json!({"path":""})),
        base(json!({"path":"relative/path.mkv"})),
        base(json!({"path":"/a/../b.mkv"})),
        base(json!({"path":"/a/./b.mkv"})),
        base(json!({"path":"/a/b/"})),
        base(json!({"path":"/"})),
        base(json!({"path":"/a\u{0}b"})),
        base(json!({"path":long})),
        base(json!({"mode":"hardlink"})),
        base(json!({"mode":"AUTO"})),
        base(json!({"priority":"urgent"})),
        base(json!({"id":"not-a-uuid"})),
        base(json!({"unexpected":true})),
        base(json!({"supersedes_command_id":Uuid::new_v4()})),
        base(json!({"tracked":tracked(&Uuid::new_v4().to_string(), "short")})),
        base(json!({"tracked":tracked(&Uuid::new_v4().to_string(), &"A".repeat(40))})),
        base(
            json!({"tracked":{"provider_id":Uuid::new_v4(),"remote_id":"a".repeat(40),"extra":1}}),
        ),
    ];
    for body in bad {
        let (code, response) = h
            .call("POST", "/api/v1/tv/downloaded-scan-commands", body.clone())
            .await;
        assert_eq!(code, 400, "{body} -> {response}");
    }
    let oversize = base(json!({"path":format!("/{}", "b".repeat(9000))}));
    assert_eq!(
        h.call("POST", "/api/v1/tv/downloaded-scan-commands", oversize)
            .await
            .0,
        400
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/tv/downloaded-scan-commands?x=1",
            base(json!({}))
        )
        .await
        .0,
        400
    );
    // Legal newline and tab filenames are valid text paths.
    let (code, odd) = h
        .call(
            "POST",
            "/api/v1/tv/downloaded-scan-commands",
            base(json!({"path":"/ok/with\nnewline\tand tab.mkv"})),
        )
        .await;
    assert_eq!(code, 202, "{odd}");
    assert_eq!(odd["path"], "/ok/with\nnewline\tand tab.mkv");
    // A tracked request needs a known provider scoped to the domain; unknown ones are rejected.
    let (code, body) = h
        .call(
            "POST",
            "/api/v1/tv/downloaded-scan-commands",
            base(json!({"tracked":tracked(&Uuid::new_v4().to_string(), &"a".repeat(40))})),
        )
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (409, Some("downloaded_scan_provider_unknown"))
    );
    assert_eq!(count(&h).await, 2);
    // Query strictness, pagination bounds and malformed identities.
    for path in [
        "/api/v1/tv/downloaded-scan-commands?limit=0",
        "/api/v1/tv/downloaded-scan-commands?limit=101",
        "/api/v1/tv/downloaded-scan-commands?offset=100001",
        "/api/v1/tv/downloaded-scan-commands?unknown=1",
        "/api/v1/tv/downloaded-scan-commands/not-a-uuid",
        &format!(
            "/api/v1/tv/downloaded-scan-commands/{}",
            id.to_string().to_uppercase()
        ),
        &format!("/api/v1/tv/downloaded-scan-commands/{id}/children?limit=101"),
        &format!("/api/v1/tv/downloaded-scan-commands/{id}?x=1"),
    ] {
        assert_eq!(h.call("GET", path, Value::Null).await.0, 400, "{path}");
    }
    assert_eq!(h.get("tv", Uuid::new_v4()).await.0, 404);
    let (_, page) = h
        .call(
            "GET",
            "/api/v1/tv/downloaded-scan-commands?status=queued",
            Value::Null,
        )
        .await;
    assert_eq!(
        (
            page["total"].as_i64(),
            page["limit"].as_i64(),
            page["offset"].as_i64()
        ),
        (Some(2), Some(25), Some(0))
    );
    // Cancel before any worker exists is immediate, idempotent, and never creates children.
    for _ in 0..2 {
        let (code, cancelled) = h
            .call(
                "POST",
                &format!("/api/v1/tv/downloaded-scan-commands/{id}/cancel"),
                Value::Null,
            )
            .await;
        assert_eq!(
            (
                code,
                cancelled["status"].as_str(),
                cancelled["attempts"].as_i64()
            ),
            (200, Some("cancelled"), Some(0))
        );
    }
    // The cancelled request still deduplicates a late replay and mismatch.
    assert_eq!(h.admit("tv", id, &source, "copy").await.0, 200);
    assert_eq!(h.admit("tv", id, &source, "move").await.0, 409);
}

#[tokio::test]
async fn tracked_requests_with_unknown_remote_fall_back_visibly_and_known_providers_are_required() {
    let mut h = Harness::new().await;
    let provider = Uuid::new_v4();
    let c = h.db.connect().await.unwrap();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Tracked fixture',1,1,1,1,'http://127.0.0.1:1/')", [provider.to_string()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','movies','movies',0,1,'started','default',0,0,0)", [provider.to_string()]).await.unwrap();
    let source = h.dir("tracked").join("Harbor.2020.1080p.WEB-DL.flv");
    write(&source, &wav());
    let remote = "ab".repeat(20);
    let id = Uuid::new_v4();
    let body = json!({"id":id,"path":source,"mode":"auto","priority":"high","tracked":{"provider_id":provider,"remote_id":remote}});
    let (code, created) = h
        .call(
            "POST",
            "/api/v1/movies/downloaded-scan-commands",
            body.clone(),
        )
        .await;
    assert_eq!(code, 202, "{created}");
    assert_eq!(
        (created["tracking"].as_str(), created["priority"].as_str()),
        (Some("pending"), Some("high"))
    );
    assert_eq!(
        created["tracked"],
        json!({"provider_id":provider,"remote_id":remote})
    );
    // The provider is scoped to movies only: TV cannot use it.
    let (code, scoped) = h
        .call("POST", "/api/v1/tv/downloaded-scan-commands", json!({"id":Uuid::new_v4(),"path":"/x/y.mkv","mode":"auto","priority":"normal","tracked":{"provider_id":provider,"remote_id":remote}}))
        .await;
    assert_eq!(
        (code, scoped["error"]["code"].as_str()),
        (409, Some("downloaded_scan_provider_unknown"))
    );
    // Replay of the tracked request is exact; a different remote identifier is a mismatch.
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/movies/downloaded-scan-commands",
            body.clone()
        )
        .await
        .0,
        200
    );
    let mut changed = body;
    changed["tracked"]["remote_id"] = json!("cd".repeat(20));
    assert_eq!(
        h.call("POST", "/api/v1/movies/downloaded-scan-commands", changed)
            .await
            .0,
        409
    );
    h.start_worker().await;
    let done = h.settled("movies", id).await;
    assert_eq!(done["tracking"], "unknown_fallback");
    // Selected by fallback identification, then refused at prepare: /fictional is absent.
    assert_eq!(done["counts"]["failed"], 1, "{done}");
    assert_eq!(done["outcome"], "unsuccessful");
    h.stop_worker().await;
}

#[tokio::test]
async fn scan_commands_share_the_1024_command_pool_with_every_other_producer() {
    let h = Harness::new().await;
    let c = h.db.connect().await.unwrap();
    // Far-future due time keeps these inert; each insert passes the real admission trigger.
    for n in 0..1024 {
        c.execute(
            "INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at) VALUES(?,'tv',?,'copy',1,9007199254740000,1)",
            libsql::params![Uuid::new_v4().to_string(), format!("/pad/{n}")],
        )
        .await
        .unwrap();
    }
    // Another producer's pre-check counts scan rows: a clean 429, not a trigger abort.
    let (code, body) = h
        .call(
            "POST",
            "/api/v1/quality-definitions/reset-commands",
            json!({"media_type":"tv","reset_titles":false,"priority":"normal"}),
        )
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (429, Some("command_history_full")),
        "{body}"
    );
    // The scan producer itself reports capacity the same way, but exact replay still works.
    let (code, body) = h
        .admit("tv", Uuid::new_v4(), Path::new("/pad/new.mkv"), "copy")
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (429, Some("command_history_full"))
    );
    // Read on a scoped connection: an idle cursor on `c` could otherwise hold a database lock.
    let (known, known_path): (String, String) = {
        let reader = h.db.connect().await.unwrap();
        let mut rows = reader
            .query(
                "SELECT id,source_path FROM downloaded_scan_commands LIMIT 1",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        (row.get(0).unwrap(), row.get(1).unwrap())
    };
    let (code, body) = h
        .call(
            "POST",
            "/api/v1/tv/downloaded-scan-commands",
            json!({"id":known,"path":known_path,"mode":"copy","priority":"normal"}),
        )
        .await;
    assert_eq!(code, 200, "{body}");
    // Cancelling one frees exactly one slot for any producer.
    let (code, _) = h
        .call(
            "POST",
            &format!("/api/v1/tv/downloaded-scan-commands/{known}/cancel"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200);
    let (code, body) = h
        .call(
            "POST",
            "/api/v1/quality-definitions/reset-commands",
            json!({"media_type":"tv","reset_titles":false,"priority":"normal"}),
        )
        .await;
    assert_eq!(code, 202, "{body}");
}

// ---------------------------------------------------------------------------------------
// Phase B: sequential import continuation
// ---------------------------------------------------------------------------------------

fn text(v: &str) -> libsql::Value {
    libsql::Value::Text(v.into())
}
/// Distinct payloads so byte-for-byte assertions cannot be satisfied by another file.
fn payload(tag: u8) -> Vec<u8> {
    let mut bytes = wav();
    bytes.extend(std::iter::repeat_n(tag, 4096));
    bytes
}
fn operation_of(child: &Value) -> String {
    child["operation_id"]
        .as_str()
        .expect("linked operation")
        .to_owned()
}

#[tokio::test]
async fn both_domains_import_one_physical_file_each_with_colliding_ids_and_visible_rejections() {
    let mut h = Harness::new().await;
    let (tv_root, movie_root) = h.library().await;
    let tv = h.dir("tv-source").join("Harbor.S01.1080p.WEB-DL");
    let multi = tv.join("Harbor.S01E01-E02.1080p.WEB-DL.flv");
    write(&multi, &payload(1));
    write(&tv.join("Other.Show.S01E01.1080p.WEB-DL.flv"), &payload(2));
    write(&tv.join("readme.nfo"), b"text");
    let movie = h.dir("movie-source").join("Harbor.2020.1080p.WEB-DL");
    let film = movie.join("Harbor.2020.1080p.WEB-DL.flv");
    write(&film, &payload(3));
    let (tv_id, movie_id) = (Uuid::new_v4(), Uuid::new_v4());
    // Untracked auto resolves move; the movie is an explicit copy.
    assert_eq!(h.admit("tv", tv_id, &tv, "auto").await.0, 202);
    assert_eq!(h.admit("movies", movie_id, &movie, "copy").await.0, 202);
    h.start_worker().await;
    let tv_done = h.settled("tv", tv_id).await;
    let movie_done = h.settled("movies", movie_id).await;
    for done in [&tv_done, &movie_done] {
        assert_eq!(
            (
                done["status"].as_str(),
                done["outcome"].as_str(),
                done["attempts"].as_i64(),
                done["error_code"].as_str()
            ),
            (Some("succeeded"), Some("success"), Some(1), None),
            "{done}"
        );
        assert_eq!(done["counts"]["imported"], 1);
        assert_eq!(done["counts"]["ready"], 0);
    }
    // Mixed outcome: success with every rejection still visible on its child.
    assert_eq!(tv_done["counts"]["rejected"], 2);
    let children = by_path(&h.children("tv", tv_id, "?limit=100").await);
    assert_eq!(
        children["Other.Show.S01E01.1080p.WEB-DL.flv"]["reasons"],
        json!(["no_library_match"])
    );
    assert_eq!(children["readme.nfo"]["disposition"], "rejected");
    let imported = &children["Harbor.S01E01-E02.1080p.WEB-DL.flv"];
    assert_eq!(imported["disposition"], "imported");
    assert_eq!(imported["member_count"], 2);
    assert_eq!(imported["operation_phase"], "complete");
    assert_eq!(imported["recovery_required"], false);
    let tv_op = operation_of(imported);

    // TV: one physical transfer (moved), one episode file serving both members, one physical
    // history event with relational membership for every member.
    let destination = tv_root.join("Harbor.S01E01-E02.1080p.WEB-DL.flv");
    assert_eq!(std::fs::read(&destination).unwrap(), payload(1));
    assert!(!multi.exists(), "move retires the source file");
    assert_eq!(
        h.count("SELECT count(*) FROM episode_files", vec![]).await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(DISTINCT e.episode_file_id) FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id IN (1,2) AND f.series_id=1 AND f.path=?",
            vec![text(destination.to_str().unwrap())]
        )
        .await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM episodes WHERE id IN (1,2) AND episode_file_id IS NOT NULL",
            vec![]
        )
        .await,
        2
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_history WHERE operation_id=? AND media_type='episode' AND episode_id=1",
            vec![text(&tv_op)]
        )
        .await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_history_episodes WHERE operation_id=? AND episode_id IN (1,2)",
            vec![text(&tv_op)]
        )
        .await,
        2
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_journal WHERE operation_id=? AND phase='complete' AND path_failure_code IS NULL",
            vec![text(&tv_op)]
        )
        .await,
        1
    );

    // Movie 1 shares the numeric ID with series 1 yet owns a separate movie file; copy keeps
    // the source bytes in place.
    let movie_child = h.children("movies", movie_id, "").await["items"][0].clone();
    assert_eq!(movie_child["disposition"], "imported");
    let movie_op = operation_of(&movie_child);
    let movie_destination = movie_root.join("Harbor.2020.1080p.WEB-DL.flv");
    assert_eq!(std::fs::read(&movie_destination).unwrap(), payload(3));
    assert_eq!(
        std::fs::read(&film).unwrap(),
        payload(3),
        "copy keeps source"
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM movie_files WHERE movie_id=1 AND path=?",
            vec![text(movie_destination.to_str().unwrap())]
        )
        .await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_history WHERE operation_id=? AND media_type='movie' AND movie_id=1 AND episode_id IS NULL",
            vec![text(&movie_op)]
        )
        .await,
        1
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM import_history_episodes WHERE operation_id=?",
            vec![text(&movie_op)]
        )
        .await,
        0
    );

    // Scan-owned operations are a distinct producer: generic execute and manual command
    // admission are refused even after completion; status remains readable.
    for op in [&tv_op, &movie_op] {
        let (code, body) = h
            .call(
                "POST",
                &format!("/api/v1/imports/{op}/execute"),
                Value::Null,
            )
            .await;
        assert_eq!(
            (code, body["error"]["code"].as_str()),
            (409, Some("scan_recovery_required")),
            "{body}"
        );
        let (code, body) = h
            .call(
                "POST",
                "/api/v1/manual-import/commands",
                json!({"operation_ids":[op],"priority":"normal"}),
            )
            .await;
        assert_eq!(code, 409, "{body}");
        let (code, status) = h
            .call("GET", &format!("/api/v1/imports/{op}"), Value::Null)
            .await;
        assert_eq!((code, status["status"].as_str()), (200, Some("complete")));
    }
    // Exactly one transfer per physical file: no second operation was ever created.
    assert_eq!(
        h.count("SELECT count(*) FROM scan_import_facts", vec![])
            .await,
        2
    );
    h.stop_worker().await;
}

#[tokio::test]
async fn failed_replacement_keeps_every_original_and_explicit_resume_is_generation_fenced() {
    let mut h = Harness::new().await;
    let (tv_root, _) = h.library().await;
    // Two distinct lower-quality originals, one per member episode.
    let old_one = tv_root.join("Harbor.S01E01.720p.HDTV.mkv");
    let old_two = tv_root.join("Harbor.S01E02.720p.HDTV.mkv");
    write(&old_one, &payload(10));
    write(&old_two, &payload(20));
    let c = h.db.connect().await.unwrap();
    for (id, path) in [(1, &old_one), (2, &old_two)] {
        c.execute(
            "INSERT INTO episode_files(id,series_id,path)VALUES(?,1,?)",
            libsql::params![id, path.to_str().unwrap()],
        )
        .await
        .unwrap();
        c.execute("INSERT INTO file_metadata(media_type,episode_file_id,quality_id,revision_json)VALUES('tv',?,3,'{\"version\":1,\"real\":0,\"is_repack\":false}')", [id]).await.unwrap();
        c.execute("UPDATE episodes SET episode_file_id=? WHERE id=?", [id, id])
            .await
            .unwrap();
    }
    drop(c);
    let source = h.dir("upgrade").join("Harbor.S01E01-E02.1080p.WEB-DL");
    let multi = source.join("Harbor.S01E01-E02.1080p.WEB-DL.flv");
    write(&multi, &payload(30));
    // Forced failure: the library directory refuses the private staging directory, so the
    // first execution fails after arming but before any library byte changes.
    std::fs::set_permissions(&tv_root, std::fs::Permissions::from_mode(0o555)).unwrap();
    let denied = std::fs::create_dir(tv_root.join("probe")).is_err();
    let id = Uuid::new_v4();
    assert_eq!(h.admit("tv", id, &source, "copy").await.0, 202);
    h.start_worker().await;
    let name = "Harbor.S01E01-E02.1080p.WEB-DL.flv";
    let mut operation = None;
    if denied {
        let failed = h
            .wait_child("tv", id, name, |c| c["recovery_required"] == true)
            .await;
        assert_eq!(failed["disposition"], "failed");
        assert_eq!(failed["path_failure_code"], "stage_denied_precommit");
        assert_eq!(failed["resume_generation"], 0);
        let parent = h.get("tv", id).await.1;
        assert_eq!(
            (parent["status"].as_str(), parent["phase"].as_str()),
            (Some("running"), Some("recovery_required")),
            "{parent}"
        );
        assert_eq!(parent["counts"]["recovery_required"], 1);
        // The originals are untouched in bytes, paths and associations; nothing was published.
        assert_eq!(std::fs::read(&old_one).unwrap(), payload(10));
        assert_eq!(std::fs::read(&old_two).unwrap(), payload(20));
        assert!(!tv_root.join(name).exists());
        assert_eq!(
            h.count(
                "SELECT count(*) FROM episodes WHERE (id=1 AND episode_file_id=1) OR (id=2 AND episode_file_id=2)",
                vec![]
            )
            .await,
            2
        );
        assert_eq!(
            h.count("SELECT count(*) FROM episode_files", vec![]).await,
            2
        );
        // No automatic retry: the parent waits for an explicit resume.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let still = h
            .wait_child("tv", id, name, |c| c["disposition"] == "failed")
            .await;
        assert_eq!(still["resume_generation"], 0);
        std::fs::set_permissions(&tv_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let op = operation_of(&failed);
        // Stop the worker so the replay below deterministically precedes consumption; once
        // the worker consumes a generation it can no longer be replayed (asserted after).
        h.stop_worker().await;
        let body = |op: &str, generation: i64| json!({"operation_id":op,"inventory_revision":1,"expected_resume_generation":generation});
        // Wrong operation, revision or generation never authorizes anything.
        let (code, wrong) = h
            .resume("tv", id, &failed, body(&Uuid::new_v4().to_string(), 0))
            .await;
        assert_eq!(
            (code, wrong["error"]["code"].as_str()),
            (409, Some("downloaded_scan_resume_conflict"))
        );
        let (code, wrong) = h.resume("tv", id, &failed, body(&op, 3)).await;
        assert_eq!(
            (code, wrong["error"]["code"].as_str()),
            (409, Some("downloaded_scan_resume_generation"))
        );
        assert_eq!(
            h.resume("movies", id, &failed, body(&op, 0)).await.0,
            404,
            "domain scoped"
        );
        let (code, resumed) = h.resume("tv", id, &failed, body(&op, 0)).await;
        assert_eq!(code, 200, "{resumed}");
        // A lost response is reconciled by replaying the identical body: no second grant.
        let (code, replay) = h.resume("tv", id, &failed, body(&op, 0)).await;
        assert_eq!(code, 200, "{replay}");
        assert_eq!(
            (
                replay["resume_generation"].as_i64(),
                replay["resume_requested"].as_bool()
            ),
            (Some(1), Some(true))
        );
        h.start_worker().await;
        h.wait_child("tv", id, name, |c| c["resume_requested"] == false)
            .await;
        // Consumed: the same body can no longer authorize or replay anything.
        let (code, consumed) = h.resume("tv", id, &failed, body(&op, 0)).await;
        assert_eq!(code, 409, "{consumed}");
        operation = Some(op);
    } else {
        std::fs::set_permissions(&tv_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!(
            "privileged runner can write mode 0555; forced replacement failure not established"
        );
    }
    let done = h.settled("tv", id).await;
    assert_eq!(
        (done["status"].as_str(), done["outcome"].as_str()),
        (Some("succeeded"), Some("success")),
        "{done}"
    );
    let child = h.children("tv", id, "").await["items"][0].clone();
    let op = operation_of(&child);
    if let Some(expected) = operation {
        assert_eq!(op, expected, "resume reuses the same operation");
        assert_eq!(child["resume_generation"], 1);
    }
    // One new physical file for both members; each distinct original is quarantined in its
    // own recovery directory with its bytes intact, and its row now names that artifact.
    assert_eq!(std::fs::read(tv_root.join(name)).unwrap(), payload(30));
    for (file, bytes) in [(1, payload(10)), (2, payload(20))] {
        let recovery = tv_root
            .join(format!(".hrrdarr-replaced-{op}-{file}"))
            .join("original");
        assert_eq!(std::fs::read(&recovery).unwrap(), bytes);
        assert_eq!(
            h.count(
                "SELECT count(*) FROM episode_files WHERE id=? AND path=?",
                vec![
                    libsql::Value::Integer(file),
                    text(recovery.to_str().unwrap())
                ]
            )
            .await,
            1
        );
    }
    assert!(!old_one.exists() && !old_two.exists());
    assert_eq!(
        h.count(
            "SELECT count(*) FROM scan_import_replacements WHERE operation_id=? AND retirement_state='quarantined'",
            vec![text(&op)]
        )
        .await,
        2
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id IN (1,2) AND f.path=?",
            vec![text(tv_root.join(name).to_str().unwrap())]
        )
        .await,
        2
    );
    assert_eq!(
        std::fs::read(&multi).unwrap(),
        payload(30),
        "copy keeps source"
    );
    // A settled parent cannot be resumed again.
    let (code, _) = h
        .resume(
            "tv",
            id,
            &child,
            json!({"operation_id":op,"inventory_revision":1,"expected_resume_generation":1}),
        )
        .await;
    assert_eq!(code, 409);
    h.stop_worker().await;
}

#[tokio::test]
async fn cancel_between_children_stops_after_current_and_never_transfers_the_next() {
    let mut h = Harness::new().await;
    let (tv_root, _) = h.library().await;
    let source = h.dir("cancel").join("Harbor.S01.1080p.WEB-DL");
    let first = source.join("Harbor.S01E01.1080p.WEB-DL.flv");
    let second = source.join("Harbor.S01E02.1080p.WEB-DL.flv");
    write(&first, &payload(1));
    write(&second, &payload(2));
    let id = Uuid::new_v4();
    assert_eq!(h.admit("tv", id, &source, "move").await.0, 202);
    h.start_worker().await;
    // Children run in ordinal order, one at a time; cancel once the first has started.
    h.wait_child("tv", id, "Harbor.S01E01.1080p.WEB-DL.flv", |c| {
        matches!(c["disposition"].as_str(), Some("importing" | "imported"))
    })
    .await;
    let (code, cancelling) = h
        .call(
            "POST",
            &format!("/api/v1/tv/downloaded-scan-commands/{id}/cancel"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200, "{cancelling}");
    assert_eq!(cancelling["cancel_requested"], true);
    let done = h.settled("tv", id).await;
    assert_eq!(
        (done["status"].as_str(), done["outcome"].as_str()),
        (Some("cancelled"), None),
        "{done}"
    );
    assert_eq!(done["counts"]["imported"], 1);
    assert_eq!(done["counts"]["cancelled"], 1);
    let children = by_path(&h.children("tv", id, "").await);
    assert_eq!(
        children["Harbor.S01E01.1080p.WEB-DL.flv"]["disposition"],
        "imported"
    );
    let skipped = &children["Harbor.S01E02.1080p.WEB-DL.flv"];
    assert_eq!(skipped["disposition"], "cancelled");
    assert_eq!(skipped["operation_id"], Value::Null, "never prepared");
    // The in-flight child completed; the next child never transferred.
    assert_eq!(
        std::fs::read(tv_root.join("Harbor.S01E01.1080p.WEB-DL.flv")).unwrap(),
        payload(1)
    );
    assert!(!first.exists());
    assert!(!tv_root.join("Harbor.S01E02.1080p.WEB-DL.flv").exists());
    assert_eq!(std::fs::read(&second).unwrap(), payload(2));
    assert_eq!(
        h.count(
            "SELECT count(*) FROM episodes WHERE id=2 AND episode_file_id IS NULL",
            vec![]
        )
        .await,
        1
    );
    h.stop_worker().await;
}

#[tokio::test]
async fn worker_restart_during_an_import_settles_the_same_operation_once_without_reimporting() {
    let mut h = Harness::new().await;
    let (_, movie_root) = h.library().await;
    let source = h.dir("restart").join("Harbor.2020.1080p.WEB-DL");
    let film = source.join("Harbor.2020.1080p.WEB-DL.flv");
    // Large enough that the detached transfer usually outlives the worker stop.
    let mut big = wav();
    big.resize(24 * 1024 * 1024, 7);
    write(&film, &big);
    let id = Uuid::new_v4();
    assert_eq!(h.admit("movies", id, &source, "move").await.0, 202);
    h.start_worker().await;
    let started = h
        .wait_child("movies", id, "Harbor.2020.1080p.WEB-DL.flv", |c| {
            matches!(c["disposition"].as_str(), Some("importing" | "imported"))
        })
        .await;
    let op = operation_of(&started);
    // Stop the worker: the detached engine task keeps its permit and finishes on its own.
    h.stop_worker().await;
    let journal_done = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let child = h
                .wait_child("movies", id, "Harbor.2020.1080p.WEB-DL.flv", |_| true)
                .await;
            if child["operation_phase"] == "complete" {
                return child;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if let Ok(child) = &journal_done {
        // The journal completed with no worker: settlement is still pending, not fabricated.
        if started["disposition"] == "importing" {
            assert_eq!(child["disposition"], "importing", "{child}");
        }
    }
    // Record which interleaving this run exercised; both must reach the same single result.
    eprintln!(
        "restart_observation started_as={} journal_complete_while_stopped={}",
        started["disposition"],
        journal_done.is_ok()
    );
    let before = h.get("movies", id).await.1;
    h.start_worker().await;
    let done = h.settled("movies", id).await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["outcome"].as_str(),
            done["attempts"].as_i64()
        ),
        (Some("succeeded"), Some("success"), Some(1)),
        "{done}"
    );
    assert!(done["run_generation"].as_i64() > before["run_generation"].as_i64());
    let child = h.children("movies", id, "").await["items"][0].clone();
    assert_eq!(
        operation_of(&child),
        op,
        "same operation before and after restart"
    );
    // Exactly one transfer, one operation and one history row.
    assert_eq!(
        h.count("SELECT count(*) FROM scan_import_facts", vec![])
            .await,
        1
    );
    assert_eq!(h.count("SELECT count(*) FROM operations", vec![]).await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM import_history", vec![]).await,
        1
    );
    assert_eq!(
        std::fs::read(movie_root.join("Harbor.2020.1080p.WEB-DL.flv"))
            .unwrap()
            .len(),
        big.len()
    );
    assert!(!film.exists());
    h.stop_worker().await;
}
