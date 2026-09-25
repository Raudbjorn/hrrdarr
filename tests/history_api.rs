use hrrdarr::{db::Database, history, import};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
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
