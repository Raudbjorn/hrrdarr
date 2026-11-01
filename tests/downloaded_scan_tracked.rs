//! Real HTTP + real worker coverage of downloaded-folder scan phase C: tracked receipt
//! reservation, typed coverage and the processing handback, and move-mode source-directory
//! cleanup with its explicit resume. Temporary directories and databases, 127.0.0.1:0
//! listeners; no live service is contacted (the client endpoint 127.0.0.1:1 is never needed).
use hrrdarr::{
    commands, completed_download_handling, db::Database, health, metadata::MetadataClient,
    providers,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

/// Hang detector for polling the real one-step-per-second worker, not a performance bound: one
/// scenario needs ~25 s of ticks on an idle host, so a 30 s window expired whenever the suite ran
/// its scenarios side by side (issue #38). A passing wait returns as soon as its state holds.
const WAIT: Duration = Duration::from_secs(120);

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
        let app = commands::router(db.clone())
            .merge(hrrdarr::import::router(db.clone()))
            .merge(health::router(db.clone()))
            .merge(completed_download_handling::router(db.clone()));
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
        tokio::time::timeout(WAIT, async {
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
            .timeout(Duration::from_secs(60))
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
        tokio::time::timeout(WAIT, async {
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

fn text(v: &str) -> libsql::Value {
    libsql::Value::Text(v.into())
}
fn payload(tag: u8) -> Vec<u8> {
    let mut bytes = wav();
    bytes.extend(std::iter::repeat_n(tag, 4096));
    bytes
}
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// One indexer and one qBittorrent client, both scoped to TV AND movies: the same provider
/// serves both domains, so only the route's domain can tell receipts apart.
struct Providers {
    indexer: String,
    client: String,
}
async fn providers(h: &Harness) -> Providers {
    let c = h.db.connect().await.unwrap();
    let (indexer, client) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    for (id, implementation) in [(&indexer, "torznab"), (&client, "qbittorrent")] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://127.0.0.1:1/')", libsql::params![id.clone(), implementation, implementation]).await.unwrap();
    }
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','tv','[5000]','[]',0,NULL)", [indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab','movies','[2000]','[]',NULL,0)", [indexer.clone()]).await.unwrap();
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)", libsql::params![client.clone(), media, media]).await.unwrap();
        c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode,enabled_override) VALUES(?,?,1,1,1,'copy',1)", libsql::params![client.clone(), media]).await.unwrap();
    }
    Providers { indexer, client }
}
/// An observed RSS receipt for series 1 episodes or movie 1 (numeric IDs collide on purpose).
async fn receipt(
    h: &Harness,
    p: &Providers,
    media: &str,
    title: &str,
    episodes: &[i64],
    tag: char,
) -> (String, String) {
    let c = h.db.connect().await.unwrap();
    let (command, candidate, hash) = (
        Uuid::new_v4().to_string(),
        Uuid::new_v4().to_string(),
        tag.to_string().repeat(40),
    );
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync',?,?,1,?,1,100,100)", libsql::params![command.clone(), media, p.indexer.clone(), p.client.clone()]).await.unwrap();
    let (series, movie) = if media == "tv" {
        (Some(1), None)
    } else {
        (None, Some(1))
    };
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,?,?,?,?,'pending','[]',100,100 FROM rss_commands WHERE id=?", libsql::params![candidate.clone(), tag.to_string().repeat(64), title, vec![1u8; 29], series, movie, command]).await.unwrap();
    for episode in episodes {
        c.execute(
            "INSERT INTO rss_candidate_episodes VALUES(?,1,?)",
            libsql::params![candidate.clone(), *episode],
        )
        .await
        .unwrap();
    }
    let target = if media == "tv" {
        json!({"media_type":"episode","id":episodes[0]})
    } else {
        json!({"media_type":"movie","id":1})
    };
    let identity = json!({"version":1,"target":target,"hashes":[hash],"settings_fingerprint":"c".repeat(64),"payload_sha256":"d".repeat(64)}).to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        libsql::params![identity, candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO rss_hash_claims VALUES(?,?,?)",
        libsql::params![p.client.clone(), hash.clone(), candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
        [candidate.clone()],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
        libsql::params![hash.clone(), candidate.clone()],
    )
    .await
    .unwrap();
    (candidate, hash)
}
/// A processing receipt with a measured hc021 marker: queued -> checking -> blocked. A blocked
/// receipt is not claimed by the worker, so the scan is its only possible owner.
async fn blocked_processing(h: &Harness, candidate: &str) {
    let c = h.db.connect().await.unwrap();
    c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,1,'queued',100,100,100)", [candidate]).await.unwrap();
    c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1,updated_at=101 WHERE candidate_id=?", [candidate]).await.unwrap();
    c.execute("UPDATE download_processing SET status='blocked',error_code='path_mapping_missing',reasons_json='[\"hc021_mapping_missing\",\"path_mapping_missing\"]',updated_at=102 WHERE candidate_id=?", [candidate]).await.unwrap();
}
async fn processing(h: &Harness, candidate: &str) -> Value {
    let (code, v) = h
        .call(
            "GET",
            &format!("/api/v1/download-processing/{candidate}"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200, "{v}");
    v
}
async fn generation(h: &Harness, media: &str) -> i64 {
    h.count(
        "SELECT generation FROM health_checks WHERE scope=? AND check_key='remote_path_mapping'",
        vec![text(media)],
    )
    .await
}
async fn admit_tracked(
    h: &Harness,
    domain: &str,
    id: Uuid,
    path: &Path,
    mode: &str,
    p: &Providers,
    hash: &str,
) -> (u16, Value) {
    h.call(
        "POST",
        &format!("/api/v1/{domain}/downloaded-scan-commands"),
        json!({"id":id,"path":path,"mode":mode,"priority":"normal","tracked":{"provider_id":p.client,"remote_id":hash}}),
    )
    .await
}

async fn admit_successor(
    h: &Harness,
    domain: &str,
    id: Uuid,
    path: &Path,
    tracked: Option<(&str, &str)>,
    previous: Uuid,
) -> (u16, Value) {
    let mut body = json!({"id":id,"path":path,"mode":"copy","priority":"normal","supersedes_command_id":previous});
    if let Some((provider, remote)) = tracked {
        body["tracked"] = json!({"provider_id":provider,"remote_id":remote});
    }
    h.call(
        "POST",
        &format!("/api/v1/{domain}/downloaded-scan-commands"),
        body,
    )
    .await
}
fn code(response: &(u16, Value)) -> (u16, Option<&str>) {
    (response.0, response.1["error"]["code"].as_str())
}
/// Health's remote-path check is scoped to completed-download handling; enable it explicitly.
async fn enable_cdh(h: &Harness, domain: &str) {
    let route = format!("/api/v1/{domain}/completed-download-handling");
    let (status, settings) = h.call("GET", &route, Value::Null).await;
    assert_eq!(status, 200, "{settings}");
    let (status, saved) = h
        .call(
            "PUT",
            &route,
            json!({"enabled":true,"revision":settings["revision"]}),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
}
/// The remote_path_mapping check and its published issue, via the real Health API.
async fn path_check(h: &Harness, domain: &str) -> Value {
    let (status, value) = h
        .call(
            "GET",
            &format!("/api/v1/health?scope={domain}"),
            Value::Null,
        )
        .await;
    assert_eq!(status, 200, "{value}");
    let mut check = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["check_key"] == "remote_path_mapping")
        .unwrap()
        .clone();
    check["issue"] = value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["identity"]["check_key"] == "remote_path_mapping")
        .cloned()
        .unwrap_or(Value::Null);
    check
}
/// Requests a fresh Health evaluation through the owned worker and waits for its publication.
async fn recheck(h: &Harness, domain: &str) -> Value {
    let previous = path_check(h, domain).await;
    let (status, body) = h
        .call(
            "POST",
            "/api/v1/health/commands",
            json!({"scope":domain,"priority":"high"}),
        )
        .await;
    assert_eq!(status, 202, "{body}");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = path_check(h, domain).await;
            if value["evaluation"] == "current"
                && value["observed_generation"].as_i64().unwrap()
                    > previous["observed_generation"].as_i64().unwrap_or(-1)
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("owned Health worker must publish a fresh path observation")
}

