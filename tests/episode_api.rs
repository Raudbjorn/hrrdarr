use hrrdarr::{
    db::{Database, Error},
    episodes,
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
            std::env::temp_dir().join(format!("hrrdarr-episode-api-{}", uuid::Uuid::new_v4()));
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
        stream.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            serde_json::from_str(body).expect("episode response must be JSON"),
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn episode_http_selectors_projections_atomic_monitoring_and_budgets() -> Result<(), Error> {
    let files = Sandbox::new();
    let db = Arc::new(Database::open_local(files.0.join("library.db")).await?);
    let conn = db.connect().await?;
    conn.execute_batch(r#"INSERT INTO series(id,title,path,monitored) VALUES(1,'TV','/tv',1),(2,'Other','/other',1);
        INSERT INTO seasons VALUES(1,0,1),(1,1,1),(2,1,1);
        INSERT INTO episode_files VALUES(7,1,'/tv/pack.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,monitored) VALUES
        (1,1,0,1,'Special',7,1),(2,1,0,2,'Special two',7,1),(3,1,1,1,'Absent',NULL,1),(4,2,1,1,'Other',NULL,1);
        INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,99,'Movie');
        INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/movie',1);
        INSERT INTO movie_files(id,movie_id,path) VALUES(7,1,'/movie/file.mkv');
        UPDATE episodes SET tvdb_id=123,air_date='2026-09-25',air_date_utc='2026-09-25T12:00:00Z',
        last_search_time='2026-09-24T12:00:00Z',runtime=42,finale_type='season',overview='Overview',
        absolute_episode_number=10,scene_absolute_episode_number=11,scene_episode_number=12,
        scene_season_number=2,unverified_scene_numbering=1,
        images_json='[{"coverType":"screenshot","url":"https://example.test/image"}]' WHERE id=1;"#) .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = episodes::router(db.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (status, page) = request(
        address,
        "GET",
        "/api/v1/episodes?series_id=1&limit=1&offset=1",
        "",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"][0]["id"], 2);
    assert!(page["items"][0].get("series").is_none());
    assert!(page["items"][0].get("images").is_none());
    assert!(page["items"][0].get("episode_file").is_none());
    for (selector, expected) in [
        ("series_id=1&season=0", vec![1, 2]),
        ("series_id=1&season=1", vec![3]),
        ("episode_file_id=7", vec![1, 2]),
        ("episode_ids=4,1", vec![1, 4]),
        ("series_id=999", vec![]),
        ("series_id=1&offset=100", vec![]),
    ] {
        let (status, result) =
            request(address, "GET", &format!("/api/v1/episodes?{selector}"), "").await;
        assert_eq!(status, 200);
        let ids: Vec<i64> = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, expected, "{selector}");
    }
    let (status, detail) = request(address, "GET", "/api/v1/episodes/1", "").await;
    assert_eq!(status, 200);
    for (field, expected) in [
        ("tvdb_id", json!(123)),
        ("air_date", json!("2026-09-25")),
        ("air_date_utc", json!("2026-09-25T12:00:00Z")),
        ("last_search_time", json!("2026-09-24T12:00:00Z")),
        ("runtime", json!(42)),
        ("finale_type", json!("season")),
        ("overview", json!("Overview")),
        ("absolute_episode_number", json!(10)),
        ("scene_absolute_episode_number", json!(11)),
        ("scene_episode_number", json!(12)),
        ("scene_season_number", json!(2)),
        ("unverified_scene_numbering", json!(true)),
        ("has_file", json!(true)),
    ] {
        assert_eq!(detail[field], expected, "{field}");
    }
    assert_eq!(detail["series"]["id"], 1);
    assert_eq!(detail["episode_file"]["path"], "/tv/pack.mkv");
    assert_eq!(detail["images"][0]["coverType"], "screenshot");
    let (_,included)=request(address,"GET","/api/v1/episodes?episode_ids=1&include_series=true&include_episode_file=true&include_images=true","").await;
    assert_eq!(included["items"][0], detail);
    let (_, absent) = request(address, "GET", "/api/v1/episodes/3", "").await;
    assert_eq!(absent["has_file"], false);
    assert!(absent["file_path"].is_null());
    assert!(absent["runtime"].is_null());
    let (_, legacy) = request(address, "GET", "/api/v1/series/1/episodes", "").await;
    assert_eq!(legacy.as_array().unwrap().len(), 3);
    assert_eq!(
        legacy[0],
        json!({"id":1,"series_id":1,"season":0,"number":1,"title":"Special","file_path":"/tv/pack.mkv"})
    );

    assert_eq!(
        request(
            address,
            "PUT",
            "/api/v1/episodes/1",
            r#"{"monitored":false}"#
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(address, "GET", "/api/v1/episodes/1", "").await.1["monitored"],
        false
    );
    let bulk = r#"{"episode_ids":[1,2],"monitored":false}"#;
    let (status, result) = request(
        address,
        "PUT",
        "/api/v1/episodes/monitor?include_images=true",
        bulk,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(result[0]["images"], detail["images"]);
    assert_eq!(result[1]["monitored"], false);
    let stable = request(address, "GET", "/api/v1/episodes?series_id=1", "")
        .await
        .1;
    for (body, code) in [
        (json!({"episode_ids":[1,999],"monitored":true}), 404),
        (json!({"episode_ids":[1,1],"monitored":true}), 400),
        (json!({"episode_ids":[],"monitored":true}), 400),
        (json!({"episode_ids":[0],"monitored":true}), 400),
        (
            json!({"episode_ids":(1..=201).collect::<Vec<_>>(),"monitored":true}),
            400,
        ),
    ] {
        assert_eq!(
            request(
                address,
                "PUT",
                "/api/v1/episodes/monitor",
                &body.to_string()
            )
            .await
            .0,
            code
        );
        assert_eq!(
            request(address, "GET", "/api/v1/episodes?series_id=1", "")
                .await
                .1,
            stable
        );
    }
    // The second write fails after the first succeeded inside the transaction.
    conn.execute_batch("CREATE TRIGGER fail_monitor BEFORE UPDATE OF monitored ON episodes WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT,'PRIVATE_EPISODE_FAILURE'); END;").await?;
    let (status, error) = request(
        address,
        "PUT",
        "/api/v1/episodes/monitor",
        r#"{"episode_ids":[1,2],"monitored":true}"#,
    )
    .await;
    assert_eq!(status, 500);
    assert!(!error.to_string().contains("PRIVATE_EPISODE_FAILURE"));
    assert_eq!(
        request(address, "GET", "/api/v1/episodes?series_id=1", "")
            .await
            .1,
        stable
    );
    conn.execute("DROP TRIGGER fail_monitor", ()).await?;
    let row=conn.query("SELECT (SELECT monitored FROM series WHERE id=1),(SELECT monitored FROM seasons WHERE series_id=1 AND number=0),(SELECT monitored FROM movies WHERE id=1),(SELECT path FROM movie_files WHERE id=7),(SELECT episode_file_id FROM episodes WHERE id=1)",()).await?.next().await?.unwrap();
    assert_eq!(
        (row.get::<i64>(0)?, row.get::<i64>(1)?, row.get::<i64>(2)?),
        (1, 1, 1)
    );
    assert_eq!(row.get::<String>(3)?, "/movie/file.mkv");
    assert_eq!(row.get::<i64>(4)?, 7);
    for query in [
        "",
        "series_id=0",
        "series_id=1&season=-1",
        "episode_file_id=0",
        "episode_ids=1,1",
        "episode_ids=1&season=0",
        "series_id=1&episode_ids=1",
        "series_id=1&limit=0",
        "series_id=1&limit=501",
        "series_id=1&offset=-1",
        "series_id=1&unknown=1",
        "series_id=1&include_images=maybe",
    ] {
        assert_eq!(
            request(address, "GET", &format!("/api/v1/episodes?{query}"), "")
                .await
                .0,
            400,
            "{query}"
        );
    }
    assert_eq!(
        request(address, "GET", "/api/v1/episodes/999", "").await.0,
        404
    );
    assert_eq!(
        request(address, "GET", "/api/v1/episodes/bad", "").await.0,
        400
    );
    for body in [
        "null",
        r#"{"monitored":"true"}"#,
        r#"{"monitored":true,"title":"forbidden"}"#,
    ] {
        assert_eq!(
            request(address, "PUT", "/api/v1/episodes/1", body).await.0,
            400
        );
    }
    assert_eq!(
        request(address, "PUT", "/api/v1/episodes/1", &"x".repeat(17000))
            .await
            .0,
        413
    );

    // The byte budget rejects a large page while a smaller page remains usable.
    conn.execute_batch("INSERT INTO series(id,title,path) VALUES(3,'Large','/large'); INSERT INTO seasons VALUES(3,1,1);
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<24)
        INSERT INTO episodes(id,series_id,season,number,title,overview) SELECT 100+x,3,1,x,'Large',replace(hex(zeroblob(32768)),'0','x') FROM n;").await?;
    assert_eq!(
        request(address, "GET", "/api/v1/episodes?series_id=3&limit=24", "")
            .await
            .0,
        409
    );
    assert_eq!(
        request(address, "GET", "/api/v1/episodes?series_id=3&limit=1", "")
            .await
            .0,
        200
    );
    // Oversized bulk monitoring must leave all rows unchanged.
    let oversized = json!({"episode_ids":(101..=124).collect::<Vec<_>>(),"monitored":false});
    assert_eq!(
        request(
            address,
            "PUT",
            "/api/v1/episodes/monitor",
            &oversized.to_string()
        )
        .await
        .0,
        409
    );
    assert_eq!(
        conn.query(
            "SELECT count(*) FROM episodes WHERE series_id=3 AND monitored=0",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        0
    );
    assert_eq!(
        request(address, "GET", "/api/v1/series/3/episodes", "")
            .await
            .0,
        200
    );
    conn.execute_batch("INSERT INTO series(id,title,path) VALUES(4,'Many','/many'); INSERT INTO seasons VALUES(4,1,1);
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001)
        INSERT INTO episodes(id,series_id,season,number,title) SELECT 1000+x,4,1,x,'Many' FROM n;").await?;
    assert_eq!(
        request(address, "GET", "/api/v1/series/4/episodes", "")
            .await
            .0,
        409
    );
    assert_eq!(
        request(address, "GET", "/api/v1/episodes?series_id=4&limit=1", "")
            .await
            .1["total"],
        10001
    );
    server.abort();
    let _ = server.await;
    Ok(())
}
