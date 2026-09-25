use hrrdarr::{
    db::{Database, Error},
    media_files,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-media-file-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        stream
            .take(9 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            serde_json::from_str(body).expect("media file response must be JSON"),
        )
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn media_file_http_domains_patch_semantics_and_atomicity() -> Result<(), Error> {
    let files = Sandbox::new();
    let media_path = files.0.join("kept.mkv");
    std::fs::write(&media_path, b"unchanged media")?;
    let db_path = files.0.join("library.db");
    let db = Arc::new(Database::open_local(&db_path).await?);
    let conn = db.connect().await?;
    conn.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/tv'),(2,'Other','/other');
        INSERT INTO seasons VALUES(1,0,1);
        INSERT INTO episode_files VALUES(7,1,'/tv/pack.mkv'),(8,2,'/outside/file.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,0,1,'First',7),(2,1,0,2,'Second',7);
        INSERT INTO movie_metadata(id,title) VALUES(1,'Movie'),(2,'Other movie');
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/a'),(2,2,'/movies/b');
        INSERT INTO movie_files(id,movie_id,path,edition) VALUES(7,1,'/movies/a/file.mkv','Original'),(8,2,'/movies/b/other.mkv',NULL);").await?;
    // A real local media file verifies property edits never modify its bytes.
    conn.execute(
        "INSERT INTO episode_files(id,series_id,path) VALUES(9,1,?1)",
        libsql::params![media_path.to_str().unwrap()],
    )
    .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = media_files::router(db.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let path = |domain: &str, suffix: &str| format!("/api/v1/{domain}/files{suffix}");
    for (domain, selector, expected) in [
        ("tv", "series_id=1", vec![7, 9]),
        ("tv", "file_ids=8,7", vec![7, 8]),
        ("movies", "movie_ids=2,1", vec![7, 8]),
        ("movies", "file_ids=8", vec![8]),
    ] {
        let (code, listed) =
            request(address, "GET", &path(domain, &format!("?{selector}")), "").await;
        assert_eq!(code, 200, "{listed}");
        let actual: Vec<i64> = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(listed["total"], expected.len());
    }
    let (code, page) = request(
        address,
        "GET",
        &path("movies", "?movie_ids=1,2&limit=1&offset=1"),
        "",
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(page["total"], 2);
    assert_eq!(page["limit"], 1);
    assert_eq!(page["offset"], 1);
    assert_eq!(page["items"][0]["movie_id"], 2);
    assert_eq!(page["items"][0]["relative_path"], "other.mkv");
    let (_, tv) = request(address, "GET", &path("tv", "/7"), "").await;
    let (_, movie) = request(address, "GET", &path("movies", "/7"), "").await;
    assert_eq!(tv["series_id"], 1);
    assert_eq!(movie["movie_id"], 1);
    assert_eq!(tv["relative_path"], "pack.mkv");
    assert_eq!(movie["edition"], "Original");
    assert!(tv.get("movie_id").is_none());
    assert!(movie.get("release_type").is_none());
    for field in [
        "quality",
        "languages",
        "size",
        "date_added",
        "scene_name",
        "media_info",
        "custom_formats",
        "custom_format_score",
        "quality_cutoff_not_met",
    ] {
        assert!(
            tv[field].is_null(),
            "unknown {field} must not be fabricated"
        );
    }
    assert!(request(address, "GET", &path("tv", "/8"), "").await.1["relative_path"].is_null());
    let quality = json!({"quality_id":20,"revision":{"version":2,"real":1,"is_repack":true}});
    let patch = json!({"quality":quality,"languages":[0,1,9],"release_group":"Group","indexer_flags":511,"release_type":2});
    let (code, changed) = request(address, "PUT", &path("tv", "/7"), &patch.to_string()).await;
    assert_eq!(code, 200, "{changed}");
    assert_eq!(changed["quality"], quality);
    assert_eq!(changed["languages"], json!([0, 1, 9]));
    assert_eq!(changed["release_type"], 2);
    assert_eq!(
        request(address, "GET", &path("movies", "/7"), "").await.1,
        movie
    );
    // Omitted fields survive; an explicit null clears just the named field.
    let (code, cleared) = request(
        address,
        "PUT",
        &path("tv", "/7"),
        r#"{"release_group":null}"#,
    )
    .await;
    assert_eq!(code, 200);
    assert!(cleared["release_group"].is_null());
    assert_eq!(cleared["quality"], quality);
    assert_eq!(cleared["languages"], json!([0, 1, 9]));
    let batch = json!({"files":[{"id":7,"edition":"Extended","quality":{"quality_id":20,"revision":null}},{"id":8,"edition":"Theatrical","languages":[57],"indexer_flags":4095}]});
    let (code, updated) =
        request(address, "PUT", &path("movies", "/bulk"), &batch.to_string()).await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated[0]["movie_id"], 1);
    assert_eq!(updated[0]["relative_path"], "file.mkv");
    assert_eq!(updated[1]["movie_id"], 2);
    assert_eq!(updated[1]["relative_path"], "other.mkv");
    assert_eq!(updated[1]["edition"], "Theatrical");
    let shared = json!({"file_ids":[7,8],"release_group":"Shared","languages":[],"edition":null});
    let (code, edited) = request(
        address,
        "PUT",
        &path("movies", "/editor"),
        &shared.to_string(),
    )
    .await;
    assert_eq!(code, 200);
    for file in edited.as_array().unwrap() {
        assert_eq!(file["release_group"], "Shared");
        assert_eq!(file["languages"], json!([]));
        assert!(file["edition"].is_null());
    }
    assert_eq!(
        request(
            address,
            "PUT",
            &path("tv", "/9"),
            r#"{"release_group":"Recorded"}"#
        )
        .await
        .0,
        200
    );
    assert_eq!(std::fs::read(&media_path)?, b"unchanged media");
    assert_eq!(
        conn.query("SELECT count(*) FROM episodes WHERE episode_file_id=7", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        2
    );
    for (domain, bad) in [
        ("tv", json!({"quality":{"quality_id":31}})),
        (
            "tv",
            json!({"quality":{"quality_id":20,"revision":{"version":0,"real":0,"is_repack":false}}}),
        ),
        (
            "tv",
            json!({"quality":{"quality_id":20,"revision":{"version":1,"real":-1,"is_repack":false}}}),
        ),
        ("tv", json!({"quality":{"quality_id":20,"secret":"bad"}})),
        ("tv", json!({"languages":[53]})),
        ("movies", json!({"languages":[58]})),
        ("tv", json!({"languages":[-2]})),
        ("movies", json!({"languages":[-1]})),
        ("movies", json!({"languages":[1,1]})),
        ("tv", json!({"indexer_flags":512})),
        ("movies", json!({"indexer_flags":4096})),
        ("tv", json!({"release_type":4})),
        ("movies", json!({"release_type":1})),
        ("tv", json!({"edition":"Wrong domain"})),
        ("tv", json!({"path":"/do-not-move"})),
        ("tv", json!({"size":42})),
        ("tv", json!({"release_group":"x".repeat(1025)})),
        ("tv", json!({})),
    ] {
        let before = request(address, "GET", &path(domain, "/7"), "").await.1;
        let (code, error) = request(address, "PUT", &path(domain, "/7"), &bad.to_string()).await;
        assert_eq!(code, 400, "{bad}: {error}");
        assert_eq!(
            request(address, "GET", &path(domain, "/7"), "").await.1,
            before
        );
    }
    for domain in ["tv", "movies"] {
        let (code, error) = request(
            address,
            "PUT",
            &path(domain, "/7"),
            r#"{"scene_name":"Film.2026.1080p-GROUP"}"#,
        )
        .await;
        assert_eq!(code, 400);
        assert_eq!(error["error"]["code"], "unsupported_field");
        let stable = request(address, "GET", &path(domain, "?file_ids=7,8"), "")
            .await
            .1;
        for (patch, expected) in [
            (
                json!({"files":[{"id":7,"release_group":"Earlier"},{"id":999,"release_group":"Missing"}]}),
                404,
            ),
            (
                json!({"files":[{"id":7,"release_group":"One"},{"id":7,"release_group":"Duplicate"}]}),
                400,
            ),
            (json!({"files":[]}), 400),
            (json!({"files":[{"id":0,"release_group":"Bad"}]}), 400),
            (
                json!({"files":[{"id":7,"release_group":"Earlier"},{"id":8,"languages":[-2]}]}),
                400,
            ),
        ] {
            assert_eq!(
                request(address, "PUT", &path(domain, "/bulk"), &patch.to_string())
                    .await
                    .0,
                expected
            );
            assert_eq!(
                request(address, "GET", &path(domain, "?file_ids=7,8"), "")
                    .await
                    .1,
                stable
            );
        }
    }
    // Late metadata failure also rolls back earlier core movie edition changes.
    conn.execute_batch("CREATE TRIGGER reject_file_metadata BEFORE UPDATE OF release_group ON file_metadata WHEN NEW.movie_file_id=8 BEGIN SELECT RAISE(ABORT,'PRIVATE_FILE_ERROR'); END;").await?;
    let before = request(address, "GET", &path("movies", "?file_ids=7,8"), "")
        .await
        .1;
    let failing = json!({"files":[{"id":7,"edition":"Must roll back","release_group":"Earlier"},{"id":8,"release_group":"Fails"}]});
    let (code, error) = request(
        address,
        "PUT",
        &path("movies", "/bulk"),
        &failing.to_string(),
    )
    .await;
    assert_eq!(code, 500);
    assert!(!error.to_string().contains("PRIVATE_FILE_ERROR"));
    assert_eq!(
        request(address, "GET", &path("movies", "?file_ids=7,8"), "")
            .await
            .1,
        before
    );
    conn.execute("DROP TRIGGER reject_file_metadata", ())
        .await?;
    for (domain, query) in [
        ("tv", ""),
        ("tv", "movie_ids=1"),
        ("movies", "series_id=1"),
        ("tv", "series_id=1&file_ids=7"),
        ("tv", "file_ids=7,7"),
        ("tv", "file_ids=-1"),
        ("tv", "series_id=1&limit=0"),
        ("tv", "series_id=1&limit=501"),
        ("tv", "series_id=1&offset=-1"),
        ("tv", "series_id=1&unknown=1"),
    ] {
        assert_eq!(
            request(address, "GET", &path(domain, &format!("?{query}")), "")
                .await
                .0,
            400,
            "{domain} {query}"
        );
    }
    assert_eq!(
        request(address, "GET", &path("tv", "/999"), "").await.0,
        404
    );
    assert_eq!(
        request(address, "GET", &path("tv", "/bad"), "").await.0,
        400
    );
    assert_eq!(
        request(address, "PUT", &path("tv", "/7"), "null").await.0,
        400
    );
    assert_eq!(
        request(address, "PUT", &path("tv", "/7"), &"x".repeat(262145))
            .await
            .0,
        413
    );
    assert_eq!(
        request(
            address,
            "PUT",
            &path("tv", "/editor"),
            &json!({"file_ids":(1..=201).collect::<Vec<_>>(),"release_group":"Too many"})
                .to_string()
        )
        .await
        .0,
        400
    );
    let (code, cleared) = request(
        address,
        "PUT",
        &path("tv", "/7"),
        r#"{"quality":null,"languages":null,"indexer_flags":null,"release_type":null}"#,
    )
    .await;
    assert_eq!(code, 200);
    assert!(cleared["quality"].is_null());
    assert!(cleared["languages"].is_null());
    assert_eq!(std::fs::read(&media_path)?, b"unchanged media");
    // Real snapshot normalization feeds the public HTTP projection, not handcrafted public JSON.
    for domain in ["tv", "movies"] {
        let snapshot_path = files.0.join(format!("{domain}-media-info.db"));
        let source = libsql::Builder::new_local(&snapshot_path).build().await?;
        let source_conn = source.connect()?;
        let schema = if domain == "tv" {
            "CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
             CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);
             INSERT INTO Series VALUES(10,901,'Imported TV',2026,'/import-tv',1,'[]');
             CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);
             CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT,MediaInfo TEXT);
             INSERT INTO EpisodeFiles VALUES(1,10,'file.mkv',NULL);"
        } else {
            "CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(242);
             CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);
             INSERT INTO MovieMetadata VALUES(50,902,NULL,'Imported movie',2026);
             CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);
             INSERT INTO Movies VALUES(10,50,'/import-movie',1,1);
             CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT,MediaInfo TEXT);
             INSERT INTO MovieFiles VALUES(1,10,'file.mkv',NULL,NULL);"
        };
        source_conn.execute_batch(schema).await?;
        let mut info = json!({"schemaRevision":14,"videoFormat":"AVC","videoCodecID":"V_MPEG4/ISO/AVC",
            "videoBitrate":8000000,"videoFps":23.9764,"width":1920,"height":1080,
            "runTime":"1.02:03:04.1234567","videoHdrFormat":1,"rawStreamData":"PRIVATE_MEDIA_INFO_SENTINEL"});
        let (table, app) = if domain == "tv" {
            info["audioStreams"] = json!([
                {"language":"eng","format":"AAC","codecId":"A_AAC","bitrate":128000,"channels":2,"privateToken":"PRIVATE_MEDIA_INFO_SENTINEL"},
                {"language":"isl","bitrate":256000,"channels":6}]);
            info["subtitleStreams"] = json!([{"language":"isl","forced":false,"hearingImpaired":true,"privateToken":"PRIVATE_MEDIA_INFO_SENTINEL"}]);
            ("EpisodeFiles", hrrdarr::snapshots::Application::Sonarr)
        } else {
            info["audioFormat"] = json!("AC-3");
            info["audioCodecID"] = json!("A_AC3");
            info["audioBitrate"] = json!(640000);
            info["audioChannels"] = json!(6);
            info["audioStreamCount"] = json!(2);
            info["audioLanguages"] = json!(["eng", "isl"]);
            info["subtitles"] = json!(["isl"]);
            info["rawFrameData"] = json!("PRIVATE_MEDIA_INFO_SENTINEL");
            ("MovieFiles", hrrdarr::snapshots::Application::Radarr)
        };
        source_conn
            .execute(
                &format!("UPDATE {table} SET MediaInfo=?1"),
                libsql::params![info.to_string()],
            )
            .await?;
        drop(source_conn);
        drop(source);
        let bytes = std::fs::read(&snapshot_path)?;
        let report = hrrdarr::snapshots::import(&db, app, bytes.clone(), false).await?;
        assert!(report.applied);
        // The report distinguishes retained unknown fields from wholly unmapped MediaInfo.
        assert!(report.unsupported.iter().any(|item| {
            item.table == table
                && item
                    .columns
                    .iter()
                    .any(|c| c == "MediaInfo.unsupported_fields_or_values")
        }));
        assert!(
            hrrdarr::snapshots::import(&db, app, bytes, false)
                .await?
                .applied
        );
        let (file_table, stored_path) = if domain == "tv" {
            ("episode_files", "/import-tv/file.mkv")
        } else {
            ("movie_files", "/import-movie/file.mkv")
        };
        let id = conn
            .query(
                &format!("SELECT id FROM {file_table} WHERE path=?1"),
                libsql::params![stored_path],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?;
        let (status, response) =
            request(address, "GET", &path(domain, &format!("/{id}")), "").await;
        assert_eq!(status, 200, "{response}");
        assert!(!response.to_string().contains("PRIVATE_MEDIA_INFO_SENTINEL"));
        let projected = &response["media_info"];
        assert_eq!(projected["resolution"], "1920x1080");
        assert_eq!(projected["video_codec_id"], "V_MPEG4/ISO/AVC");
        assert_eq!(projected["video_fps"], 23.976);
        assert_eq!(projected["runtime_ticks"], 937841234567_i64);
        assert_eq!(projected["run_time"], "26:03:04");
        assert_eq!(projected["audio_stream_count"], 2);
        assert_eq!(projected["audio_languages"], "eng/isl");
        assert_eq!(projected["subtitles"], "isl");
        assert_eq!(
            projected["video_dynamic_range_type"],
            if domain == "tv" { "HDR" } else { "PQ" }
        );
        assert_eq!(
            projected["audio_bitrate"],
            if domain == "tv" { 128000 } else { 640000 }
        );
        assert_eq!(
            projected["audio_codec_id"],
            if domain == "tv" { "A_AAC" } else { "A_AC3" }
        );
        assert!(projected["audio_codec"].is_null());
        assert!(projected["video_codec"].is_null());
        let archive = conn
            .query(
                "SELECT record_json FROM snapshot_records WHERE source_table=?1 AND fingerprint=?2",
                libsql::params![table, report.fingerprint],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?;
        assert!(archive.contains("PRIVATE_MEDIA_INFO_SENTINEL"));
        // A nested unknown key in corrupted persisted data must fail closed, not leak.
        let target = if domain == "tv" {
            "episode_file_id"
        } else {
            "movie_file_id"
        };
        let raw = conn
            .query(
                &format!("SELECT media_info_json FROM file_metadata WHERE {target}=?1"),
                [id],
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?;
        let mut corrupt: Value = serde_json::from_str(&raw)?;
        corrupt["audio_streams"] = json!([{"privateToken":"PRIVATE_MEDIA_INFO_SENTINEL"}]);
        conn.execute(
            &format!("UPDATE file_metadata SET media_info_json=?1 WHERE {target}=?2"),
            libsql::params![corrupt.to_string(), id],
        )
        .await?;
        let (status, error) = request(address, "GET", &path(domain, &format!("/{id}")), "").await;
        assert_eq!(status, 500);
        assert!(!error.to_string().contains("PRIVATE_MEDIA_INFO_SENTINEL"));
        conn.execute(
            &format!("UPDATE file_metadata SET media_info_json=?1 WHERE {target}=?2"),
            libsql::params![raw, id],
        )
        .await?;
    }
    let too_many_ids = (1..=201)
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    for (domain, selector) in [("tv", "file_ids"), ("movies", "movie_ids")] {
        assert_eq!(
            request(
                address,
                "GET",
                &path(domain, &format!("?{selector}={too_many_ids}")),
                ""
            )
            .await
            .0,
            400
        );
    }
    // Core paths are intentionally oversized synthetic DB values, never filesystem paths accessed here.
    // Each row fits the response budget; the pair exceeds it because both path forms are returned.
    for id in [100, 101] {
        conn.execute(
            "INSERT INTO episode_files(id,series_id,path) VALUES(?1,1,?2)",
            libsql::params![id, format!("/tv/{id}{}", "x".repeat(2_200_000))],
        )
        .await?;
    }
    let (status, error) = request(address, "GET", &path("tv", "?file_ids=100,101"), "").await;
    assert_eq!(status, 413);
    assert_eq!(error["error"]["code"], "response_too_large");
    let (status, smaller) =
        request(address, "GET", &path("tv", "?file_ids=100,101&limit=1"), "").await;
    assert_eq!(status, 200);
    assert_eq!(smaller["total"], 2);
    assert_eq!(smaller["items"].as_array().unwrap().len(), 1);
    // Unlike request validation, response-size rejection occurs after metadata inserts/updates.
    let (status, error) = request(
        address,
        "PUT",
        &path("tv", "/editor"),
        r#"{"file_ids":[100,101],"release_group":"Must roll back"}"#,
    )
    .await;
    assert_eq!(status, 413);
    assert_eq!(error["error"]["code"], "response_too_large");
    assert_eq!(
        conn.query(
            "SELECT count(*) FROM file_metadata WHERE episode_file_id IN (100,101)",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    server.abort();
    let _ = server.await;
    drop(conn);
    drop(db);
    let reopened = Database::open_local(&db_path).await?;
    let c = reopened.connect().await?;
    let row=c.query("SELECT quality_id,languages_json,release_group FROM file_metadata WHERE media_type='movies' AND movie_file_id=7",()).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 20);
    assert_eq!(row.get::<String>(1)?, "[]");
    assert_eq!(row.get::<String>(2)?, "Shared");
    Ok(())
}