#[tokio::test]
async fn tracked_receipts_in_both_domains_hand_back_full_coverage_and_reject_cross_domain_hashes() {
    let mut h = Harness::new().await;
    let (tv_root, movie_root) = h.library().await;
    let p = providers(&h).await;
    // Series 1 / episode 1 and movie 1 share numeric IDs; the provider serves both domains.
    let (tv_receipt, tv_hash) =
        receipt(&h, &p, "tv", "Harbor.S01E01.1080p.WEB-DL", &[1], 'a').await;
    let (movie_receipt, movie_hash) =
        receipt(&h, &p, "movies", "Harbor.2020.1080p.WEB-DL", &[], 'b').await;
    blocked_processing(&h, &tv_receipt).await;
    blocked_processing(&h, &movie_receipt).await;
    let before = (generation(&h, "tv").await, generation(&h, "movies").await);
    let tv = h.dir("dl-tv").join("Harbor.S01E01.1080p.WEB-DL");
    let episode = tv.join("Harbor.S01E01.1080p.WEB-DL.flv");
    write(&episode, &payload(1));
    let movie = h.dir("dl-movie").join("Harbor.2020.1080p.WEB-DL");
    let film = movie.join("Harbor.2020.1080p.WEB-DL.flv");
    write(&film, &payload(2));
    let (tv_id, movie_id, cross_id) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    assert_eq!(
        admit_tracked(&h, "tv", tv_id, &tv, "auto", &p, &tv_hash)
            .await
            .0,
        202
    );
    assert_eq!(
        admit_tracked(&h, "movies", movie_id, &movie, "auto", &p, &movie_hash)
            .await
            .0,
        202
    );
    // The TV receipt's hash under the movie route: known, but another domain's receipt.
    assert_eq!(
        admit_tracked(&h, "movies", cross_id, &movie, "auto", &p, &tv_hash)
            .await
            .0,
        202
    );
    h.start_worker().await;
    let cross = h.settled("movies", cross_id).await;
    assert_eq!(
        (
            cross["status"].as_str(),
            cross["error_code"].as_str(),
            cross["receipt_id"].as_str()
        ),
        (Some("failed"), Some("scan_tracked_receipt_domain"), None),
        "{cross}"
    );
    assert_eq!(cross["inventory_revision"], 0);
    for (domain, id, candidate) in [
        ("tv", tv_id, &tv_receipt),
        ("movies", movie_id, &movie_receipt),
    ] {
        let done = h.settled(domain, id).await;
        assert_eq!(
            (
                done["status"].as_str(),
                done["outcome"].as_str(),
                done["tracking"].as_str()
            ),
            (Some("succeeded"), Some("success"), Some("receipt")),
            "{done}"
        );
        assert_eq!(done["receipt_id"].as_str(), Some(candidate.as_str()));
        assert_eq!(done["predecessor_operation_id"], Value::Null);
        assert_eq!(
            done["coverage"],
            json!({"state":"complete","expected":1,"covered":1,"missing":0})
        );
        // Tracked auto never implies move when the client's move capability is unknown.
        assert_eq!(done["cleanup"]["state"], "not_needed", "{done}");
        let child = &h.children(domain, id, "?disposition=imported").await["items"][0];
        assert_eq!(child["resolved_mode"], "copy");
        // Handback: full typed coverage -> imported through the scan, hc021 marker cleared
        // only by that proven completion; preflight lifetime counters are preserved.
        let receipt = processing(&h, candidate).await;
        assert_eq!(
            (
                receipt["status"].as_str(),
                receipt["error_code"].as_str(),
                receipt["scan_command_id"].as_str()
            ),
            (Some("imported"), None, Some(id.to_string().as_str())),
            "{receipt}"
        );
        assert_eq!(receipt["reasons"], json!([]));
        assert_eq!(
            (
                receipt["preflight_attempts"].as_i64(),
                receipt["total_preflight_attempts"].as_i64(),
                receipt["next_attempt_at"].as_i64(),
                receipt["created_at"].as_i64()
            ),
            (Some(1), Some(1), Some(100), Some(100))
        );
        // A singleton retry cannot bypass the scan's settled proof.
        let (code, body) = h
            .call(
                "POST",
                "/api/v1/download-processing",
                json!({"provider_id":p.client,"provider_revision":1,"media_type":domain,"receipt_ids":[candidate]}),
            )
            .await;
        assert_eq!(
            (code, body["error"]["code"].as_str()),
            (409, Some("processing_reserved_by_scan")),
            "{body}"
        );
    }
    let after = (generation(&h, "tv").await, generation(&h, "movies").await);
    assert!(
        after.0 > before.0 && after.1 > before.1,
        "{before:?} {after:?}"
    );
    // Copy keeps both sources; each library received its own file.
    assert_eq!(std::fs::read(&episode).unwrap(), payload(1));
    assert_eq!(std::fs::read(&film).unwrap(), payload(2));
    assert_eq!(
        std::fs::read(tv_root.join("Harbor.S01E01.1080p.WEB-DL.flv")).unwrap(),
        payload(1)
    );
    assert_eq!(
        std::fs::read(movie_root.join("Harbor.2020.1080p.WEB-DL.flv")).unwrap(),
        payload(2)
    );
    // TV membership is paginated per child; a movie child has none.
    let tv_child = h.children("tv", tv_id, "?disposition=imported").await["items"][0].clone();
    let (code, members) = h
        .call(
            "GET",
            &format!(
                "/api/v1/tv/downloaded-scan-commands/{tv_id}/children/{}/members?limit=1",
                tv_child["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200, "{members}");
    assert_eq!(members["total"], 1);
    assert_eq!(
        members["items"][0],
        json!({"episode_id":1,"series_id":1,"season_number":1,"episode_number":1,"expected_old_episode_file_id":null})
    );
    // The same command under the other domain is unknown.
    let (code, _) = h
        .call(
            "GET",
            &format!(
                "/api/v1/movies/downloaded-scan-commands/{tv_id}/children/{}/members",
                tv_child["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(code, 404);
    h.stop_worker().await;
}

#[tokio::test]
async fn partial_coverage_hands_back_blocked_and_preserves_the_hc021_marker() {
    let mut h = Harness::new().await;
    h.library().await;
    let p = providers(&h).await;
    // Receipts are singletons (0025 admits one episode per candidate). The folder holds no
    // acceptable file for the receipt's episode, so its expected coverage stays unproven.
    let (candidate, hash) = receipt(&h, &p, "tv", "Harbor.S01E01.1080p.WEB-DL", &[1], 'e').await;
    blocked_processing(&h, &candidate).await;
    let before = processing(&h, &candidate).await;
    let health = generation(&h, "tv").await;
    let folder = h.dir("dl-season").join("Harbor.S01E01.1080p.WEB-DL");
    write(&folder.join("Harbor.S01E02.1080p.WEB-DL.flv"), &payload(3));
    write(&folder.join("readme.nfo"), b"text");
    let id = Uuid::new_v4();
    assert_eq!(
        admit_tracked(&h, "tv", id, &folder, "copy", &p, &hash)
            .await
            .0,
        202
    );
    h.start_worker().await;
    let done = h.settled("tv", id).await;
    assert_eq!(
        (done["status"].as_str(), done["outcome"].as_str()),
        (Some("succeeded"), Some("unsuccessful")),
        "{done}"
    );
    assert_eq!(
        done["coverage"],
        json!({"state":"incomplete","expected":1,"covered":0,"missing":1})
    );
    // The other episode's file contradicts the receipt's release folder: a visible rejection.
    let children = by_path(&h.children("tv", id, "?limit=100").await);
    assert_eq!(
        children["Harbor.S01E02.1080p.WEB-DL.flv"]["reasons"],
        json!(["folder_target_mismatch"])
    );
    let after = processing(&h, &candidate).await;
    assert_eq!(
        (
            after["status"].as_str(),
            after["error_code"].as_str(),
            after["scan_command_id"].as_str()
        ),
        (
            Some("blocked"),
            Some("import_failed"),
            Some(id.to_string().as_str())
        ),
        "{after}"
    );
    // The retained path marker stays first; the ordinary reason says why it is still blocked.
    assert_eq!(
        after["reasons"],
        json!(["hc021_mapping_missing", "downloaded_scan_incomplete"])
    );
    for field in [
        "policy_revision",
        "preflight_attempts",
        "total_preflight_attempts",
        "next_attempt_at",
        "created_at",
    ] {
        assert_eq!(after[field], before[field], "{field}");
    }
    assert!(generation(&h, "tv").await > health);
    assert_eq!(
        h.count(
            "SELECT count(*) FROM downloaded_scan_coverage WHERE command_id=? AND credited_operation_id IS NULL AND episode_id=1",
            vec![text(&id.to_string())]
        )
        .await,
        1
    );
    // The partial proof keeps the receipt reserved: no singleton retry or cancel bypass.
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/download-processing".to_string(),
            json!({"provider_id":p.client,"provider_revision":1,"media_type":"tv","receipt_ids":[candidate]}),
        ),
        (
            "POST",
            format!("/api/v1/download-processing/{candidate}/cancel"),
            Value::Null,
        ),
    ] {
        let (code, body) = h.call(method, &path, body).await;
        assert_eq!(
            (code, body["error"]["code"].as_str()),
            (409, Some("processing_reserved_by_scan")),
            "{path} {body}"
        );
    }
    h.stop_worker().await;
}

#[tokio::test]
async fn a_running_predecessor_is_waited_for_never_stolen_and_its_history_is_coverage() {
    let mut h = Harness::new().await;
    let (tv_root, _) = h.library().await;
    let p = providers(&h).await;
    let (candidate, hash) = receipt(&h, &p, "tv", "Harbor.S01E01.1080p.WEB-DL", &[1], 'f').await;
    let folder = h.dir("dl-pack").join("Harbor.S01E01.1080p.WEB-DL");
    let first = folder.join("Harbor.S01E01.1080p.WEB-DL.flv");
    let second = folder.join("Harbor.S01E02.1080p.WEB-DL.flv");
    write(&first, &payload(4));
    write(&second, &payload(5));
    // A linked singleton predecessor for episode 1 that is still unfinished (preview with a
    // measured error, so the singleton worker leaves it for its own explicit resume).
    let predecessor = Uuid::new_v4().to_string();
    let destination = tv_root.join("Harbor.S01E01.1080p.WEB-DL.flv");
    let c = h.db.connect().await.unwrap();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message)VALUES(?,'episode',1,?,'copy',?,'preview','Owned download import')", libsql::params![predecessor.clone(), first.to_str().unwrap(), destination.to_str().unwrap()]).await.unwrap();
    c.execute("INSERT INTO import_journal(operation_id,plan_json,phase,error_code)VALUES(?,'{}','preview','import_failed')", [predecessor.clone()]).await.unwrap();
    c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at)VALUES(?,1,'queued',100,100,100)", [candidate.clone()]).await.unwrap();
    c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?", [candidate.clone()]).await.unwrap();
    c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json)VALUES(?,?,7,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}')", libsql::params![candidate.clone(), predecessor.clone()]).await.unwrap();
    c.execute("UPDATE download_processing SET status='importing',error_code='import_failed',reasons_json='[\"import_failed\"]' WHERE candidate_id=?", [candidate.clone()]).await.unwrap();
    drop(c);
    let id = Uuid::new_v4();
    assert_eq!(
        admit_tracked(&h, "tv", id, &folder, "copy", &p, &hash)
            .await
            .0,
        202
    );
    h.start_worker().await;
    let waiting = h
        .wait("tv", id, |v| v["phase"] == "waiting_predecessor")
        .await;
    assert_eq!(
        (
            waiting["status"].as_str(),
            waiting["inventory_revision"].as_i64(),
            waiting["attempts"].as_i64()
        ),
        (Some("running"), Some(0), Some(1)),
        "{waiting}"
    );
    assert_eq!(
        waiting["predecessor_operation_id"].as_str(),
        Some(predecessor.as_str())
    );
    assert_eq!(waiting["receipt_id"].as_str(), Some(candidate.as_str()));
    // Several worker turns later: still waiting, no children, and the predecessor's own
    // authority (journal, link and processing lifetime) is untouched by the scan.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let still = h.get("tv", id).await.1;
    assert_eq!(
        (still["phase"].as_str(), still["attempts"].as_i64()),
        (Some("waiting_predecessor"), Some(1))
    );
    assert_eq!(
        h.count(
            "SELECT count(*) FROM downloaded_scan_children WHERE command_id=?",
            vec![text(&id.to_string())]
        )
        .await,
        0
    );
    assert_eq!(h.count("SELECT count(*) FROM import_journal WHERE operation_id=? AND phase='preview' AND error_code='import_failed'", vec![text(&predecessor)]).await, 1);
    let owner = processing(&h, &candidate).await;
    assert_eq!(
        (
            owner["status"].as_str(),
            owner["operation_id"].as_str(),
            owner["total_preflight_attempts"].as_i64()
        ),
        (Some("importing"), Some(predecessor.as_str()), Some(1)),
        "{owner}"
    );
    // The predecessor's owner finishes under its own authority (simulated by the same rows
    // its commit writes: file, association, history, complete journal, imported receipt).
    write(&destination, &payload(4));
    let c = h.db.connect().await.unwrap();
    c.execute(
        "INSERT INTO episode_files(id,series_id,path)VALUES(1,1,?)",
        [destination.to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute("UPDATE episodes SET episode_file_id=1 WHERE id=1", ())
        .await
        .unwrap();
    c.execute("INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)VALUES(?,'episode',1,1,?,?,?,?)", libsql::params![predecessor.clone(), first.to_str().unwrap(), destination.to_str().unwrap(), payload(4).len() as i64, "e".repeat(64)]).await.unwrap();
    c.execute("UPDATE import_journal SET phase='complete',error_code=NULL,stage_json='{}' WHERE operation_id=?", [predecessor.clone()]).await.unwrap();
    c.execute("UPDATE download_processing SET status='imported',error_code=NULL,reasons_json='[]' WHERE candidate_id=?", [candidate.clone()]).await.unwrap();
    // Wake the idle wait instead of sleeping for its poll interval.
    c.execute(
        "UPDATE downloaded_scan_commands SET next_attempt_at=0 WHERE id=?",
        [id.to_string()],
    )
    .await
    .unwrap();
    drop(c);
    let done = h.settled("tv", id).await;
    assert_eq!(
        (
            done["status"].as_str(),
            done["outcome"].as_str(),
            done["attempts"].as_i64()
        ),
        (Some("succeeded"), Some("unsuccessful"), Some(1)),
        "{done}"
    );
    // Episode 1 is the predecessor's historical coverage (its source is already imported, so
    // it is no new success of this scan); the episode-2 file contradicts the release folder.
    let children = by_path(&h.children("tv", id, "?limit=100").await);
    assert_eq!(
        children["Harbor.S01E01.1080p.WEB-DL.flv"]["disposition"],
        "already_imported"
    );
    assert_eq!(
        children["Harbor.S01E02.1080p.WEB-DL.flv"]["reasons"],
        json!(["folder_target_mismatch"])
    );
    assert_eq!(
        done["coverage"],
        json!({"state":"complete","expected":1,"covered":1,"missing":0})
    );
    assert_eq!(
        h.count("SELECT count(*) FROM downloaded_scan_coverage WHERE command_id=? AND episode_id=1 AND credited_operation_id=?", vec![text(&id.to_string()), text(&predecessor)]).await,
        1
    );
    // The predecessor owner kept processing; the scan never rewrote it.
    let owner = processing(&h, &candidate).await;
    assert_eq!(
        (owner["status"].as_str(), owner["reasons"].clone()),
        (Some("imported"), json!([]))
    );
    assert_eq!(h.count("SELECT count(*) FROM download_processing WHERE candidate_id=? AND scan_command_id IS NULL", vec![text(&candidate)]).await, 1);
    assert!(!tv_root.join("Harbor.S01E02.1080p.WEB-DL.flv").exists());
    assert_eq!(std::fs::read(&second).unwrap(), payload(5));
    h.stop_worker().await;
}

#[tokio::test]
async fn move_cleanup_removes_only_emptied_directories_and_copy_needs_none() {
    let mut h = Harness::new().await;
    h.library().await;
    let base = h.dir("dl-move");
    // Leftovers: a sample (rejected by its directory), an unknown file in its own directory,
    // and a directory that is emptied by the move.
    let mixed = base.join("Harbor.S01.1080p.WEB-DL");
    write(&mixed.join("Harbor.S01E01.1080p.WEB-DL.flv"), &payload(6));
    write(
        &mixed
            .join("Season 1")
            .join("Harbor.S01E02.1080p.WEB-DL.flv"),
        &payload(7),
    );
    write(
        &mixed.join("Sample").join("Harbor.S01E01.sample.flv"),
        &payload(8),
    );
    write(&mixed.join("Extras").join("notes.txt"), b"keep me");
    let mixed_id = Uuid::new_v4();
    assert_eq!(h.admit("tv", mixed_id, &mixed, "move").await.0, 202);
    // A movie folder whose only content is moved: the whole folder goes.
    let lone = h.dir("dl-move-movie").join("Harbor.2020.1080p.WEB-DL");
    write(&lone.join("Harbor.2020.1080p.WEB-DL.flv"), &payload(9));
    let lone_id = Uuid::new_v4();
    assert_eq!(h.admit("movies", lone_id, &lone, "auto").await.0, 202);
    // An explicit copy never cleans anything.
    let copied = h.dir("dl-copy").join("Orbit.2021.1080p.WEB-DL");
    write(&copied.join("Orbit.2021.1080p.WEB-DL.flv"), &payload(10));
    let copy_id = Uuid::new_v4();
    assert_eq!(h.admit("movies", copy_id, &copied, "copy").await.0, 202);
    h.start_worker().await;

    let done = h.settled("tv", mixed_id).await;
    assert_eq!(
        (done["status"].as_str(), done["outcome"].as_str()),
        (Some("succeeded"), Some("success")),
        "{done}"
    );
    assert_eq!(done["counts"]["imported"], 2, "{done}");
    assert_eq!(done["cleanup"]["state"], "retained", "{done}");
    assert_eq!(done["cleanup"]["phase"], "settled");
    assert_eq!(done["cleanup"]["reason"], "unimported_media");
    assert_eq!(done["cleanup"]["recovery_required"], false);
    // Season 1 and both private move directories were removed; nothing else was touched.
    assert!(done["cleanup"]["removed"].as_i64().unwrap() >= 3, "{done}");
    assert!(mixed.exists(), "a folder with leftovers is retained");
    assert_eq!(
        names(&mixed),
        vec!["Extras".to_string(), "Sample".to_string()]
    );
    assert_eq!(
        std::fs::read(mixed.join("Sample").join("Harbor.S01E01.sample.flv")).unwrap(),
        payload(8)
    );
    assert_eq!(
        std::fs::read(mixed.join("Extras").join("notes.txt")).unwrap(),
        b"keep me"
    );

    let lone_done = h.settled("movies", lone_id).await;
    assert_eq!(lone_done["counts"]["imported"], 1, "{lone_done}");
    assert_eq!(lone_done["cleanup"]["state"], "complete", "{lone_done}");
    assert_eq!(lone_done["cleanup"]["reason"], Value::Null);
    assert!(!lone.exists(), "an emptied scanned folder is removed");
    assert!(
        lone.parent().unwrap().exists(),
        "never above the scanned source"
    );

    let copy_done = h.settled("movies", copy_id).await;
    assert_eq!(copy_done["counts"]["imported"], 1, "{copy_done}");
    assert_eq!(copy_done["cleanup"]["state"], "not_needed");
    assert_eq!(
        names(&copied),
        vec!["Orbit.2021.1080p.WEB-DL.flv".to_string()]
    );
    h.stop_worker().await;
}

#[tokio::test]
async fn an_uncertain_cleanup_after_intent_stays_pending_until_a_generation_fenced_resume() {
    let mut h = Harness::new().await;
    h.library().await;
    let source = h.dir("dl-uncertain").join("Harbor.2020.1080p.WEB-DL");
    write(&source.join("Harbor.2020.1080p.WEB-DL.flv"), &payload(11));
    // Injected failure AFTER the removal: settling the checkpoint aborts, so the durable state
    // is an armed removal intent whose effect the worker cannot know.
    let c = h.db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER injected_cleanup_settle BEFORE UPDATE OF cleanup_state ON downloaded_scan_commands WHEN NEW.cleanup_state IN ('complete','retained') BEGIN SELECT RAISE(ABORT,'injected cleanup settlement failure'); END;").await.unwrap();
    drop(c);
    enable_cdh(&h, "movies").await;
    let id = Uuid::new_v4();
    assert_eq!(h.admit("movies", id, &source, "move").await.0, 202);
    h.start_worker().await;
    let stuck = h
        .wait("movies", id, |v| v["cleanup"]["recovery_required"] == true)
        .await;
    // The uncertain removal intent is surfaced through the existing path-diagnostics check.
    let issue = recheck(&h, "movies").await;
    assert_eq!(
        issue["issue"]["reason"], "path_scan_cleanup_uncertain",
        "{issue}"
    );
    assert!(!issue.to_string().contains(source.to_str().unwrap()));
    assert_eq!(
        (
            stuck["status"].as_str(),
            stuck["phase"].as_str(),
            stuck["cleanup"]["state"].as_str(),
            stuck["cleanup"]["phase"].as_str()
        ),
        (
            Some("running"),
            Some("recovery_required"),
            Some("pending"),
            Some("remove_intent")
        ),
        "{stuck}"
    );
    assert_eq!(stuck["cleanup"]["resume_generation"], 0);
    assert!(
        !source.exists(),
        "the removal happened before the failed settlement"
    );
    // Cancellation cannot terminally settle an armed removal intent.
    let (code, cancelled) = h
        .call(
            "POST",
            &format!("/api/v1/movies/downloaded-scan-commands/{id}/cancel"),
            Value::Null,
        )
        .await;
    assert_eq!(
        (code, cancelled["status"].as_str()),
        (200, Some("running")),
        "{cancelled}"
    );
    let c = h.db.connect().await.unwrap();
    c.execute_batch("DROP TRIGGER injected_cleanup_settle;")
        .await
        .unwrap();
    drop(c);
    let route = format!("/api/v1/movies/downloaded-scan-commands/{id}/resume-cleanup");
    let (code, body) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":4}),
        )
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (409, Some("downloaded_scan_cleanup_resume_generation")),
        "{body}"
    );
    let (code, body) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":0,"expected_cleanup_resume_generation":0}),
        )
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (409, Some("downloaded_scan_cleanup_resume_conflict")),
        "{body}"
    );
    let (code, body) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":0,"extra":1}),
        )
        .await;
    assert_eq!(code, 400, "{body}");
    let (code, wrong_domain) = h
        .call(
            "POST",
            &format!("/api/v1/tv/downloaded-scan-commands/{id}/resume-cleanup"),
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":0}),
        )
        .await;
    assert_eq!(code, 404, "{wrong_domain}");
    // Stop the worker so the authorized generation is observable before it is consumed.
    h.stop_worker().await;
    let (code, body) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":0}),
        )
        .await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        (
            body["cleanup"]["resume_generation"].as_i64(),
            body["cleanup"]["resume_requested"].as_bool()
        ),
        (Some(1), Some(true))
    );
    // Exact replay after a lost response: same answer, no second authorization.
    let (code, replay) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":0}),
        )
        .await;
    assert_eq!(
        (code, replay["cleanup"]["resume_generation"].as_i64()),
        (200, Some(1)),
        "{replay}"
    );
    h.start_worker().await;
    let done = h.settled("movies", id).await;
    // Same parent, no replacement and the retained intent: the absent folder is our removal.
    assert_eq!(
        (
            done["status"].as_str(),
            done["cleanup"]["state"].as_str(),
            done["cleanup"]["resume_requested"].as_bool()
        ),
        (Some("cancelled"), Some("complete"), Some(false)),
        "{done}"
    );
    assert_eq!(done["counts"]["imported"], 1);
    // The settled cleanup is no longer evidence.
    assert!(recheck(&h, "movies").await["issue"].is_null());
    let (code, body) = h
        .call(
            "POST",
            &route,
            json!({"inventory_revision":1,"expected_cleanup_resume_generation":1}),
        )
        .await;
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (409, Some("downloaded_scan_cleanup_resume_conflict")),
        "{body}"
    );
    h.stop_worker().await;
}

