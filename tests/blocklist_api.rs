use axum::{
    Json,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use hrrdarr::{api::SnapshotOptions, blocklist, db::Database, snapshots};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinHandle;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-blocklist-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("scratch cleanup failed: {e}")
        }
    }
}
async fn migrate(
    State(db): State<Arc<Database>>,
    Query(q): Query<SnapshotOptions>,
    body: axum::body::Bytes,
) -> Response {
    match snapshots::import(&db, q.application, body.to_vec(), q.dry_run).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"snapshot_rejected"})),
        )
            .into_response(),
    }
}
async fn serve(db: Arc<Database>) -> (String, JoinHandle<()>) {
    let app = blocklist::router(db.clone()).merge(
        axum::Router::new()
            .route("/api/v1/migrations", post(migrate))
            .layer(DefaultBodyLimit::max(snapshots::MAX_SNAPSHOT_BYTES))
            .with_state(db),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn request(base: &str, method: &str, path: &str, body: Vec<u8>) -> (u16, Value) {
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let code = response.status().as_u16();
    let text = response.text().await.unwrap();
    assert!(!text.contains("BLOCKLIST_PRIVATE"));
    (
        code,
        if code == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{code}: {text}"))
        },
    )
}
async fn page(base: &str, query: &str) -> Value {
    let (code, v) = request(base, "GET", &format!("/api/v1/blocklist{query}"), vec![]).await;
    assert_eq!(code, 200, "{v}");
    v
}
fn identity_path(id: &Value) -> String {
    format!(
        "/api/v1/blocklist/{}/{}/{}",
        id["application"].as_str().unwrap(),
        id["fingerprint"].as_str().unwrap(),
        id["source_id"].as_i64().unwrap()
    )
}
async fn backup(path: &Path, root: &Path, version: i64) -> Vec<u8> {
    let tv = version == 233;
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(&format!(
        "CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});"
    ))
    .await
    .unwrap();
    if tv {
        c.execute_batch("CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,Date TEXT,PublishedDate TEXT,SourceTitle TEXT,Protocol INTEGER,Size INTEGER,Quality TEXT,Languages TEXT,Message TEXT,Source TEXT,Indexer TEXT);").await.unwrap();
        c.execute("INSERT INTO Series VALUES(1,101,'Series',2020,?,1,'[{\"seasonNumber\":1,\"monitored\":true}]')",[root.to_str().unwrap()]).await.unwrap();
        c.execute_batch("INSERT INTO Episodes VALUES(1,1,1,1,'One',1,0),(2,1,1,2,'Two',1,0)")
            .await
            .unwrap();
    } else {
        c.execute_batch("CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);CREATE TABLE Blocklist(Id INTEGER,MovieId INTEGER,Date TEXT,PublishedDate TEXT,SourceTitle TEXT,Protocol INTEGER,Size INTEGER,Quality TEXT,Languages TEXT,Message TEXT,Indexer TEXT);").await.unwrap();
        c.execute(
            "INSERT INTO Movies VALUES(1,101,'tt101','Movie',2020,?,1,0)",
            [root.to_str().unwrap()],
        )
        .await
        .unwrap();
    }
    if version == 242 {
        // Radarr 242 stores catalog facts separately; exercise its actual split snapshot layout.
        c.execute_batch("ALTER TABLE Movies RENAME TO InlineMovies; CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER); INSERT INTO MovieMetadata SELECT Id,TmdbId,ImdbId,Title,Year FROM InlineMovies; CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER); INSERT INTO Movies SELECT Id,Id,Path,Monitored,MovieFileId FROM InlineMovies; DROP TABLE InlineMovies;").await.unwrap();
    }
    for (id, title, date, protocol) in [
        (1, "Zeta pack", "2020-01-01T00:00:00.1000Z", 2),
        (
            2,
            "Alpha release",
            "2019-12-31T19:00:00.2-05:00",
            if tv { 0 } else { 1 },
        ),
        (3, "Unsupported protocol", "2020-01-02T00:00:00Z", 99),
    ] {
        let sql = if tv {
            "INSERT INTO Blocklist(Id,SeriesId,EpisodeIds,Date,PublishedDate,SourceTitle,Protocol,Size,Quality,Languages,Message,Source,Indexer)VALUES(?,1,?,?,NULL,?,?,1234,?,?, 'BLOCKLIST_PRIVATE_MESSAGE','https://example.invalid/?apikey=BLOCKLIST_PRIVATE_SOURCE','BLOCKLIST_PRIVATE_INDEXER')"
        } else {
            "INSERT INTO Blocklist(Id,MovieId,Date,PublishedDate,SourceTitle,Protocol,Size,Quality,Languages,Message,Indexer)VALUES(?,1,?,NULL,?,?,1234,?,?, 'BLOCKLIST_PRIVATE_MESSAGE','BLOCKLIST_PRIVATE_INDEXER')"
        };
        let mut values = vec![libsql::Value::Integer(id)];
        if tv {
            values.push(libsql::Value::Text(
                if id == 1 { "[1,2]" } else { "[]" }.into(),
            ))
        }
        values.extend([
            libsql::Value::Text(date.into()),
            libsql::Value::Text(title.into()),
            libsql::Value::Integer(protocol),
            libsql::Value::Text(
                r#"{"quality":7,"revision":{"version":1,"real":0,"isRepack":false}}"#.into(),
            ),
            libsql::Value::Text("[1,2]".into()),
        ]);
        c.execute(sql, values).await.unwrap();
    }
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
#[tokio::test]
async fn source_blocklist_pages_and_atomic_removals_survive_reopen_and_reupload() {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    let mut sources = vec![];
    for (version, app) in [(233, "sonarr"), (206, "radarr"), (242, "radarr")] {
        let bytes = backup(
            &scratch.0.join(format!("source-{version}")),
            &scratch
                .0
                .join(if app == "sonarr" { "tv" } else { "movies" }),
            version,
        )
        .await;
        let (code, dry) = request(
            &base,
            "POST",
            &format!("/api/v1/migrations?application={app}&dry_run=true"),
            bytes.clone(),
        )
        .await;
        assert_eq!(code, 200, "{dry}");
        assert_eq!(page(&base, "").await["total"], sources.len() * 2);
        let (code, report) = request(
            &base,
            "POST",
            &format!("/api/v1/migrations?application={app}&dry_run=false"),
            bytes.clone(),
        )
        .await;
        assert_eq!(code, 200, "{report}");
        // HTTP 200 may carry a conflict report; only accepted replay proves tombstone preservation.
        assert_eq!(report["applied"], true, "{report}");
        assert_eq!(report["conflicts"], 0, "{report}");
        assert!(
            report["unsupported"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["table"] == "Blocklist")
        );
        sources.push((app, bytes));
    }
    let all = page(&base, "").await;
    assert_eq!(all["total"], 6);
    let items = all["items"].as_array().unwrap();
    assert!(items.iter().all(|v| v["origin"] == "source_snapshot"
        && v["quality"]["quality_id"] == 7
        && v["languages"] == json!([1, 2])
        && v["size_bytes"] == 1234));
    assert_eq!(items[0]["occurred_at"], "2020-01-01T00:00:00.200Z");
    let tv = page(&base, "?media_type=tv&series_ids=1").await;
    assert_eq!(tv["total"], 2);
    let pack = tv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"]["source_id"] == 1)
        .unwrap();
    // The source pack remains a series-scoped episode set, not a fabricated single episode target.
    assert_eq!(
        pack["target"],
        json!({"media_type":"tv","series_id":1,"episode_ids":[1,2]})
    );
    assert_eq!(tv["items"][0]["target"]["episode_ids"], json!([]));
    let movies = page(&base, "?media_type=movies&movie_ids=1").await;
    assert_eq!(movies["total"], 4);
    assert_eq!(
        movies["items"][0]["target"],
        json!({"media_type":"movies","movie_id":1})
    );
    assert_eq!(page(&base, "?protocols=torrent").await["total"], 3);
    assert_eq!(page(&base, "?protocols=unknown,usenet").await["total"], 3);
    assert_eq!(
        page(&base, "?sort=source_title&sort_direction=asc").await["items"][0]["source_title"],
        "Alpha release"
    );
    let mut paged = vec![];
    for offset in 0..6 {
        paged.push(page(&base, &format!("?limit=1&offset={offset}")).await["items"][0].clone())
    }
    assert_eq!(Value::Array(paged), all["items"]);
    for q in [
        "?series_ids=1",
        "?media_type=tv&movie_ids=1",
        "?media_type=movies&series_ids=1&movie_ids=1",
        "?media_type=tv&series_ids=1,1",
        "?media_type=movies&movie_ids=0",
        "?protocols=torrent,torrent",
        "?protocols=999",
        "?sort=quality",
        "?limit=101",
        "?offset=10001",
        "?unknown=1",
    ] {
        assert_eq!(
            request(&base, "GET", &format!("/api/v1/blocklist{q}"), vec![])
                .await
                .0,
            400,
            "{q}"
        )
    }
    let id = pack["id"].clone();
    let mut unknown = id.clone();
    unknown["source_id"] = json!(999);
    let mixed = json!({"ids":[id,unknown]});
    assert_eq!(
        request(
            &base,
            "DELETE",
            "/api/v1/blocklist/bulk",
            mixed.to_string().into_bytes()
        )
        .await
        .0,
        404
    );
    assert_eq!(
        page(&base, "").await["total"],
        6,
        "unknown bulk identity must roll back all removals"
    );
    for ids in [vec![], vec![id.clone(), id.clone()], vec![id.clone(); 101]] {
        assert_eq!(
            request(
                &base,
                "DELETE",
                "/api/v1/blocklist/bulk",
                json!({"ids":ids}).to_string().into_bytes()
            )
            .await
            .0,
            400
        )
    }
    assert_eq!(
        request(&base, "DELETE", &identity_path(&id), vec![])
            .await
            .0,
        204
    );
    assert_eq!(
        request(&base, "DELETE", &identity_path(&id), vec![])
            .await
            .0,
        204
    );
    assert_eq!(page(&base, "").await["total"], 5);
    let rest = page(&base, "").await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].clone())
        .collect::<Vec<_>>();
    let body = json!({"ids":rest}).to_string().into_bytes();
    assert_eq!(
        request(&base, "DELETE", "/api/v1/blocklist/bulk", body.clone())
            .await
            .0,
        204
    );
    assert_eq!(
        request(&base, "DELETE", "/api/v1/blocklist/bulk", body)
            .await
            .0,
        204
    );
    assert_eq!(page(&base, "").await["total"], 0);
    let c = db.connect().await.unwrap();
    assert_eq!(
        c.query(
            "SELECT count(*) FROM snapshot_blocklist WHERE removed_at IS NOT NULL",
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
        6
    );
    assert_eq!(
        c.query("SELECT count(*) FROM blocklist_episodes", ())
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
    server.abort();
    let _ = server.await;
    drop(db);
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db).await;
    for (app, bytes) in sources {
        let (code, report) = request(
            &base,
            "POST",
            &format!("/api/v1/migrations?application={app}&dry_run=false"),
            bytes,
        )
        .await;
        assert_eq!(code, 200, "{report}");
        // HTTP 200 may carry a conflict report; only accepted replay proves tombstone preservation.
        assert_eq!(report["applied"], true, "{report}");
        assert_eq!(report["conflicts"], 0, "{report}");
        assert_eq!(
            page(&base, "").await["total"],
            0,
            "identical snapshot replay must preserve explicit removals"
        )
    }
    assert_eq!(
        request(&base, "DELETE", &identity_path(&id), vec![])
            .await
            .0,
        204
    );
    server.abort();
    let _ = server.await;
}
