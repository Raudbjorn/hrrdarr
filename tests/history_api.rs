use hrrdarr::{db::Database, history, import};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
// The real importer has one process-wide execution permit; these are sequential workflow fixtures.
static IMPORT_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-history-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn serve(db: Arc<Database>) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = history::router(db.clone()).merge(import::router(db));
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("{status}: {body}")),
    )
}
async fn page(base: &str, query: &str) -> Value {
    let (code, body) = request(base, "GET", &format!("/api/v1/history{query}"), Value::Null).await;
    assert_eq!(code, 200, "{body}");
    body
}
fn dates(from: Option<&str>, to: Option<&str>) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    if let Some(v) = from {
        q.append_pair("from", v);
    }
    if let Some(v) = to {
        q.append_pair("to", v);
    }
    format!("?{}", q.finish())
}

#[tokio::test]
async fn committed_import_history_is_typed_filtered_bounded_and_retained() {
    let _fixture = IMPORT_TESTS.lock().await;
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let tv = scratch.0.join("tv");
    let movies = scratch.0.join("movies");
    let incoming = scratch.0.join("incoming");
    for path in [&tv, &movies, &incoming] {
        std::fs::create_dir(path).unwrap();
    }
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'Series',?)",
        [tv.to_str().unwrap()],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO seasons VALUES(1,0,1),(1,1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'One'),(2,1,0,1,'Special'),(3,1,1,2,'Failed');").await.unwrap();
    for id in 1..=2 {
        let root = movies.join(id.to_string());
        std::fs::create_dir(&root).unwrap();
        c.execute(
            "INSERT INTO movie_metadata(id,title) VALUES(?,'Movie')",
            [id],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO movies(id,metadata_id,path) VALUES(?,?,?)",
            libsql::params![id, id, root.to_str().unwrap()],
        )
        .await
        .unwrap();
    }
    let (base, server) = serve(db.clone()).await;
    assert_eq!(page(&base, "").await["total"], 0);
    let mut facts = Vec::new();
    for media in ["episode", "movie"] {
        for id in 1..=2 {
            let source = incoming.join(format!("{media}{id}.mkv"));
            let destination = if media == "episode" {
                tv.join(format!("episode{id}.mkv"))
            } else {
                movies.join(id.to_string()).join("movie.mkv")
            };
            let bytes = format!("owned history bytes {media} {id}");
            std::fs::write(&source, &bytes).unwrap();
            let(code,preview)=request(&base,"POST","/api/v1/imports",json!({"target":{"media_type":media,"id":id},"source":source,"destination":destination,"mode":"copy"})).await;
            assert_eq!(code, 202, "{preview}");
            assert_eq!(
                page(&base, "").await["total"],
                facts.len(),
                "preview is not an import event"
            );
            let (code, done) = request(
                &base,
                "POST",
                &format!(
                    "/api/v1/imports/{}/execute",
                    preview["id"].as_str().unwrap()
                ),
                Value::Null,
            )
            .await;
            assert_eq!(code, 200, "{done}");
            assert_eq!(done["status"], "complete");
            facts.push((
                preview["id"].clone(),
                json!({"media_type":media,"id":id}),
                source,
                destination,
                bytes.len(),
            ));
        }
    }
    let all = page(&base, "").await;
    assert_eq!(all["total"], 4);
    assert_eq!(all["limit"], 50);
    assert_eq!(all["offset"], 0);
    let items = all["items"].as_array().unwrap();
    let actual_order = items
        .iter()
        .map(|v| {
            (
                v["imported_at"].as_str().unwrap(),
                v["id"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut ordered = actual_order.clone();
    ordered.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(actual_order, ordered);
    for (operation, target, source, destination, size) in &facts {
        let event = items.iter().find(|v| v["id"] == *operation).unwrap();
        assert_eq!(event["origin"], "native_import");
        assert_eq!(event["event_type"], "file_imported");
        assert_eq!(event["target"], *target);
        assert_eq!(event["source"], source.to_str().unwrap());
        assert_eq!(event["destination"], destination.to_str().unwrap());
        assert_eq!(event["size_bytes"], *size);
        // Compare immutable producer facts, not merely DTO formatting: wrong hashes or
        // a cross-domain file ID projection must fail even when their shapes look valid.
        let bytes = std::fs::read(source).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let expected_hash = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(event["sha256"], expected_hash);
        let file_row = c
            .query(
                "SELECT episode_file_id,movie_file_id FROM import_history WHERE operation_id=?",
                [operation.as_str().unwrap()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        let file_id = if target["media_type"] == "episode" {
            file_row.get::<i64>(0).unwrap()
        } else {
            file_row.get::<i64>(1).unwrap()
        };
        assert_eq!(event["file"]["id"], file_id);
        if target["id"] == 1 {
            assert_eq!(
                file_id, 1,
                "first file IDs deliberately collide across both domains"
            );
        }
        assert!(event.get("quality").is_none());
        assert!(event.get("download_id").is_none());
        assert!(event.get("cleanup_complete").is_none());
        assert_eq!(
            event["file"]["media_type"],
            if target["media_type"] == "episode" {
                "tv"
            } else {
                "movies"
            }
        );
    }
    for (query, total) in [
        ("?media_type=tv", 2),
        ("?media_type=movies", 2),
        ("?episode_id=1", 1),
        ("?movie_id=1", 1),
        ("?series_id=1", 2),
        ("?series_id=1&season=0", 1),
        ("?series_id=1&season=1&episode_id=2", 0),
        ("?movie_id=999", 0),
        ("?series_id=999", 0),
    ] {
        assert_eq!(page(&base, query).await["total"], total, "{query}");
    }
    assert_eq!(
        page(&base, "?episode_id=1").await["items"][0]["target"],
        json!({"media_type":"episode","id":1})
    );
    assert_eq!(
        page(&base, "?movie_id=1").await["items"][0]["target"],
        json!({"media_type":"movie","id":1})
    );
    for offset in 0..4 {
        let p = page(&base, &format!("?limit=1&offset={offset}")).await;
        assert_eq!(p["total"], 4);
        assert_eq!(p["items"][0], items[offset]);
    }
    assert_eq!(page(&base, "?offset=10000").await["items"], json!([]));
    // Boundaries come from actual immutable producer timestamps, not fixture rewrites.
    let instant =
        chrono::DateTime::parse_from_rfc3339(items[1]["imported_at"].as_str().unwrap()).unwrap();
    let at = instant.to_rfc3339();
    let fraction = (instant + chrono::Duration::milliseconds(500)).to_rfc3339();
    let offset = instant
        .with_timezone(&chrono::FixedOffset::east_opt(7200).unwrap())
        .to_rfc3339();
    let expected = |from: Option<chrono::DateTime<chrono::FixedOffset>>,
                    to: Option<chrono::DateTime<chrono::FixedOffset>>| {
        items
            .iter()
            .filter(|v| {
                let t = chrono::DateTime::parse_from_rfc3339(v["imported_at"].as_str().unwrap())
                    .unwrap();
                from.is_none_or(|f| t >= f) && to.is_none_or(|u| t < u)
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        page(&base, &dates(Some(&offset), None)).await["items"],
        json!(expected(Some(instant), None))
    );
    assert_eq!(
        page(&base, &dates(Some(&fraction), None)).await["items"],
        json!(expected(
            Some(instant + chrono::Duration::milliseconds(500)),
            None
        ))
    );
    assert_eq!(
        page(&base, &dates(None, Some(&at))).await["items"],
        json!(expected(None, Some(instant)))
    );
    assert_eq!(
        page(&base, &dates(None, Some(&fraction))).await["items"],
        json!(expected(
            None,
            Some(instant + chrono::Duration::milliseconds(500))
        ))
    );
    assert_eq!(
        page(&base, &dates(Some(&at), Some(&fraction))).await["items"],
        json!(expected(
            Some(instant),
            Some(instant + chrono::Duration::milliseconds(500))
        ))
    );
    for query in [
        "?limit=0",
        "?limit=101",
        "?offset=10001",
        "?offset=-1",
        "?episode_id=0",
        "?movie_id=9007199254740992",
        "?episode_id=1&movie_id=1",
        "?media_type=movies&series_id=1",
        "?media_type=tv&movie_id=1",
        "?season=0",
        "?series_id=1&season=-1",
        "?from=not-a-date",
        "?from=2016-12-31T23:59:60Z",
        "?from=2025-01-01T00:00:00.0000000001Z",
        "?to=2025-01-01T00:00:00.1234567890Z",
        "?quality=1",
        "?event_type=grabbed",
        "?media_type=episode",
    ] {
        let (code, error) = request(
            &base,
            "GET",
            &format!("/api/v1/history{query}"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 400, "{query}: {error}");
        assert_eq!(error["error"]["code"], "invalid_history_query");
    }
    for query in [
        dates(Some(&at), Some(&at)),
        dates(Some(&fraction), Some(&at)),
    ] {
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/history{query}"),
                Value::Null
            )
            .await
            .0,
            400
        );
    }
    // Force the real association/history transaction to fail after transfer. No successful event
    // or file association may survive that rollback, and existing history remains unchanged.
    let source = incoming.join("fail.mkv");
    std::fs::write(&source, b"rollback bytes").unwrap();
    let destination = tv.join("failed.mkv");
    let(code,preview)=request(&base,"POST","/api/v1/imports",json!({"target":{"media_type":"episode","id":3},"source":source,"destination":destination,"mode":"copy"})).await;
    assert_eq!(code, 202);
    c.execute_batch("CREATE TRIGGER fixture_history_failure BEFORE INSERT ON import_history WHEN NEW.episode_id=3 BEGIN SELECT RAISE(ABORT,'fixture rejected commit'); END;").await.unwrap();
    assert_ne!(
        request(
            &base,
            "POST",
            &format!(
                "/api/v1/imports/{}/execute",
                preview["id"].as_str().unwrap()
            ),
            Value::Null
        )
        .await
        .0,
        200
    );
    assert_eq!(page(&base, "").await, all);
    assert!(
        c.query("SELECT episode_file_id FROM episodes WHERE id=3", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<Option<i64>>(0)
            .unwrap()
            .is_none()
    );
    c.execute_batch("DROP TRIGGER fixture_history_failure;")
        .await
        .unwrap();
    // Historical file identifiers are facts, not foreign keys to currently retained files.
    c.execute_batch("UPDATE episodes SET episode_file_id=NULL WHERE id=1; DELETE FROM file_metadata WHERE episode_file_id IN (SELECT episode_file_id FROM import_history WHERE episode_id=1) OR movie_file_id IN (SELECT movie_file_id FROM import_history WHERE movie_id=1); DELETE FROM episode_files WHERE id IN (SELECT episode_file_id FROM import_history WHERE episode_id=1); DELETE FROM movie_files WHERE id IN (SELECT movie_file_id FROM import_history WHERE movie_id=1);").await.unwrap();
    assert_eq!(page(&base, "").await, all);
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    assert_eq!(page(&base, "").await, all);
    server.abort();
    let _ = server.await;
    drop(db);
}

async fn history_backup(path: &std::path::Path, root: &std::path::Path, tv: bool) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    if tv {
        c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);CREATE TABLE History(Id INTEGER,EpisodeId INTEGER,SeriesId INTEGER,Date TEXT,EventType INTEGER,SourceTitle TEXT,DownloadId TEXT,Quality TEXT,Languages TEXT,Data TEXT);").await.unwrap();
        c.execute("INSERT INTO Series VALUES(1,100,'Imported TV',2020,?,1,'[{\"seasonNumber\":1,\"monitored\":true}]')",[root.to_str().unwrap()]).await.unwrap();
        c.execute_batch("INSERT INTO Episodes VALUES(1,1,1,1,'Episode',1,0);")
            .await
            .unwrap();
    } else {
        c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);CREATE TABLE History(Id INTEGER,MovieId INTEGER,Date TEXT,EventType INTEGER,SourceTitle TEXT,DownloadId TEXT,Quality TEXT,Languages TEXT,Data TEXT);").await.unwrap();
        c.execute(
            "INSERT INTO Movies VALUES(1,200,'tt200','Imported Movie',2020,?,1,0)",
            [root.to_str().unwrap()],
        )
        .await
        .unwrap();
    }
    let columns = if tv {
        "Id,EpisodeId,SeriesId,Date,EventType,SourceTitle,DownloadId,Quality,Languages,Data"
    } else {
        "Id,MovieId,Date,EventType,SourceTitle,DownloadId,Quality,Languages,Data"
    };
    let values = if tv {
        "1,1,1,?,?,?,?,?,?,?"
    } else {
        "1,1,?,?,?,?,?,?,?"
    };
    c.execute(
        &format!("INSERT INTO History({columns}) VALUES({values})"),
        libsql::params![
            "2020-01-01T00:00:00.1000000Z",
            6,
            "Original source title",
            "SABnzbd_nzo_123",
            r#"{"quality":7,"revision":{"version":1,"real":0,"isRepack":false}}"#,
            "[1,2]",
            r#"{"privateUrl":"https://example.invalid/?apikey=SOURCE_PRIVATE_DATA"}"#
        ],
    )
    .await
    .unwrap();
    let values = if tv {
        "2,1,1,?,1,NULL,?,NULL,NULL,?"
    } else {
        "2,1,?,1,NULL,?,NULL,NULL,?"
    };
    c.execute(
        &format!("INSERT INTO History({columns}) VALUES({values})"),
        libsql::params![
            "2019-12-31T19:00:00.1-05:00",
            "https://example.invalid/?apikey=SOURCE_PRIVATE_DATA",
            r#"{"password":"SOURCE_PRIVATE_DATA"}"#
        ],
    )
    .await
    .unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn snapshot_history_and_native_receipts_share_truthful_scoped_pages() {
    let _fixture = IMPORT_TESTS.lock().await;
    use hrrdarr::snapshots::{self, Application};
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let tv = scratch.0.join("tv");
    let movie = scratch.0.join("movie");
    std::fs::create_dir(&tv).unwrap();
    std::fs::create_dir(&movie).unwrap();
    let tv_bytes = history_backup(&scratch.0.join("sonarr.db"), &tv, true).await;
    let movie_bytes = history_backup(&scratch.0.join("radarr.db"), &movie, false).await;
    let mut identities = Vec::new();
    for (app, bytes) in [
        (Application::Sonarr, tv_bytes.clone()),
        (Application::Radarr, movie_bytes.clone()),
    ] {
        let report = snapshots::import(&db, app, bytes, false).await.unwrap();
        assert!(report.applied);
        identities.push(report.fingerprint);
    }
    let (base, server) = serve(db.clone()).await;
    let before = page(&base, "").await;
    assert_eq!(before["total"], 4);
    assert!(!before.to_string().contains("SOURCE_PRIVATE_DATA"));
    let items = before["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|v| (
                v["id"]["application"].as_str().unwrap(),
                v["id"]["source_id"].as_i64().unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![("radarr", 2), ("radarr", 1), ("sonarr", 2), ("sonarr", 1)]
    );
    for item in items {
        assert_eq!(item["origin"], "source_snapshot");
        assert_eq!(item["occurred_at"], "2020-01-01T00:00:00.100Z");
        for absent in [
            "file",
            "source",
            "destination",
            "sha256",
            "size_bytes",
            "imported_at",
            "data",
            "Data",
        ] {
            assert!(
                item.get(absent).is_none(),
                "source history must not invent {absent}"
            );
        }
        let tv = item["id"]["application"] == "sonarr";
        assert_eq!(
            item["id"]["fingerprint"],
            identities[if tv { 0 } else { 1 }]
        );
        assert_eq!(
            item["target"],
            json!({"media_type":if tv{"episode"}else{"movie"},"id":1})
        );
        if item["id"]["source_id"] == 1 {
            // The same source event integer is different domain behavior: TV6 renames, movie6 deletes.
            assert_eq!(item["source_event_type"], 6);
            assert_eq!(
                item["event_type"],
                if tv { "file_renamed" } else { "file_deleted" }
            );
            assert_eq!(item["source_title"], "Original source title");
            assert_eq!(item["download_id"], "SABnzbd_nzo_123");
            assert_eq!(
                item["quality"],
                json!({"quality_id":7,"revision":{"version":1,"real":0,"is_repack":false}})
            );
            assert_eq!(item["languages"], json!([1, 2]));
        } else {
            assert_eq!(item["event_type"], "grabbed");
            for field in ["source_title", "download_id", "quality", "languages"] {
                assert!(item[field].is_null());
            }
        }
    }
    // Exact reuploads are tested before native imports change the snapshot's no-file episode
    // association; that later library change correctly belongs to core reconciliation conflicts.
    for (app, bytes) in [
        (Application::Sonarr, tv_bytes),
        (Application::Radarr, movie_bytes),
    ] {
        let report = snapshots::import(&db, app, bytes, false).await.unwrap();
        assert!(report.applied);
        assert_eq!(report.conflicts, 0);
    }
    assert_eq!(page(&base, "").await, before);
    for (media, root) in [("episode", &tv), ("movie", &movie)] {
        let source = scratch.0.join(format!("{media}.mkv"));
        std::fs::write(&source, b"owned mixed history receipt").unwrap();
        let destination = root.join("imported.mkv");
        let(code,preview)=request(&base,"POST","/api/v1/imports",json!({"target":{"media_type":media,"id":1},"source":source,"destination":destination,"mode":"copy"})).await;
        assert_eq!(code, 202, "{preview}");
        let (code, done) = request(
            &base,
            "POST",
            &format!(
                "/api/v1/imports/{}/execute",
                preview["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200, "{done}");
    }
    let mixed = page(&base, "").await;
    assert_eq!(mixed["total"], 6);
    for item in mixed["items"].as_array().unwrap().iter().take(2) {
        assert_eq!(item["origin"], "native_import");
        assert!(item["id"].is_string());
        assert_eq!(item["event_type"], "file_imported");
        assert!(item.get("occurred_at").is_none());
    }
    assert_eq!(&mixed["items"].as_array().unwrap()[2..], items.as_slice());
    for offset in 0..6 {
        let p = page(&base, &format!("?offset={offset}&limit=1")).await;
        assert_eq!(p["total"], 6);
        assert_eq!(p["items"][0], mixed["items"][offset]);
    }
    for query in [
        "?episode_id=1",
        "?movie_id=1",
        "?media_type=tv&series_id=1&season=1",
        "?media_type=movies",
    ] {
        assert_eq!(page(&base, query).await["total"], 3);
    }
    for from in [
        "2020-01-01T00:00:00.1Z",
        "2020-01-01T00:00:00.100000000Z",
        "2019-12-31T19:00:00.100-05:00",
    ] {
        let p = page(&base, &dates(Some(from), Some("2020-01-01T00:00:00.101Z"))).await;
        assert_eq!(
            p["items"], before["items"],
            "equivalent fractional instants must include the same boundary"
        );
    }
    assert_eq!(
        page(
            &base,
            &dates(
                Some("2020-01-01T00:00:00.100000001Z"),
                Some("2020-01-01T00:00:00.101Z")
            )
        )
        .await["total"],
        0
    );
    assert_eq!(
        page(&base, &dates(None, Some("2020-01-01T00:00:00.1000Z"))).await["total"],
        0
    );
    // A supplemental synthetic source fact uses an actual native event's immutable timestamp.
    // This isolates cross-origin tie ordering without rewriting producer history or claiming
    // that this added row came from the source reader's separate end-to-end fixture.
    let c = db.connect().await.unwrap();
    let native_id = mixed["items"][0]["id"].as_str().unwrap();
    let timestamp = c
        .query(
            "SELECT imported_at FROM import_history WHERE operation_id=?",
            [native_id],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    c.execute("INSERT INTO snapshot_history_events(application,fingerprint,source_id,media_type,episode_id,occurred_at,event_type,source_event_type) VALUES('sonarr',?,3,'episode',1,?,'grabbed',1)", libsql::params![identities[0].clone(),timestamp]).await.unwrap();
    let mixed = page(&base, "").await;
    assert_eq!(mixed["total"], 7);
    let tied = mixed["items"]
        .as_array()
        .unwrap()
        .iter()
        .take_while(|v| v["origin"] == "native_import")
        .count();
    assert!(tied >= 1);
    assert_eq!(mixed["items"][tied]["origin"], "source_snapshot");
    assert_eq!(mixed["items"][tied]["id"]["source_id"], 3);
    for offset in 0..7 {
        assert_eq!(
            page(&base, &format!("?limit=1&offset={offset}")).await["items"][0],
            mixed["items"][offset]
        );
    }
    drop(c);
    assert_eq!(page(&base, "").await, mixed);
    server.abort();
    let _ = server.await;
    drop(db);
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    assert_eq!(page(&base, "").await, mixed);
    server.abort();
    let _ = server.await;
    drop(db);
}