#[tokio::test]
async fn an_incomplete_tracked_receipt_completes_only_through_one_explicit_successor() {
    let mut h = Harness::new().await;
    let (tv_root, movie_root) = h.library().await;
    let p = providers(&h).await;
    // Receipts are singletons (0025), so an incomplete predecessor carries no credit; the
    // successor must prove the member itself and no credit may be counted twice.
    let cases = [
        (
            "tv",
            "Harbor.S01E01.1080p.WEB-DL",
            vec![1i64],
            'e',
            "Harbor.S01E01.1080p.WEB-DL.flv",
            tv_root.clone(),
            "SELECT count(*) FROM import_history WHERE media_type='episode' AND episode_id=1",
        ),
        (
            "movies",
            "Harbor.2020.1080p.WEB-DL",
            vec![],
            'f',
            "Harbor.2020.1080p.WEB-DL.flv",
            movie_root.clone(),
            "SELECT count(*) FROM import_history WHERE media_type='movie' AND movie_id=1",
        ),
    ];
    let mut prepared = Vec::new();
    for (domain, title, episodes, tag, _, _, _) in &cases {
        let (candidate, hash) = receipt(&h, &p, domain, title, episodes, *tag).await;
        blocked_processing(&h, &candidate).await;
        let folder = h.dir(&format!("dl-successor-{domain}")).join(title);
        // Nothing acceptable for the receipt's member yet: the first scan proves nothing.
        write(&folder.join("readme.nfo"), b"text");
        if *domain == "tv" {
            write(&folder.join("Harbor.S01E02.1080p.WEB-DL.flv"), &payload(21));
        }
        let first = Uuid::new_v4();
        assert_eq!(
            admit_tracked(&h, domain, first, &folder, "copy", &p, &hash)
                .await
                .0,
            202
        );
        prepared.push((candidate, hash, folder, first));
    }
    h.start_worker().await;
    for ((domain, _, _, _, file, root, history), (candidate, hash, folder, first)) in
        cases.iter().zip(&prepared)
    {
        let done = h.settled(domain, *first).await;
        assert_eq!(
            done["coverage"],
            json!({"state":"incomplete","expected":1,"covered":0,"missing":1}),
            "{done}"
        );
        assert_eq!(done["successor_allowed"], true, "{done}");
        assert_eq!(done["successor_command_id"], Value::Null);
        let before = processing(&h, candidate).await;
        assert_eq!(before["status"], "blocked");
        assert_eq!(before["scan_reserved"], true, "{before}");
        // Invalid lineage: every request below is rejected and creates nothing.
        let other_domain = if *domain == "tv" { "movies" } else { "tv" };
        for (route_domain, path, tracked, previous, expected) in [
            (
                *domain,
                folder.join("other"),
                Some((p.client.as_str(), hash.as_str())),
                *first,
                "downloaded_scan_successor_invalid",
            ),
            (
                *domain,
                folder.clone(),
                None,
                *first,
                "downloaded_scan_successor_invalid",
            ),
            (
                *domain,
                folder.clone(),
                Some((p.client.as_str(), "9".repeat(40).as_str())),
                *first,
                "downloaded_scan_successor_invalid",
            ),
            (
                other_domain,
                folder.clone(),
                Some((p.client.as_str(), hash.as_str())),
                *first,
                "downloaded_scan_successor_invalid",
            ),
            (
                *domain,
                folder.clone(),
                Some((p.client.as_str(), hash.as_str())),
                Uuid::new_v4(),
                "downloaded_scan_successor_invalid",
            ),
        ] {
            let response =
                admit_successor(&h, route_domain, Uuid::new_v4(), &path, tracked, previous).await;
            assert_eq!(code(&response), (409, Some(expected)), "{}", response.1);
        }
        // The remaining file arrives; the successor is reviewed and admitted with a NEW id.
        write(&folder.join(file), &payload(22));
        h.stop_worker().await;
        let tracked = Some((p.client.as_str(), hash.as_str()));
        let successor = Uuid::new_v4();
        let admitted = admit_successor(&h, domain, successor, folder, tracked, *first).await;
        assert_eq!(admitted.0, 202, "{}", admitted.1);
        assert_eq!(admitted.1["supersedes_command_id"], first.to_string());
        assert_eq!(admitted.1["successor_allowed"], false);
        // Exact replay is the same command; the same id with other lineage is a conflict.
        let replay = admit_successor(&h, domain, successor, folder, tracked, *first).await;
        assert_eq!(replay.0, 200, "{}", replay.1);
        let different = admit_tracked(&h, domain, successor, folder, "copy", &p, hash).await;
        assert_eq!(code(&different), (409, Some("downloaded_scan_conflict")));
        // The lineage is reserved atomically: a competing successor is refused.
        let competing = admit_successor(&h, domain, Uuid::new_v4(), folder, tracked, *first).await;
        assert_eq!(
            code(&competing),
            (409, Some("downloaded_scan_successor_conflict")),
            "{}",
            competing.1
        );
        // A queued (unsettled) successor cannot itself be superseded.
        let unsettled =
            admit_successor(&h, domain, Uuid::new_v4(), folder, tracked, successor).await;
        assert_eq!(
            code(&unsettled),
            (409, Some("downloaded_scan_successor_not_settled")),
            "{}",
            unsettled.1
        );
        let (_, previous) = h.get(domain, *first).await;
        assert_eq!(
            (
                previous["successor_command_id"].as_str(),
                previous["successor_allowed"].as_bool()
            ),
            (Some(successor.to_string().as_str()), Some(false))
        );
        // The singleton path still cannot bypass the scan-owned proof.
        let (status, body) = h
            .call(
                "POST",
                "/api/v1/download-processing",
                json!({"provider_id":p.client,"provider_revision":1,"media_type":domain,"receipt_ids":[candidate]}),
            )
            .await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (409, Some("processing_reserved_by_scan"))
        );
        h.start_worker().await;
        let done = h.settled(domain, successor).await;
        assert_eq!(
            (
                done["status"].as_str(),
                done["outcome"].as_str(),
                done["tracking"].as_str(),
                done["receipt_id"].as_str()
            ),
            (
                Some("succeeded"),
                Some("success"),
                Some("receipt"),
                Some(candidate.as_str())
            ),
            "{done}"
        );
        assert_eq!(
            done["coverage"],
            json!({"state":"complete","expected":1,"covered":1,"missing":0})
        );
        let children = by_path(&h.children(domain, successor, "?limit=100").await);
        assert_eq!(children[*file]["disposition"], "imported", "{children:?}");
        assert_eq!(children["readme.nfo"]["disposition"], "rejected");
        assert_eq!(
            std::fs::read(root.join(file)).unwrap(),
            payload(22),
            "the remaining file reached the library"
        );
        // Full typed coverage hands the receipt back as imported through the successor;
        // the hc021 marker is cleared only by that proof and lifetime counters are preserved.
        let after = processing(&h, candidate).await;
        assert_eq!(
            (
                after["status"].as_str(),
                after["error_code"].as_str(),
                after["scan_command_id"].as_str(),
                after["scan_reserved"].as_bool()
            ),
            (
                Some("imported"),
                None,
                Some(successor.to_string().as_str()),
                Some(true)
            ),
            "{after}"
        );
        assert_eq!(after["reasons"], json!([]));
        for field in [
            "policy_revision",
            "preflight_attempts",
            "total_preflight_attempts",
            "next_attempt_at",
            "created_at",
        ] {
            assert_eq!(after[field], before[field], "{field}");
        }
        // The predecessor's proof is immutable and the member is credited exactly once.
        let (_, previous) = h.get(domain, *first).await;
        assert_eq!(
            previous["coverage"],
            json!({"state":"incomplete","expected":1,"covered":0,"missing":1})
        );
        assert_eq!(h.count(history, vec![]).await, 1);
        assert_eq!(
            h.count(
                "SELECT count(*) FROM downloaded_scan_coverage WHERE command_id IN (?,?) AND credited_operation_id IS NOT NULL",
                vec![text(&first.to_string()), text(&successor.to_string())]
            )
            .await,
            1
        );
        // Lineage is consumed: neither a complete successor nor its predecessor can be
        // superseded again.
        let again = admit_successor(&h, domain, Uuid::new_v4(), folder, tracked, successor).await;
        assert_eq!(
            code(&again),
            (409, Some("downloaded_scan_successor_not_incomplete")),
            "{}",
            again.1
        );
        let stale = admit_successor(&h, domain, Uuid::new_v4(), folder, tracked, *first).await;
        assert_eq!(
            code(&stale),
            (409, Some("downloaded_scan_successor_conflict")),
            "{}",
            stale.1
        );
    }
    h.stop_worker().await;
}

