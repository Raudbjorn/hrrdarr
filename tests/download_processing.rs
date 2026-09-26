use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use hrrdarr::{commands, db::Database, providers};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
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
#[derive(Default)]
struct Remote {
    items: Mutex<Vec<Value>>,
    adds: Mutex<Vec<String>>,
    mode: AtomicU8,
    completed: AtomicU8,
    detail_reads: AtomicU8,
    endpoint: Mutex<String>,
}
const TV: &str = "1111111111111111111111111111111111111111";
const MOVIE: &str = "2222222222222222222222222222222222222222";
fn item(hash: &str, category: &str) -> Value {
    json!({"hash":hash,"infohash_v1":hash,"infohash_v2":"","category":category,"name":"safe item","state":"downloading","progress":0.5,"size":100,"amount_left":50,"dlspeed":2,"upspeed":1,"eta":25,"ratio":0.2,"seeding_time":0,"priority":5,"force_start":false,"ratio_limit":-2.0,"seeding_time_limit":-2,"inactive_seeding_time_limit":-1,"seq_dl":false,"f_l_piece_prio":false,"auto_tmm":false,"tags":""})
}
async fn remote(
    State(s): State<Arc<Remote>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let q: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    if uri.path() == "/shows/en/101" {
        return axum::Json(json!({"tvdbId":101,"title":"Harbor","seasons":[{"seasonNumber":1}],"episodes":[{"tvdbId":501,"seasonNumber":1,"episodeNumber":1,"title":"Pilot","runtime":45,"airDateUtc":"2020-01-01T00:00:00Z"}]})).into_response();
    }
    if uri.path() == "/movie/201" {
        return axum::Json(json!({"tmdbId":201,"title":"Harbor","year":2020,"runtime":100,"digitalRelease":"2020-01-01T00:00:00Z"})).into_response();
    }
    if uri.path().ends_with("torrents/files") {
        s.detail_reads.fetch_add(1, Ordering::SeqCst);
        if s.mode.load(Ordering::SeqCst) == 11 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let hash = q.get("hash").unwrap();
        let tv = hash == TV || hash.starts_with('3');
        let upgrade = hash.starts_with('3') || hash.starts_with('4');
        let mut name = if upgrade {
            if tv {
                "Harbor.S01E01.1080p.Bluray.mkv"
            } else {
                "Harbor.2020.1080p.Bluray.mkv"
            }
        } else if tv {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        }
        .to_string();
        if s.mode.load(Ordering::SeqCst) == 9 {
            name = if tv {
                "Harbor.S01E02.1080p.WEB-DL.mkv"
            } else {
                "Harbor.2021.1080p.WEB-DL.mkv"
            }
            .into()
        }
        if tv {
            name = format!("batch/{name}")
        }
        let mut files =
            vec![json!({"index":0,"name":name,"size":1048576,"progress":1.0,"priority":1})];
        if s.mode.load(Ordering::SeqCst) == 10 {
            let mut duplicate = files[0].clone();
            duplicate["index"] = json!(1);
            duplicate["name"] = json!(format!("duplicate/{name}"));
            files.push(duplicate);
        }
        files.push(json!({"index":2,"name":if tv{"Harbor.S01E01.sample.1080p.WEB-DL.mkv"}else{"Harbor.2020.sample.1080p.WEB-DL.mkv"},"size":1,"progress":1.0,"priority":1}));
        return axum::Json(json!(files)).into_response();
    }
    if uri.path().ends_with("torrents/properties") {
        return axum::Json(json!({"save_path":"/remote","total_size":1048576,"addition_date":1,"completion_date":2,"seeding_time":0})).into_response();
    }
    if uri.path() == "/api" || uri.path() == "/" {
        if q.get("t").is_some_and(|v| v == "caps") {
            return r#"<caps><limits max="100" default="100"/><searching><search available="yes" supportedParams="q"/><tv-search available="yes" supportedParams="q,tvdbid,season,ep"/><movie-search available="yes" supportedParams="q,tmdbid"/></searching><categories><category id="5000"><subcat id="5030"/></category><category id="2000"><subcat id="2030"/></category></categories></caps>"#.into_response();
        }
        let tv = q.get("cat").is_some_and(|v| v.contains("5030"));
        let (title, hash, cat) = if tv {
            ("Harbor.S01E01.1080p.WEB-DL", TV, 5030)
        } else {
            ("Harbor.2020.1080p.WEB-DL", MOVIE, 2030)
        };
        let hash = match s.mode.load(Ordering::SeqCst) {
            6 => {
                if tv {
                    "3333333333333333333333333333333333333333"
                } else {
                    "4444444444444444444444444444444444444444"
                }
            }
            _ => hash,
        };
        let title = if s.mode.load(Ordering::SeqCst) == 6 {
            if tv {
                "Harbor.S01E01.1080p.Bluray"
            } else {
                "Harbor.2020.1080p.Bluray"
            }
        } else {
            title
        };
        let date = "Mon, 01 Jan 2024 12:00:00 +0000";
        let link = format!(
            "{}/torrent?passkey=PRIVATE_RSS_LOCATOR",
            s.endpoint.lock().unwrap()
        );
        let magnet =
            format!(r#"<torznab:attr name="magneturl" value="magnet:?xt=urn:btih:{hash}"/>"#);
        // Link-only magnets exercise the shared locator normalizer through a real RSS grab.
        let link = if tv {
            link
        } else {
            format!("magnet:?xt=urn:btih:{hash}")
        };
        let magnet = if tv { magnet } else { String::new() };
        return format!(r#"<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>{title}</title><guid>PRIVATE_RSS_GUID-{hash}</guid><pubDate>{date}</pubDate><link>{link}</link>{magnet}<torznab:attr name="category" value="{cat}"/><torznab:attr name="size" value="1073741824"/></item></channel></rss>"#).into_response();
    }
    if uri.path().ends_with("webapiVersion") {
        return "2.8.4".into_response();
    }
    if uri.path().ends_with("preferences") {
        return axum::Json(json!({"queueing_enabled":true,"dht":true,"save_path":"/remote","max_ratio_enabled":false,"max_ratio":-1,"max_seeding_time_enabled":false,"max_seeding_time":-1,"max_ratio_act":0})).into_response();
    }
    if uri.path().ends_with("categories") {
        return axum::Json(
            json!({"tv":{"savePath":"/remote/tv"},"movies":{"savePath":"/remote/movies"}}),
        )
        .into_response();
    }
    if uri.path().ends_with("torrents/info") {
        let items = s
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|v| {
                q.get("category")
                    .is_none_or(|cat| v["category"] == cat.as_str())
                    && q.get("hashes").is_none_or(|hashes| {
                        hashes
                            .split('|')
                            .any(|hash| v["hash"] == hash || v["infohash_v2"] == hash)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let items: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                if s.completed.load(Ordering::SeqCst) == 1 {
                    let tv = item["category"] == "tv";
                    item["state"] = json!("uploading");
                    item["progress"] = json!(1.0);
                    item["amount_left"] = json!(0);
                    item["size"] = json!(1048576);
                    item["save_path"] = json!("/remote");
                    item["content_path"] = json!(if tv {
                        "/remote/Harbor.S01E01.1080p.WEB-DL.mkv"
                    } else {
                        "/remote/Harbor.2020.1080p.WEB-DL.mkv"
                    });
                }
                item
            })
            .collect();
        return axum::Json(items).into_response();
    }
    if uri.path().ends_with("torrents/add") {
        assert_eq!(method, Method::POST);
        let pairs: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(&body).into_owned().collect();
        let category = pairs.get("category").unwrap();
        let hash = if s.mode.load(Ordering::SeqCst) == 6 {
            if category == "tv" {
                "3333333333333333333333333333333333333333"
            } else {
                "4444444444444444444444444444444444444444"
            }
        } else if category == "tv" {
            TV
        } else {
            MOVIE
        };
        s.adds.lock().unwrap().push(hash.into());
        s.items.lock().unwrap().push(item(hash, category));
        return "Ok.".into_response();
    }
    (StatusCode::NOT_FOUND, "unexpected owned mock request").into_response()
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
        .timeout(Duration::from_secs(6))
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
    assert!(
        !text.contains("PRIVATE_RSS_GUID") && !text.contains("PRIVATE_RSS_LOCATOR"),
        "private feed facts cannot enter API responses"
    );
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status} {text}"))
        },
    )
}
async fn providers(base: &str, remote: &str) -> (Value, Value) {
    let mut values = Vec::new();
    for settings in [
        json!({"implementation":"torznab","endpoint":remote,"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2030]}}),
        json!({"implementation":"qbittorrent","endpoint":remote,"tv":{"category":"tv","imported_category":null,"recent_priority":0,"older_priority":0},"movies":{"category":"movies","imported_category":null,"recent_priority":0,"older_priority":0}}),
    ] {
        let (code, value) = request(
            base,
            "POST",
            "/api/v1/providers",
            json!({"name":"owned","enabled":true,"priority":1,"settings":settings}),
        )
        .await;
        assert_eq!(code, 201, "{value}");
        values.push(value)
    }
    (values.remove(0), values.remove(0))
}
fn target(indexer: &Value, client: &Value, media: &str) -> Value {
    json!({"media_type":media,"indexer_id":indexer["id"],"indexer_revision":indexer["revision"],"client_id":client["id"],"client_revision":client["revision"]})
}
async fn enqueue(base: &str, target: Value) -> Value {
    let (code, v) = request(
        base,
        "POST",
        "/api/v1/rss/commands",
        json!({"target":target,"priority":"normal"}),
    )
    .await;
    assert_eq!(code, 202, "{v}");
    v
}
async fn wait_status(base: &str, path: &str, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (code, v) = request(base, "GET", path, Value::Null).await;
            assert_eq!(code, 200, "{v}");
            if v["status"] == status {
                return v;
            }
            if v["status"] == "blocked"
                || v["status"] == "failed"
                || (v["status"] == "importing"
                    && !v["error_code"].is_null()
                    && v["resume_requested"] != true)
            {
                panic!("unexpected terminal: {v}")
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("processing deadline")
}
#[tokio::test]
async fn real_add_rss_owned_completed_downloads_import_both_domains_once() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-processing-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (origin, _upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let router = router
        .merge(commands::router(db.clone()))
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::library::metadata_router(
            db.clone(),
            metadata.clone(),
        ))
        .merge(hrrdarr::remote_paths::router(db.clone()))
        .merge(hrrdarr::naming::router(db.clone()))
        .merge(hrrdarr::history::router(db.clone()));
    let (base, _api) = serve(router).await;
    let c = db.connect().await.unwrap();
    // Profiles are configuration fixtures; all catalog targets, receipts, import journals and history use real producers.
    c.execute_batch("UPDATE quality_definitions SET min_size=0; INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',3,0,1),(2,'movies',3,0,1),(1,'tv',7,1,1),(2,'movies',7,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,7,0,0,1,NULL),(2,'movies',1,7,0,0,1,-2); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    // Fresh-install naming defaults must be disabled with every format unset; a naming
    // config that is *configured but disabled* must not perturb any basename assertion
    // below, since `rename_enabled` is the only automated-path gate that matters.
    let (code, tv_defaults) = request(&base, "GET", "/api/v1/tv/config/naming", Value::Null).await;
    assert_eq!(code, 200, "{tv_defaults}");
    assert_eq!(tv_defaults["rename_enabled"], false);
    assert_eq!(tv_defaults["revision"], 1);
    assert!(tv_defaults["standard_episode_format"].is_null());
    let (code, movie_defaults) =
        request(&base, "GET", "/api/v1/movies/config/naming", Value::Null).await;
    assert_eq!(code, 200, "{movie_defaults}");
    assert_eq!(movie_defaults["rename_enabled"], false);
    assert_eq!(movie_defaults["revision"], 1);
    assert!(movie_defaults["standard_movie_format"].is_null());
    let(code,v)=request(&base,"PUT","/api/v1/tv/config/naming",json!({"revision":1,"rename_enabled":false,"replace_illegal_characters":true,"colon_replacement":"smart","custom_colon_replacement":null,"standard_episode_format":"DISABLED-{Series Title}","daily_episode_format":null,"anime_episode_format":null,"series_folder_format":null,"season_folder_format":null,"specials_folder_format":null,"multi_episode_style":null})).await;
    assert_eq!(code, 200, "{v}");
    let(code,v)=request(&base,"PUT","/api/v1/movies/config/naming",json!({"revision":1,"rename_enabled":false,"replace_illegal_characters":true,"colon_replacement":"smart","custom_colon_replacement":null,"standard_movie_format":"DISABLED-{Movie Title}","movie_folder_format":null})).await;
    assert_eq!(code, 200, "{v}");
    let (indexer, download) = providers(&base, &origin).await;
    let mut receipts = Vec::new();
    let mut roots = Vec::new();
    let mut sources = Vec::new();
    for (media, profile) in [("tv", 1), ("movies", 2)] {
        let root = scratch.0.join(media);
        let source = scratch.0.join(format!("source-{media}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&source).unwrap();
        let media_source = if media == "tv" {
            let path = source.join("batch");
            std::fs::create_dir(&path).unwrap();
            path
        } else {
            source.clone()
        };
        let name = if media == "tv" {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        std::fs::write(
            media_source.join(name),
            vec![if media == "tv" { 1 } else { 2 }; 1048576],
        )
        .unwrap();
        let route = if media == "tv" {
            "/api/v1/tv/series/lookup"
        } else {
            "/api/v1/movies/lookup"
        };
        let input = if media == "tv" {
            json!({"tvdb_id":101,"path":root,"settings":{"quality_profile_id":profile,"series_type":"standard","use_scene_numbering":false,"monitored":true}})
        } else {
            json!({"tmdb_id":201,"path":root,"settings":{"quality_profile_id":profile,"minimum_availability":"released","monitored":true}})
        };
        let (code, v) = request(&base, "POST", route, input).await;
        assert_eq!(code, 201, "{v}");
        assert_eq!(v["id"], 1, "colliding native IDs must retain typed targets");
        let (code, v) = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/remote-path-mappings"),
            json!({"host":"127.0.0.1","remote_path":"/remote","local_path":source}),
        )
        .await;
        assert_eq!(code, 201, "{v}");
        let(code,v)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",download["id"].as_str().unwrap()),json!({"provider_revision":download["revision"],"revision":null,"enabled":true,"mode":if media=="tv"{"copy"}else{"hardlink"}})).await;
        assert_eq!(code, 200, "{v}");
        roots.push(root);
        sources.push(media_source);
    }
    let runtime = commands::start_with_metadata(db.clone(), client, metadata)
        .await
        .unwrap();
    for media in ["tv", "movies"] {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = wait_status(
            &base,
            &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
            "succeeded",
        )
        .await;
        assert_eq!(done["observed"], 1, "{done}");
        let (code, v) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        receipts.push(v["items"][0]["id"].clone());
    }
    state.completed.store(1, Ordering::SeqCst);
    let mut operations = Vec::new();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        state.detail_reads.store(0, Ordering::SeqCst);
        state.mode.store(11, Ordering::SeqCst);
        let(code,v)=request(&base,"POST","/api/v1/commands",json!({"name":"refresh_downloads","target":{"provider_id":download["id"],"media_type":media},"provider_revision":download["revision"],"priority":"normal"})).await;
        assert_eq!(code, 202, "{v}");
        wait_status(
            &base,
            &format!("/api/v1/commands/{}", v["id"].as_str().unwrap()),
            "succeeded",
        )
        .await;
        let path = format!(
            "/api/v1/download-processing/{}",
            receipts[i].as_str().unwrap()
        );
        let exhausted = wait_status(&base, &path, "blocked").await;
        assert_eq!(exhausted["error_code"], "download_unavailable");
        assert_eq!(exhausted["preflight_attempts"], 3);
        assert_eq!(exhausted["total_preflight_attempts"], 3);
        assert!(exhausted["operation_id"].is_null());
        assert_eq!(
            state.detail_reads.load(Ordering::SeqCst),
            3,
            "bounded automatic retries repeat only read-only details, never an import or submission"
        );
        state.mode.store(9, Ordering::SeqCst);
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipts[i]]})).await;
        assert_eq!(code, 202, "{v}");
        let wrong = wait_status(&base, &path, "blocked").await;
        assert_eq!(wrong["error_code"], "quality_rejected");
        assert!(
            wrong["operation_id"].is_null(),
            "wrong episode/year cannot create transfer intent"
        );
        state.mode.store(10, Ordering::SeqCst);
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipts[i]]})).await;
        assert_eq!(code, 202, "{v}");
        let ambiguous = wait_status(&base, &path, "blocked").await;
        assert_eq!(ambiguous["error_code"], "ambiguous_files");
        assert!(ambiguous["operation_id"].is_null());
        state.mode.store(0, Ordering::SeqCst);
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipts[i]]})).await;
        assert_eq!(code, 202, "{v}");
        let row = wait_status(&base, &path, "imported").await;
        assert_eq!(
            row["total_preflight_attempts"], 6,
            "three automatic failures plus three explicit rounds retain all six lifetime attempts"
        );
        assert_eq!(row["target"]["media_type"], *media);
        assert_eq!(row["preflight_attempts"], 1);
        assert_eq!(row["import_phase"], "complete");
        operations.push(row["operation_id"].clone());
        let name = if *media == "tv" {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        assert_eq!(
            std::fs::read(roots[i].join(name)).unwrap(),
            std::fs::read(sources[i].join(name)).unwrap(),
            "source bytes remain available for seeding"
        );
        if *media == "movies" {
            use std::os::unix::fs::MetadataExt;
            let source = std::fs::metadata(sources[i].join(name)).unwrap();
            let destination = std::fs::metadata(roots[i].join(name)).unwrap();
            assert_eq!(
                (source.dev(), source.ino()),
                (destination.dev(), destination.ino()),
                "hardlink mode shares the source inode"
            );
        }
        let(code,replay)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipts[i]]})).await;
        assert_eq!(code, 202, "{replay}");
        assert_eq!(replay[0]["operation_id"], operations[i]);
    }
    let (code, history) = request(&base, "GET", "/api/v1/history", Value::Null).await;
    assert_eq!(code, 200);
    assert_eq!(
        history["total"], 2,
        "one immutable import fact per typed receipt"
    );
    assert_eq!(state.adds.lock().unwrap().len(), 2);
    assert_eq!(
        c.query("SELECT count(*) FROM file_metadata WHERE quality_id=3", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        2
    );

    // A second real RSS grab upgrades the completed target; original torrent ownership remains retained.
    state.mode.store(6, Ordering::SeqCst);
    let mut upgrades = Vec::new();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let name = if *media == "tv" {
            "Harbor.S01E01.1080p.Bluray.mkv"
        } else {
            "Harbor.2020.1080p.Bluray.mkv"
        };
        std::fs::write(sources[i].join(name), vec![3; 1048576]).unwrap();
        let cmd = enqueue(&base, target(&indexer, &download, media)).await;
        let done = wait_status(
            &base,
            &format!("/api/v1/rss/commands/{}", cmd["id"].as_str().unwrap()),
            "succeeded",
        )
        .await;
        assert_eq!(
            done["observed"], 1,
            "completed import releases only typed target ownership, not prior hash claims: {done}"
        );
        let (_, v) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                cmd["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        let receipt = &v["items"][0]["id"];
        let association_sql = if *media == "tv" {
            "SELECT episode_file_id FROM episodes WHERE id=1"
        } else {
            "SELECT id FROM movie_files WHERE movie_id=1"
        };
        let old_id = c
            .query(association_sql, ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap();
        c.execute_batch("CREATE TRIGGER test_late_owned_failure BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'owned injected late commit failure'); END;").await.unwrap();
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{v}");
        let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
        let failed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, v) = request(&base, "GET", &path, Value::Null).await;
                if v["error_code"] == "import_failed" {
                    break v;
                }
                assert_ne!(v["status"], "blocked", "{v}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let original = if *media == "tv" {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        assert_eq!(
            std::fs::read(roots[i].join(original)).unwrap(),
            vec![if *media == "tv" { 1 } else { 2 }; 1048576],
            "failed replacement preserves original bytes"
        );
        assert_eq!(
            c.query(association_sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            old_id,
            "late database failure cannot change live association"
        );
        c.execute_batch("DROP TRIGGER test_late_owned_failure;")
            .await
            .unwrap();
        // Removing the injected fault cannot itself authorize another execution attempt.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (code, parked) = request(&base, "GET", &path, Value::Null).await;
        assert_eq!(code, 200);
        assert_eq!(parked["error_code"], "import_failed");
        assert_eq!(parked["operation_id"], failed["operation_id"]);
        assert_eq!(parked["import_phase"], "published");
        // Disabled automation cannot strand recovery of a linked, already-published local operation.
        let(code,v)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",download["id"].as_str().unwrap()),json!({"provider_revision":download["revision"],"revision":1,"enabled":false,"mode":if *media=="tv"{"copy"}else{"hardlink"}})).await;
        assert_eq!(code, 200, "{v}");
        upgrades.push((receipt.clone(), failed["operation_id"].clone(), old_id));
    }
    // Both real producer operations are unfinished at publication. This is a backend
    // shutdown/database reopen, not a browser reload or completed-operation replay.
    runtime.shutdown().await;
    _api.stop().await;
    drop(c);
    assert_eq!(
        Arc::strong_count(&db),
        1,
        "all runtime/API database owners released"
    );
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, _api) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::history::router(db.clone())),
    )
    .await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let c = db.connect().await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        state.adds.lock().unwrap().len(),
        4,
        "restart cannot resubmit an owned release"
    );
    let (_, history) = request(&base, "GET", "/api/v1/history", Value::Null).await;
    assert_eq!(
        history["total"], 2,
        "failed replacements have no committed history"
    );
    c.execute_batch("CREATE TRIGGER test_retirement_failure BEFORE UPDATE OF retirement_state ON rss_candidate_imports WHEN NEW.retirement_state='quarantined' BEGIN SELECT RAISE(ABORT,'retirement checkpoint unavailable'); END;").await.unwrap();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let (receipt, operation, old_id) = &upgrades[i];
        let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
        let (code, parked) = request(&base, "GET", &path, Value::Null).await;
        assert_eq!(code, 200);
        assert_eq!(parked["operation_id"], *operation);
        assert_eq!(
            parked["target"],
            if *media == "tv" {
                json!({"media_type":"tv","series_id":1,"episode_ids":[1]})
            } else {
                json!({"media_type":"movies","movie_id":1})
            }
        );
        assert_eq!(parked["import_phase"], "published");
        assert_eq!(parked["error_code"], "import_failed");
        assert_eq!(parked["resume_requested"], false);
        let association_sql = if *media == "tv" {
            "SELECT episode_file_id FROM episodes WHERE id=1"
        } else {
            "SELECT id FROM movie_files WHERE movie_id=1"
        };
        assert_eq!(
            c.query(association_sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            *old_id
        );
        let original = if *media == "tv" {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        assert_eq!(
            std::fs::read(roots[i].join(original)).unwrap(),
            vec![if *media == "tv" { 1 } else { 2 }; 1048576]
        );
        let name = if *media == "tv" {
            "Harbor.S01E01.1080p.Bluray.mkv"
        } else {
            "Harbor.2020.1080p.Bluray.mkv"
        };
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{v}");
        assert_eq!(v[0]["operation_id"], *operation);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, row) = request(&base, "GET", &path, Value::Null).await;
                if row["error_code"] == "import_failed" && row["import_phase"] == "committed" {
                    assert_eq!(row["operation_id"], *operation);
                    assert_eq!(row["retirement_state"], "pending");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("postcommit retirement failure");
        assert_eq!(
            std::fs::read(roots[i].join(name)).unwrap(),
            vec![3; 1048576]
        );
        let retained = roots[i]
            .join(format!(".hrrdarr-replaced-{}", operation.as_str().unwrap()))
            .join("original");
        assert_eq!(
            std::fs::read(retained).unwrap(),
            vec![if *media == "tv" { 1 } else { 2 }; 1048576]
        );
    }
    // Both associations/history are committed and originals renamed, but retirement
    // checkpoint persistence failed. Reopen before authorizing the same operations again.
    c.execute_batch("DROP TRIGGER test_retirement_failure;")
        .await
        .unwrap();
    runtime.shutdown().await;
    _api.stop().await;
    drop(c);
    assert_eq!(Arc::strong_count(&db), 1);
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, _api) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::history::router(db.clone())),
    )
    .await;
    let runtime = commands::start(db.clone(), client).await.unwrap();
    let c = db.connect().await.unwrap();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let (receipt, operation, old_id) = &upgrades[i];
        let name = if *media == "tv" {
            "Harbor.S01E01.1080p.Bluray.mkv"
        } else {
            "Harbor.2020.1080p.Bluray.mkv"
        };
        let association_sql = if *media == "tv" {
            "SELECT episode_file_id FROM episodes WHERE id=1"
        } else {
            "SELECT id FROM movie_files WHERE movie_id=1"
        };
        let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
        let (_, parked) = request(&base, "GET", &path, Value::Null).await;
        assert_eq!(parked["operation_id"], *operation);
        assert_eq!(parked["import_phase"], "committed");
        assert_eq!(parked["error_code"], "import_failed");
        let file_sql = if *media == "tv" {
            "SELECT f.path FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=1"
        } else {
            "SELECT path FROM movie_files WHERE movie_id=1"
        };
        assert_eq!(
            c.query(file_sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            roots[i].join(name).to_str().unwrap()
        );
        assert_eq!(
            c.query(
                "SELECT count(*) FROM import_history WHERE operation_id=?",
                [operation.as_str().unwrap()]
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            1,
            "commit already published one history fact before retirement recovery"
        );
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{v}");
        assert_eq!(v[0]["operation_id"], *operation);
        let imported = wait_status(&base, &path, "imported").await;
        assert_eq!(imported["operation_id"], *operation);
        assert_eq!(imported["retirement_state"], "quarantined");
        assert_eq!(imported["recovery_bytes_retained"], true);
        assert!(!imported["resume_requested"].as_bool().unwrap());
        assert_eq!(
            std::fs::read(roots[i].join(name)).unwrap(),
            vec![3; 1048576]
        );
        assert!(sources[i].join(name).exists());
        if *media == "movies" {
            use std::os::unix::fs::MetadataExt;
            let source = std::fs::metadata(sources[i].join(name)).unwrap();
            let destination = std::fs::metadata(roots[i].join(name)).unwrap();
            assert_eq!(
                (source.dev(), source.ino()),
                (destination.dev(), destination.ino())
            );
        }
        if *media == "movies" {
            assert_eq!(
                c.query(association_sql, ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                *old_id,
                "movie replacement retains its file identity"
            )
        }
        let retained = roots[i]
            .join(format!(
                ".hrrdarr-replaced-{}",
                imported["operation_id"].as_str().unwrap()
            ))
            .join("original");
        assert_eq!(
            std::fs::read(retained).unwrap(),
            vec![if *media == "tv" { 1 } else { 2 }; 1048576],
            "completed replacement retains exact original bytes in its owned recovery artifact"
        );
    }
    let (_, history) = request(&base, "GET", "/api/v1/history", Value::Null).await;
    assert_eq!(history["total"], 4);
    assert_eq!(state.adds.lock().unwrap().len(), 4);
    runtime.shutdown().await;
    _api.stop().await;
    drop(c);
    drop(db);
    let reopened = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(reopened.clone(), Some(key));
    let (base, _api) = serve(router.merge(commands::router(reopened.clone()))).await;
    let runtime = commands::start(reopened.clone(), client).await.unwrap();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipts[i]]})).await;
        assert_eq!(code, 202, "{v}");
        assert_eq!(v[0]["operation_id"], operations[i]);
        assert_eq!(v[0]["status"], "imported");
    }
    assert_eq!(
        state.adds.lock().unwrap().len(),
        4,
        "reopen and repeated receipt processing never resubmit"
    );
    let c = reopened.connect().await.unwrap();
    assert_eq!(
        c.query("SELECT count(*) FROM rss_hash_claims", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        4,
        "all original and upgrade remote identity claims remain"
    );
    assert_eq!(
        c.query("SELECT count(*) FROM import_history", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        4
    );
    for operation in operations
        .iter()
        .chain(upgrades.iter().map(|(_, operation, _)| operation))
    {
        let row = c
            .query(
                "SELECT count(*) FROM import_history WHERE operation_id=?",
                [operation.as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get::<i64>(0).unwrap(),
            1,
            "each original/upgrade operation has exactly one history fact"
        );
    }
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        for (receipt, hash) in [
            (
                &receipts[i],
                if i == 0 {
                    TV.to_owned()
                } else {
                    MOVIE.to_owned()
                },
            ),
            (
                &upgrades[i].0,
                if i == 0 {
                    "3".repeat(40)
                } else {
                    "4".repeat(40)
                },
            ),
        ] {
            let row = c.query("SELECT h.hash,r.media_type,h.client_id FROM rss_hash_claims h JOIN rss_candidates r ON r.id=h.candidate_id WHERE h.candidate_id=?", [receipt.as_str().unwrap()]).await.unwrap().next().await.unwrap().unwrap();
            assert_eq!(row.get::<String>(0).unwrap(), hash);
            assert_eq!(row.get::<String>(1).unwrap(), *media);
            assert_eq!(
                row.get::<String>(2).unwrap(),
                download["id"].as_str().unwrap()
            );
        }
    }
    runtime.shutdown().await;
    same_basename_http().await;
    naming_renders_tv_destination_http().await;
    naming_renders_movie_destination_http().await;
    naming_null_format_falls_back_to_basename_not_another_column_http().await;
    naming_rendered_collision_uses_same_path_replacement_http().await;
    naming_render_failure_blocks_and_touches_nothing_http().await;
    naming_resume_uses_captured_destination_not_reconfigured_one_http().await;
    naming_hostile_episode_title_renders_safely_http().await;
}

// Runs after the existing HTTP scenario in the same test so the global import lease is serial.
async fn same_basename_http() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-same-path-http-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (origin, _upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let (base, _api) = serve(
        router
            .merge(commands::router(db.clone()))
            .merge(hrrdarr::library::router(db.clone()))
            .merge(hrrdarr::library::metadata_router(
                db.clone(),
                metadata.clone(),
            ))
            .merge(hrrdarr::remote_paths::router(db.clone()))
            .merge(hrrdarr::import::router(db.clone()))
            .merge(hrrdarr::media_files::router(db.clone()))
            .merge(hrrdarr::history::router(db.clone())),
    )
    .await;
    let c = db.connect().await.unwrap();
    c.execute_batch("UPDATE quality_definitions SET min_size=0; INSERT INTO quality_profiles VALUES(1,'tv','HD'),(2,'movies','HD'); INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'tv',1,0,1),(1,'tv',3,1,1),(2,'movies',1,0,1),(2,'movies',3,1,1); INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'tv',1,3,0,0,1,NULL),(2,'movies',1,3,0,0,1,-2); INSERT INTO release_delay_policies VALUES('tv',0,0,0),('movies',0,0,0);").await.unwrap();
    let (indexer, download) = providers(&base, &origin).await;
    let mut paths = Vec::new();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let root = scratch.0.join(media);
        let source = scratch.0.join(format!("source-{media}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(source.join("batch")).unwrap();
        let name = if i == 0 {
            "Harbor.S01E01.1080p.WEB-DL.mkv"
        } else {
            "Harbor.2020.1080p.WEB-DL.mkv"
        };
        let new = if i == 0 {
            source.join("batch").join(name)
        } else {
            source.join(name)
        };
        std::fs::write(&new, vec![9u8; 1048576]).unwrap();
        let old_source = scratch.0.join(format!("original-{media}"));
        std::fs::write(&old_source, b"original-same-path-media").unwrap();
        let route = if i == 0 {
            "/api/v1/tv/series/lookup"
        } else {
            "/api/v1/movies/lookup"
        };
        let add = if i == 0 {
            json!({"tvdb_id":101,"path":root,"settings":{"quality_profile_id":1,"series_type":"standard","use_scene_numbering":false,"monitored":true}})
        } else {
            json!({"tmdb_id":201,"path":root,"settings":{"quality_profile_id":2,"minimum_availability":"released","monitored":true}})
        };
        let (code, v) = request(&base, "POST", route, add).await;
        assert_eq!(code, 201, "{v}");
        let (code,op)=request(&base,"POST","/api/v1/imports",json!({"target":{"media_type":if i==0{"episode"}else{"movie"},"id":1},"source":old_source,"destination":root.join(name),"mode":"copy"})).await;
        assert_eq!(code, 202, "{op}");
        let (code, v) = request(
            &base,
            "POST",
            &format!("/api/v1/imports/{}/execute", op["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
        assert_eq!(code, 200, "{v}");
        // The existing filename is not authoritative quality metadata: a native user correction
        // makes this an eligible upgrade without unsupported proper/repack semantics.
        let (code, v) = request(
            &base,
            "PUT",
            &format!("/api/v1/{media}/files/1"),
            json!({"quality":{"quality_id":1,"revision":{"version":1,"real":0,"is_repack":false}}}),
        )
        .await;
        assert_eq!(code, 200, "{v}");
        let (code, v) = request(
            &base,
            "POST",
            &format!("/api/v1/{media}/remote-path-mappings"),
            json!({"host":"127.0.0.1","remote_path":"/remote","local_path":source}),
        )
        .await;
        assert_eq!(code, 201, "{v}");
        let(code,v)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",download["id"].as_str().unwrap()),json!({"provider_revision":download["revision"],"revision":null,"enabled":true,"mode":if i==0{"copy"}else{"hardlink"}})).await;
        assert_eq!(code, 200, "{v}");
        paths.push((root.join(name), new));
    }
    let runtime = commands::start_with_metadata(db.clone(), client, metadata)
        .await
        .unwrap();
    for (i, media) in ["tv", "movies"].iter().enumerate() {
        let command = enqueue(&base, target(&indexer, &download, media)).await;
        let done = wait_status(
            &base,
            &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
            "succeeded",
        )
        .await;
        assert_eq!(done["observed"], 1, "{done}");
        let (code, v) = request(
            &base,
            "GET",
            &format!(
                "/api/v1/rss/candidates?command_id={}",
                command["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        let receipt = v["items"][0]["id"].clone();
        state.completed.store(1, Ordering::SeqCst);
        c.execute_batch("CREATE TRIGGER same_path_http_failure BEFORE INSERT ON import_history BEGIN SELECT RAISE(ABORT,'same path failure'); END;").await.unwrap();
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{v}");
        let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
        let failed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, v) = request(&base, "GET", &path, Value::Null).await;
                if v["error_code"] == "import_failed" {
                    break v;
                }
                assert_ne!(v["status"], "blocked", "{v}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&paths[i].0).unwrap(),
            b"original-same-path-media"
        );
        let association = if i == 0 {
            "SELECT episode_file_id FROM episodes WHERE id=1"
        } else {
            "SELECT id FROM movie_files WHERE movie_id=1"
        };
        assert_eq!(
            c.query(association, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            1,
            "failed same-path commit retains original association"
        );
        let file_column = if i == 0 {
            "episode_file_id"
        } else {
            "movie_file_id"
        };
        assert_eq!(
            c.query(
                &format!("SELECT quality_id FROM file_metadata WHERE {file_column}=1"),
                ()
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
            1,
            "failed commit retains corrected original quality"
        );
        c.execute_batch("DROP TRIGGER same_path_http_failure;")
            .await
            .unwrap();
        let(code,v)=request(&base,"POST","/api/v1/download-processing",json!({"provider_id":download["id"],"provider_revision":download["revision"],"media_type":media,"receipt_ids":[receipt]})).await;
        assert_eq!(code, 202, "{v}");
        let done = wait_status(&base, &path, "imported").await;
        assert_eq!(done["operation_id"], failed["operation_id"]);
        let old_json: String = c
            .query(
                "SELECT old_file_json FROM rss_candidate_imports WHERE operation_id=?",
                [done["operation_id"].as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let old: Value = serde_json::from_str(&old_json).unwrap();
        let retained = paths[i]
            .0
            .parent()
            .unwrap()
            .join(old["quarantine_name"].as_str().unwrap())
            .join("original");
        assert_eq!(
            std::fs::read(retained).unwrap(),
            b"original-same-path-media"
        );
        assert_eq!(std::fs::read(&paths[i].0).unwrap(), vec![9u8; 1048576]);
        assert_eq!(std::fs::read(&paths[i].1).unwrap(), vec![9u8; 1048576]);
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(&paths[i].0).unwrap();
        let b = std::fs::metadata(&paths[i].1).unwrap();
        assert_eq!((a.dev(), a.ino()) == (b.dev(), b.ino()), i == 1);
    }
    let (code, v) = request(&base, "GET", "/api/v1/history", Value::Null).await;
    assert_eq!(code, 200);
    assert_eq!(
        v["total"], 4,
        "two native originals plus two same-path upgrades"
    );
    assert_eq!(state.adds.lock().unwrap().len(), 2);
    runtime.shutdown().await;
}

// Shared fixture for the naming-wiring scenarios below: one series/movie, one root, one
// completed WEB-DL release, real HTTP add/RSS/grab through the same producer pipeline as
// the rest of this file. Each scenario gets its own scratch/db so the global owned-import
// lease (`src/import/mod.rs`'s `EXECUTING`/`ACTIVE_OPERATION` statics) never contends across
// concurrently-running test binaries; every caller runs serially from the bottom of the main
// `#[tokio::test]` above for the same reason `same_basename_http` already does.
struct Ctx {
    scratch: Scratch,
    db: Arc<Database>,
    state: Arc<Remote>,
    base: String,
    root: PathBuf,
    source: PathBuf,
    runtime: hrrdarr::commands::Runtime,
    api: Server,
    _upstream: Server,
    download: Value,
    indexer: Value,
    movie: bool,
}
impl Ctx {
    async fn shutdown(self) {
        self.runtime.shutdown().await;
        self.api.stop().await;
    }
}
async fn naming_ctx(movie: bool, tv_series_type: &str) -> Ctx {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("hrrdarr-naming-wiring-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&scratch.0).unwrap();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let state = Arc::new(Remote::default());
    let (origin, upstream) = serve(
        axum::Router::new()
            .fallback(remote)
            .with_state(state.clone()),
    )
    .await;
    *state.endpoint.lock().unwrap() = origin.clone();
    let metadata = Arc::new(
        hrrdarr::metadata::MetadataClient::with_origins(
            &format!("{origin}/"),
            &format!("{origin}/"),
        )
        .unwrap(),
    );
    let key = Arc::new(providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap());
    let (router, client) = providers::router_with_refresh(db.clone(), Some(key));
    let router = router
        .merge(commands::router(db.clone()))
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::library::metadata_router(
            db.clone(),
            metadata.clone(),
        ))
        .merge(hrrdarr::remote_paths::router(db.clone()))
        .merge(hrrdarr::naming::router(db.clone()))
        .merge(hrrdarr::import::router(db.clone()))
        .merge(hrrdarr::media_files::router(db.clone()))
        .merge(hrrdarr::history::router(db.clone()));
    let (base, api) = serve(router).await;
    let c = db.connect().await.unwrap();
    let media = if movie { "movies" } else { "tv" };
    let lang = if movie { "-2" } else { "NULL" };
    c.execute_batch(&format!(
        "UPDATE quality_definitions SET min_size=0; \
         INSERT INTO quality_profiles VALUES(1,'{media}','HD'); \
         INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed)VALUES(1,'{media}',1,0,1),(1,'{media}',3,1,1); \
         INSERT INTO quality_profile_policies(profile_id,media_type,upgrade_allowed,cutoff_quality_id,min_format_score,cutoff_format_score,min_upgrade_format_score,language_id)VALUES(1,'{media}',1,3,0,0,1,{lang}); \
         INSERT INTO release_delay_policies VALUES('{media}',0,0,0);"
    ))
    .await
    .unwrap();
    let (indexer, download) = providers(&base, &origin).await;
    let root = scratch.0.join(media);
    std::fs::create_dir(&root).unwrap();
    let source_top = scratch.0.join(format!("source-{media}"));
    let source = if movie {
        std::fs::create_dir_all(&source_top).unwrap();
        source_top.clone()
    } else {
        std::fs::create_dir_all(source_top.join("batch")).unwrap();
        source_top.join("batch")
    };
    let name = if movie {
        "Harbor.2020.1080p.WEB-DL.mkv"
    } else {
        "Harbor.S01E01.1080p.WEB-DL.mkv"
    };
    std::fs::write(source.join(name), vec![1u8; 1048576]).unwrap();
    let route = if movie {
        "/api/v1/movies/lookup"
    } else {
        "/api/v1/tv/series/lookup"
    };
    let input = if movie {
        json!({"tmdb_id":201,"path":root,"settings":{"quality_profile_id":1,"minimum_availability":"released","monitored":true}})
    } else {
        json!({"tvdb_id":101,"path":root,"settings":{"quality_profile_id":1,"series_type":tv_series_type,"use_scene_numbering":false,"monitored":true}})
    };
    let (code, v) = request(&base, "POST", route, input).await;
    assert_eq!(code, 201, "{v}");
    let (code, v) = request(
        &base,
        "POST",
        &format!("/api/v1/{media}/remote-path-mappings"),
        json!({"host":"127.0.0.1","remote_path":"/remote","local_path":source_top}),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let(code,v)=request(&base,"PUT",&format!("/api/v1/download-processing/policies/{}/{media}",download["id"].as_str().unwrap()),json!({"provider_revision":download["revision"],"revision":null,"enabled":true,"mode":if movie{"hardlink"}else{"copy"}})).await;
    assert_eq!(code, 200, "{v}");
    let runtime = commands::start_with_metadata(db.clone(), client, metadata)
        .await
        .unwrap();
    Ctx {
        scratch,
        db,
        state,
        base,
        root,
        source,
        runtime,
        api,
        _upstream: upstream,
        download,
        indexer,
        movie,
    }
}
async fn naming_grab_and_complete(ctx: &Ctx) -> Value {
    let media = if ctx.movie { "movies" } else { "tv" };
    let command = enqueue(&ctx.base, target(&ctx.indexer, &ctx.download, media)).await;
    let done = wait_status(
        &ctx.base,
        &format!("/api/v1/rss/commands/{}", command["id"].as_str().unwrap()),
        "succeeded",
    )
    .await;
    assert_eq!(done["observed"], 1, "{done}");
    let (code, v) = request(
        &ctx.base,
        "GET",
        &format!(
            "/api/v1/rss/candidates?command_id={}",
            command["id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    let receipt = v["items"][0]["id"].clone();
    ctx.state.completed.store(1, Ordering::SeqCst);
    receipt
}
async fn naming_process(ctx: &Ctx, receipt: &Value) -> (u16, Value) {
    let media = if ctx.movie { "movies" } else { "tv" };
    request(
        &ctx.base,
        "POST",
        "/api/v1/download-processing",
        json!({"provider_id":ctx.download["id"],"provider_revision":ctx.download["revision"],"media_type":media,"receipt_ids":[receipt]}),
    )
    .await
}
async fn put_tv_naming(
    base: &str,
    revision: i64,
    rename_enabled: bool,
    standard: Option<&str>,
    daily: Option<&str>,
    anime: Option<&str>,
) -> Value {
    let (code, v) = request(
        base,
        "PUT",
        "/api/v1/tv/config/naming",
        json!({
            "revision": revision, "rename_enabled": rename_enabled, "replace_illegal_characters": true,
            "colon_replacement": "dash", "custom_colon_replacement": null,
            "standard_episode_format": standard, "daily_episode_format": daily, "anime_episode_format": anime,
            "series_folder_format": null, "season_folder_format": null, "specials_folder_format": null,
            "multi_episode_style": null,
        }),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    v
}
async fn put_movie_naming(
    base: &str,
    revision: i64,
    rename_enabled: bool,
    standard: Option<&str>,
) -> Value {
    let (code, v) = request(
        base,
        "PUT",
        "/api/v1/movies/config/naming",
        json!({
            "revision": revision, "rename_enabled": rename_enabled, "replace_illegal_characters": true,
            "colon_replacement": "dash", "custom_colon_replacement": null,
            "standard_movie_format": standard, "movie_folder_format": null,
        }),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    v
}

// Scenario: `standard_episode_format` configured and enabled actually changes the on-disk
// filename for TV, through the real Add->RSS->grab->completed-download pipeline.
async fn naming_renders_tv_destination_http() {
    let ctx = naming_ctx(false, "standard").await;
    put_tv_naming(
        &ctx.base,
        1,
        true,
        Some("{Series Title} - S{season:00}E{episode:00} - {Episode Title} [{Quality Title}]"),
        Some("DAILY-{Series Title}-S{season:00}E{episode:00}"),
        Some("ANIME-{Series Title}-{episode:00}"),
    )
    .await;
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["import_phase"], "complete");
    let expected = "Harbor - S01E01 - Pilot [WEBDL-1080p].mkv";
    assert_eq!(
        std::fs::read(ctx.root.join(expected)).unwrap(),
        vec![1u8; 1048576]
    );
    assert!(
        !ctx.root.join("Harbor.S01E01.1080p.WEB-DL.mkv").exists(),
        "raw downloaded basename must not remain once rendering is enabled"
    );
    ctx.shutdown().await;
}

// Scenario: `standard_movie_format` configured and enabled actually changes the on-disk
// filename for movies, through the real pipeline.
async fn naming_renders_movie_destination_http() {
    let ctx = naming_ctx(true, "standard").await;
    put_movie_naming(
        &ctx.base,
        1,
        true,
        Some("{Movie Title} ({Release Year}) [{Quality Title}]"),
    )
    .await;
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["import_phase"], "complete");
    let expected = "Harbor (2020) [WEBDL-1080p].mkv";
    assert_eq!(
        std::fs::read(ctx.root.join(expected)).unwrap(),
        vec![1u8; 1048576]
    );
    assert!(!ctx.root.join("Harbor.2020.1080p.WEB-DL.mkv").exists());
    ctx.shutdown().await;
}

// Scenario 3 (series-type selection) is only partially reachable end to end: NOTE below and
// in the final report explains why the `daily`/`anime` arms of `resolve_owned_destination`
// cannot be exercised through the real completed-download pipeline as it exists today.
//
// `src/search/downloaded.rs::evaluate` (the decision the automated owned-download path uses to
// accept a completed file) rejects ANY target whose `library_settings.series_type` is not
// `"standard"` with `numbering_unsupported`, before naming is ever consulted, and its numbering
// match arm only implements `parser::Numbering::Episodes` (`_ => false` for `Daily`/`Absolute`).
// So a `daily`- or `anime`-typed series can never have a completed download accepted by this
// pipeline at all today, independent of naming configuration -- there is no way to reach the
// `Some("daily")`/`Some("anime")` arms in `src/naming/destination.rs`'s series-type match from
// a real HTTP flow. `naming_renders_tv_destination_http` above already proves the `"standard"`
// arm picks `standard_episode_format` over the (also configured) daily/anime templates; the
// scenario below proves the reverse half of the same selection (a `NULL` `standard_episode_format`
// does not fall back to `standard_episode_format`'s own value, obviously, but more importantly
// does not fall back to ANY other configured format either). Together these are the full
// selection-logic coverage obtainable without a change to `search/downloaded.rs`.

// Scenario: `rename_enabled=true` but `standard_episode_format` itself is `NULL`, while
// `daily_episode_format`/`anime_episode_format` are both configured and would render
// visibly-different names, falls back to basename preservation rather than sliding to
// either of the other configured format columns.
async fn naming_null_format_falls_back_to_basename_not_another_column_http() {
    let ctx = naming_ctx(false, "standard").await;
    put_tv_naming(
        &ctx.base,
        1,
        true,
        None,
        Some("DAILY-{Series Title}-S{season:00}E{episode:00}"),
        Some("ANIME-{Series Title}-{episode:00}"),
    )
    .await;
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["import_phase"], "complete");
    let raw = "Harbor.S01E01.1080p.WEB-DL.mkv";
    assert_eq!(
        std::fs::read(ctx.root.join(raw)).unwrap(),
        vec![1u8; 1048576]
    );
    assert!(
        !ctx.root.join("DAILY-Harbor-S01E01.mkv").exists(),
        "a NULL standard_episode_format must not fall back to daily_episode_format"
    );
    assert!(
        !ctx.root.join("ANIME-Harbor-01.mkv").exists(),
        "a NULL standard_episode_format must not fall back to anime_episode_format"
    );
    ctx.shutdown().await;
}

// Scenario: a configured template that happens to render to the SAME filename as a
// pre-existing associated file must still go through the same-path replacement/exchange
// machinery from the prior iteration, not be treated as a fresh distinct destination.
async fn naming_rendered_collision_uses_same_path_replacement_http() {
    let ctx = naming_ctx(false, "standard").await;
    let existing_name = "Collide S01E01.mkv";
    let old_source = ctx.scratch.0.join("original-tv");
    std::fs::write(&old_source, b"original-collision-media").unwrap();
    let (code, op) = request(
        &ctx.base,
        "POST",
        "/api/v1/imports",
        json!({"target":{"media_type":"episode","id":1},"source":old_source,"destination":ctx.root.join(existing_name),"mode":"copy"}),
    )
    .await;
    assert_eq!(code, 202, "{op}");
    let (code, v) = request(
        &ctx.base,
        "POST",
        &format!("/api/v1/imports/{}/execute", op["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    let (code, v) = request(
        &ctx.base,
        "PUT",
        "/api/v1/tv/files/1",
        json!({"quality":{"quality_id":1,"revision":{"version":1,"real":0,"is_repack":false}}}),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    put_tv_naming(
        &ctx.base,
        1,
        true,
        Some("Collide S{season:00}E{episode:00}"),
        None,
        None,
    )
    .await;
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["import_phase"], "complete");
    assert_eq!(done["retirement_state"], "quarantined");
    assert_eq!(
        std::fs::read(ctx.root.join(existing_name)).unwrap(),
        vec![1u8; 1048576],
        "the rendered destination must equal the pre-existing path, not a fresh one"
    );
    assert!(
        !ctx.root.join("Harbor.S01E01.1080p.WEB-DL.mkv").exists(),
        "same-path replacement must not also leave the raw downloaded basename behind"
    );
    let c = ctx.db.connect().await.unwrap();
    let old_json: String = c
        .query(
            "SELECT old_file_json FROM rss_candidate_imports WHERE operation_id=?",
            [done["operation_id"].as_str().unwrap()],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    let old: Value = serde_json::from_str(&old_json).unwrap();
    let retained = ctx
        .root
        .join(old["quarantine_name"].as_str().unwrap())
        .join("original");
    assert_eq!(
        std::fs::read(retained).unwrap(),
        b"original-collision-media"
    );
    ctx.shutdown().await;
}

// Scenario: a template that fails to render against real stored facts blocks the item and
// touches nothing -- no destination file, no operation, no journal.
//
// BUG FOUND (reported, not fixed here -- out of file-ownership scope): the item does NOT end
// up blocked with `naming_render_failed` as documented. `download_processing.reasons_json` has
// a trigger (migration 0026_download_processing.sql, both `CHECK`-style triggers around lines
// 103/114) that `RAISE(ABORT,'invalid processing reason')` whenever any reason string is not
// `[a-z0-9_]*` of at most 128 bytes. `src/commands/processing.rs::preflight()`'s naming-render-
// failure branch calls `blocked(&c, item.receipt_id, "naming_render_failed", vec![detail])`
// where `detail` is `finish_render`'s free-text message (e.g. "standard_episode_format failed
// to render: rendering produced an empty path component") -- which always contains spaces and
// punctuation and therefore always violates that trigger. The `blocked()` write itself fails
// with a real SQLite constraint error (verified via temporary `{error:?}` instrumentation on
// `commands::Error::from(libsql::Error)`, since reverted), is treated as a retryable
// `storage_error` for up to 3 attempts (identical failure each time, since the render input
// never changes), and only succeeds at blocking on the 3rd attempt using the generic
// `storage_error`/`["storage_error"]` pair instead. Every render failure hits this, since
// `finish_render`'s messages are always human-readable sentences, never bare tokens. This test
// asserts the ACTUAL observed behavior (including the bug) plus the safety invariants that DO
// still hold (no operation/journal ever created, no file ever touched, in any of the 3 attempts).
async fn naming_render_failure_blocks_and_touches_nothing_http() {
    let ctx = naming_ctx(false, "standard").await;
    put_tv_naming(&ctx.base, 1, true, Some("{Episode Title}"), None, None).await;
    let c = ctx.db.connect().await.unwrap();
    // `episodes.title` is `NOT NULL` with no emptiness check; an empty title is the only
    // schema-legal way to make `{Episode Title}` alone render to an empty path component.
    c.execute("UPDATE episodes SET title='' WHERE id=1", ())
        .await
        .unwrap();
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let blocked = wait_status(&ctx.base, &path, "blocked").await;
    // `error_code` (migration 0026) is a closed enum predating this feature with no
    // naming-specific member; `reasons_json` only accepts `[a-z0-9_]*` tokens <=128 bytes,
    // so the free-text render-failure detail is logged, not stored. `unsupported_download`
    // (otherwise unused) plus a `naming_render_failed` reason token is the fix for the
    // previously-discovered bug where the raw sentence violated the reasons_json trigger and
    // masked the real outcome as `storage_error` after 3 failed retries.
    assert_eq!(blocked["error_code"], "unsupported_download", "{blocked}");
    assert_eq!(
        blocked["reasons"].as_array().unwrap(),
        &vec![serde_json::json!("naming_render_failed")]
    );
    assert_eq!(blocked["total_preflight_attempts"], 1, "{blocked}");
    assert!(blocked["operation_id"].is_null());
    assert!(
        ctx.source.join("Harbor.S01E01.1080p.WEB-DL.mkv").exists(),
        "source file must remain untouched"
    );
    assert_eq!(
        std::fs::read_dir(&ctx.root).unwrap().count(),
        0,
        "no destination file may be created on a render failure"
    );
    for table in ["operations", "import_journal", "rss_candidate_imports"] {
        let count: i64 = c
            .query(&format!("SELECT count(*) FROM {table}"), ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    ctx.shutdown().await;
}

// Scenario: an operation whose `Plan` was already captured/journaled under naming config A
// must resume to completion using A's destination verbatim, never re-rendering against a
// naming config B applied while the operation was paused mid-flight.
async fn naming_resume_uses_captured_destination_not_reconfigured_one_http() {
    let ctx = naming_ctx(false, "standard").await;
    put_tv_naming(
        &ctx.base,
        1,
        true,
        Some("CONFIG-A-{Series Title}-S{season:00}E{episode:00}"),
        None,
        None,
    )
    .await;
    let c = ctx.db.connect().await.unwrap();
    // Installed before the grab/complete step (not after) so there is no window in which a
    // scheduled refresh could auto-queue and race the completed item past `staging`
    // unpaused. Pauses right after the Plan is journaled (phase='preview') and before any
    // file is staged/published, so a re-render (if it happened) would be directly observable.
    c.execute_batch("CREATE TRIGGER naming_resume_pause BEFORE UPDATE OF phase ON import_journal WHEN NEW.phase='staging' BEGIN SELECT RAISE(ABORT,'test pause before staging'); END;").await.unwrap();
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let paused = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let (_, v) = request(&ctx.base, "GET", &path, Value::Null).await;
            if v["error_code"] == "import_failed" {
                break v;
            }
            assert_ne!(v["status"], "blocked", "{v}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        paused["import_phase"], "preview",
        "must pause before any file materialization"
    );
    assert!(!paused["operation_id"].is_null());
    let config_a = "CONFIG-A-Harbor-S01E01.mkv";
    assert!(
        !ctx.root.join(config_a).exists(),
        "nothing may be written before staging even begins"
    );
    let plan_json: String = c
        .query(
            "SELECT plan_json FROM import_journal WHERE operation_id=?",
            [paused["operation_id"].as_str().unwrap()],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    let plan: Value = serde_json::from_str(&plan_json).unwrap();
    assert_eq!(
        plan["destination"],
        ctx.root.join(config_a).to_str().unwrap()
    );
    put_tv_naming(
        &ctx.base,
        2,
        true,
        Some("CONFIG-B-{Series Title}-S{season:00}E{episode:00}"),
        None,
        None,
    )
    .await;
    c.execute_batch("DROP TRIGGER naming_resume_pause;")
        .await
        .unwrap();
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["operation_id"], paused["operation_id"]);
    assert_eq!(
        std::fs::read(ctx.root.join(config_a)).unwrap(),
        vec![1u8; 1048576]
    );
    assert!(
        !ctx.root.join("CONFIG-B-Harbor-S01E01.mkv").exists(),
        "resume must not re-render with the newly-applied config"
    );
    let association: String = c
        .query(
            "SELECT f.path FROM episodes e JOIN episode_files f ON f.id=e.episode_file_id WHERE e.id=1",
            (),
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(association, ctx.root.join(config_a).to_str().unwrap());
    ctx.shutdown().await;
}

// Scenario: a hostile episode title (a literal `/`, a literal `:`, and 250+ bytes combined
// into one fact) renders through the real pipeline into a single, sanitized, <=255-byte path
// component -- proving the wiring carries a hostile real stored fact through safely, on top of
// `src/naming/render.rs`'s own unit tests for the pure sanitization rules in isolation.
async fn naming_hostile_episode_title_renders_safely_http() {
    let ctx = naming_ctx(false, "standard").await;
    put_tv_naming(&ctx.base, 1, true, Some("{Episode Title}"), None, None).await;
    let c = ctx.db.connect().await.unwrap();
    let hostile = format!("Face/Off: {}", "x".repeat(300));
    c.execute("UPDATE episodes SET title=? WHERE id=1", [hostile])
        .await
        .unwrap();
    let receipt = naming_grab_and_complete(&ctx).await;
    let (code, v) = naming_process(&ctx, &receipt).await;
    assert_eq!(code, 202, "{v}");
    let path = format!("/api/v1/download-processing/{}", receipt.as_str().unwrap());
    let done = wait_status(&ctx.base, &path, "imported").await;
    assert_eq!(done["import_phase"], "complete");
    // `fs.rs::cleanup` intentionally retains the emptied `.hrrdarr-import-<opid>` staging
    // directory as a durable ownership receipt after every completed import; filter it out
    // rather than asserting an exact directory entry count.
    let entries: Vec<_> = std::fs::read_dir(&ctx.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| !n.starts_with(".hrrdarr-"))
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let name = &entries[0];
    assert!(
        name.starts_with("Face-Off- "),
        "slash and colon must sanitize within one component: {name}"
    );
    assert!(name.ends_with(".mkv"), "{name}");
    assert_eq!(name.len(), 255, "{name}");
    assert!(!name.contains('/'), "{name}");
    assert!(!name.contains(':'), "{name}");
    ctx.shutdown().await;
}