#[tokio::test]
async fn a_measured_prepare_refusal_is_health_evidence_until_the_source_is_rescanned() {
    let mut h = Harness::new().await;
    enable_cdh(&h, "tv").await;
    // The series root (/fictional/tv) does not exist: the selected child is refused at prepare
    // with a measured path cause and no operation.
    let folder = h.dir("dl-prepare").join("Harbor.S01E01.1080p.WEB-DL");
    write(&folder.join("Harbor.S01E01.1080p.WEB-DL.flv"), &payload(31));
    h.start_worker().await;
    assert!(recheck(&h, "tv").await["issue"].is_null());
    let health = generation(&h, "tv").await;
    let first = Uuid::new_v4();
    assert_eq!(h.admit("tv", first, &folder, "copy").await.0, 202);
    let done = h.settled("tv", first).await;
    assert_eq!(done["outcome"], "unsuccessful", "{done}");
    let child = &h.children("tv", first, "?disposition=failed").await["items"][0];
    assert_eq!(
        (child["error_code"].as_str(), child["operation_id"].as_str()),
        (Some("hc021_root_missing_prepare"), None)
    );
    assert!(
        generation(&h, "tv").await > health,
        "refusal invalidates Health"
    );
    let issue = recheck(&h, "tv").await;
    assert_eq!(
        issue["issue"]["reason"], "path_import_unavailable",
        "{issue}"
    );
    assert!(!issue.to_string().contains(folder.to_str().unwrap()));
    // Repairing the root alone does not clear recorded evidence.
    h.library().await;
    assert_eq!(
        recheck(&h, "tv").await["issue"]["reason"],
        "path_import_unavailable"
    );
    // A new scan of the same source re-observes it (and imports): the evidence is superseded.
    let second = Uuid::new_v4();
    assert_eq!(h.admit("tv", second, &folder, "copy").await.0, 202);
    let done = h.settled("tv", second).await;
    assert_eq!(done["outcome"], "success", "{done}");
    assert!(recheck(&h, "tv").await["issue"].is_null());
    h.stop_worker().await;
}
