use hrrdarr::{
    db::{Database, Error},
    library,
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
            std::env::temp_dir().join(format!("hrrdarr-library-api-{}", uuid::Uuid::new_v4()));
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
            serde_json::from_str(body).expect("library response must be JSON"),
        )
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn native_library_domains_monitoring_statistics_and_atomicity() -> Result<(), Error> {
    let files = Sandbox::new();
    let db_path = files.0.join("library.db");
    let db = Arc::new(Database::open_local(&db_path).await?);
    let conn = db.connect().await?;
    conn.execute_batch("INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'tv','TV'),(2,'movies','Movies'); INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(50,500,'Catalog only',2020);").await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = library::router(db.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let tv = "/api/v1/tv/series";
    let movies = "/api/v1/movies";
    let (status, page) = request(address, "GET", movies, "").await;
    assert_eq!(status, 200);
    assert_eq!(page["total"], 0);
    let (status,series)=request(address,"POST",tv,&json!({"title":"Show","tvdb_id":100,"path":"/declared/tv","settings":{"quality_profile_id":1,"series_type":"anime","seasons":[{"number":0,"monitored":true},{"number":1,"monitored":true}]}}).to_string()).await;
    assert_eq!(status, 201, "{series}");
    let sid = series["id"].as_i64().unwrap();
    let (status,movie)=request(address,"POST",movies,&json!({"metadata_id":50,"path":"/declared/movie","settings":{"quality_profile_id":2,"minimum_availability":"released"}}).to_string()).await;
    assert_eq!(status, 201, "{movie}");
    let mid = movie["id"].as_i64().unwrap();
    assert_eq!(
        sid, mid,
        "Independent domain sequences can share a numeric ID"
    );
    assert_eq!(movie["title"], "Catalog only");
    assert_eq!(movie["metadata_id"], 50);
    assert!(movie["is_available"].is_null());
    let (status, second) = request(
        address,
        "POST",
        movies,
        r#"{"title":"Second","tmdb_id":501,"year":2021,"path":"/declared/second"}"#,
    )
    .await;
    assert_eq!(status, 201, "{second}");
    let second_id = second["id"].as_i64().unwrap();
    for payload in [
        json!({"metadata_id":50,"path":"/duplicate"}),
        json!({"title":"Duplicate","tmdb_id":501,"path":"/duplicate"}),
    ] {
        assert_eq!(
            request(address, "POST", movies, &payload.to_string())
                .await
                .0,
            409
        );
    }
    for (route, patch) in [
        (format!("{tv}/{sid}"), json!({"quality_profile_id":2})),
        (format!("{movies}/{mid}"), json!({"quality_profile_id":1})),
    ] {
        assert_eq!(
            request(address, "PUT", &route, &patch.to_string()).await.0,
            400
        );
    }
    conn.execute_batch(&format!("INSERT INTO episode_files(id,series_id,path) VALUES(10,{sid},'/declared/tv/pack.mkv'),(11,{sid},'/declared/tv/special.mkv');
      INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,monitored) VALUES(1,{sid},1,1,'First',10,1),(2,{sid},1,2,'Second',10,0),(3,{sid},0,1,'Special',11,1);
      INSERT INTO file_metadata(media_type,episode_file_id,size,season_number) VALUES('tv',10,100,1),('tv',11,50,0);")).await?;
    let (status, item) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert_eq!(status, 200, "{item}");
    let stats = &item["statistics"];
    assert_eq!(stats["total_episode_count"], 3);
    assert_eq!(stats["episode_file_count"], 3);
    assert_eq!(stats["file_count"], 2);
    assert_eq!(stats["size_on_disk"], 150);
    assert_eq!(stats["season_count"], 1);
    // Setting an unchanged season flag preserves the individual episode override.
    for patch in [
        json!({"monitored":false}),
        json!({"seasons":[{"number":1,"monitored":true}]}),
    ] {
        assert_eq!(
            request(address, "PUT", &format!("{tv}/{sid}"), &patch.to_string())
                .await
                .0,
            200
        );
    }
    assert_eq!(
        conn.query("SELECT monitored FROM episodes WHERE id=2", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        0
    );
    for flag in [false, true] {
        assert_eq!(
            request(
                address,
                "PUT",
                &format!("{tv}/{sid}"),
                &json!({"seasons":[{"number":1,"monitored":flag}]}).to_string()
            )
            .await
            .0,
            200
        );
        assert_eq!(
            conn.query("SELECT sum(monitored) FROM episodes WHERE season=1", ())
                .await?
                .next()
                .await?
                .unwrap()
                .get::<i64>(0)?,
            if flag { 2 } else { 0 }
        );
    }
    assert_eq!(
        conn.query("SELECT monitored FROM episodes WHERE id=3", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        1
    );
    conn.execute(
        "UPDATE file_metadata SET size=NULL WHERE episode_file_id=11",
        (),
    )
    .await?;
    let (_, item) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert!(item["statistics"]["size_on_disk"].is_null());
    assert_eq!(item["statistics"]["known_size_file_count"], 1);
    for (route, patch) in [
        (format!("{tv}/{sid}"), json!({"path":"/moved"})),
        (format!("{tv}/{sid}"), json!({"move_files":true})),
        (format!("{movies}/{mid}"), json!({"search_on_add":true})),
        (format!("{movies}/{mid}"), json!({"series_type":"anime"})),
        (
            format!("{tv}/{sid}"),
            json!({"minimum_availability":"released"}),
        ),
        (format!("{tv}/{sid}"), json!({"monitored":null})),
        (
            format!("{tv}/{sid}"),
            json!({"seasons":[{"number":99,"monitored":false}]}),
        ),
    ] {
        let (status, error) = request(address, "PUT", &route, &patch.to_string()).await;
        // Missing seasons are validly shaped references; unsupported fields are malformed patches.
        assert_eq!(
            status,
            if patch.get("seasons").is_some() {
                404
            } else {
                400
            },
            "{error}"
        );
    }
    for query in [
        "limit=0",
        "limit=501",
        "offset=-1",
        "ids=1,1",
        "ids=0",
        "tmdb_id=1",
        "ids=1&tvdb_id=100",
        "unknown=1",
    ] {
        assert_eq!(
            request(address, "GET", &format!("{tv}?{query}"), "")
                .await
                .0,
            400,
            "{query}"
        );
    }
    let (_, page) = request(address, "GET", &format!("{movies}?limit=1&offset=1"), "").await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"][0]["id"], second_id);
    let (_, page) = request(address, "GET", &format!("{tv}?tvdb_id=100"), "").await;
    assert_eq!(page["items"][0]["id"], sid);
    let (_, page) = request(address, "GET", &format!("{movies}?tmdb_id=500"), "").await;
    assert_eq!(page["items"][0]["id"], mid);
    // A missing later target cannot commit the earlier monitoring change.
    let (status, _) = request(
        address,
        "PUT",
        &format!("{movies}/editor"),
        &json!({"ids":[mid,999999],"patch":{"monitored":false}}).to_string(),
    )
    .await;
    assert_eq!(status, 404);
    assert!(
        request(address, "GET", &format!("{movies}/{mid}"), "")
            .await
            .1["monitored"]
            .as_bool()
            .unwrap()
    );
    conn.execute_batch(&format!("CREATE TRIGGER fail_late_library BEFORE UPDATE ON movies WHEN NEW.id={second_id} BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")).await?;
    let (status,_)=request(address,"PUT",&format!("{movies}/bulk"),&json!({"items":[{"id":mid,"patch":{"monitored":false,"minimum_availability":"announced"}},{"id":second_id,"patch":{"monitored":false}}]}).to_string()).await;
    assert_eq!(status, 500);
    let (_, movie) = request(address, "GET", &format!("{movies}/{mid}"), "").await;
    assert_eq!(movie["monitored"], true);
    assert_eq!(movie["settings"]["minimum_availability"], "released");
    conn.execute_batch("DROP TRIGGER fail_late_library;")
        .await?;
    assert_eq!(
        request(
            address,
            "PUT",
            &format!("{movies}/editor"),
            &json!({"ids":[mid,second_id],"patch":{"monitored":false}}).to_string()
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(
            address,
            "PUT",
            &format!("{tv}/{sid}"),
            r#"{"quality_profile_id":null}"#
        )
        .await
        .0,
        200
    );
    let (_, item) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert!(item["settings"]["quality_profile_id"].is_null());
    assert_eq!(item["settings"]["series_type"], "anime");
    // Eligibility counts aired monitored episodes and any existing association, even unmonitored.
    conn.execute_batch(&format!("INSERT INTO episodes(id,series_id,season,number,title,monitored,air_date_utc) VALUES(4,{sid},1,4,'Past',1,'2000-01-01T00:00:00Z'),(5,{sid},1,5,'Future',1,'2999-01-01T00:00:00Z'),(6,{sid},1,6,'Unknown',1,NULL),(7,{sid},1,7,'Past ignored',0,'2000-01-01T00:00:00Z'); UPDATE episodes SET monitored=0 WHERE id=2; INSERT INTO movie_files(id,movie_id,path) VALUES(10,{mid},'/movie/same-id.mkv'); INSERT INTO file_metadata(media_type,movie_file_id,size) VALUES('movies',10,777);")).await?;
    let (_, item) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert_eq!(item["statistics"]["total_episode_count"], 7);
    assert_eq!(item["statistics"]["episode_count"], 4);
    assert_eq!(item["statistics"]["episode_file_count"], 3);
    let (_, item) = request(address, "GET", &format!("{movies}/{mid}"), "").await;
    assert_eq!(item["statistics"]["file_count"], 1);
    assert_eq!(item["statistics"]["size_on_disk"], 777);
    assert!(item["statistics"]["episode_count"].is_null());
    conn.execute(
        "UPDATE file_metadata SET size=NULL WHERE movie_file_id=10",
        (),
    )
    .await?;
    let (_, item) = request(address, "GET", &format!("{movies}/{mid}"), "").await;
    assert!(item["statistics"]["size_on_disk"].is_null());
    assert_eq!(item["statistics"]["known_size_file_count"], 0);
    let (status, updated) = request(address,"PUT",&format!("{movies}/bulk"),&json!({"items":[{"id":mid,"patch":{"minimum_availability":null}},{"id":second_id,"patch":{"minimum_availability":"in_cinemas"}}]}).to_string()).await;
    assert_eq!(status, 200, "{updated}");
    assert!(updated[0]["settings"]["minimum_availability"].is_null());
    assert_eq!(updated[1]["settings"]["minimum_availability"], "in_cinemas");
    for (route, payload) in [
        (tv, json!({"title":"Bad","tvdb_id":200,"path":"relative"})),
        (tv, json!({"title":"Bad","tvdb_id":200,"path":"/a/../b"})),
        (tv, json!({"title":"Bad","tmdb_id":200,"path":"/bad-tv"})),
        (
            movies,
            json!({"title":"Bad","tvdb_id":200,"path":"/bad-movie"}),
        ),
        (
            movies,
            json!({"metadata_id":50,"title":"Overwrite","path":"/bad-adoption"}),
        ),
    ] {
        let (status, error) = request(address, "POST", route, &payload.to_string()).await;
        assert_eq!(status, 400, "{error}");
    }
    // A failure during season propagation must restore both season and already-updated leaves.
    conn.execute_batch("CREATE TRIGGER fail_episode_monitor BEFORE UPDATE OF monitored ON episodes WHEN NEW.id=2 BEGIN SELECT RAISE(ABORT,'synthetic leaf failure'); END;").await?;
    let (status, _) = request(
        address,
        "PUT",
        &format!("{tv}/{sid}"),
        &json!({"quality_profile_id":1,"seasons":[{"number":1,"monitored":false}]}).to_string(),
    )
    .await;
    assert_eq!(status, 500);
    let (_, item) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert!(item["settings"]["quality_profile_id"].is_null());
    assert_eq!(item["seasons"][1]["monitored"], true);
    assert_eq!(
        conn.query("SELECT monitored FROM episodes WHERE id=1", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        1
    );
    conn.execute_batch("DROP TRIGGER fail_episode_monitor;")
        .await?;
    let csv = (1..=201)
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        request(address, "GET", &format!("{tv}?ids={csv}"), "")
            .await
            .0,
        400
    );
    let huge = json!({"title":"x".repeat(256 * 1024),"tvdb_id":200,"path":"/large"}).to_string();
    assert_eq!(request(address, "POST", tv, &huge).await.0, 413);
    assert_eq!(
        request(address, "GET", &format!("{movies}/999999"), "")
            .await
            .0,
        404
    );
    let (_, legacy) = request(address, "GET", "/api/v1/series", "").await;
    assert_eq!(legacy.as_array().unwrap().len(), 1);
    assert_eq!(legacy[0]["title"], "Show");
    // Imported roots may retain a trailing slash; they still own descendant paths.
    conn.execute("UPDATE series SET path='/old-root/' WHERE id=?1", [sid])
        .await?;
    for (route, payload) in [
        (
            tv,
            json!({"title":"Nested TV","tvdb_id":700,"path":"/old-root/child"}),
        ),
        (
            movies,
            json!({"title":"Nested movie","tmdb_id":700,"path":"/old-root/child"}),
        ),
    ] {
        let (status, error) = request(address, "POST", route, &payload.to_string()).await;
        assert_eq!(status, 409, "{error}");
        assert_eq!(error["error"]["code"], "library_conflict");
    }
    let (_, before) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert_eq!(before["path"], "/old-root/");
    // An unsupported move flag must reject the entire otherwise-valid mutation.
    let (status, error) = request(
        address,
        "PUT",
        &format!("{tv}/{sid}?move_files=true"),
        &json!({"monitored":!before["monitored"].as_bool().unwrap()}).to_string(),
    )
    .await;
    assert_eq!(status, 400, "{error}");
    assert_eq!(error["error"]["code"], "invalid_request");
    let (_, after) = request(address, "GET", &format!("{tv}/{sid}"), "").await;
    assert_eq!(after["monitored"], before["monitored"]);
    assert_eq!(after["path"], "/old-root/");
    server.abort();
    let _ = server.await;
    drop(conn);
    drop(db);
    let reopened = Database::open_local(&db_path).await?;
    let c = reopened.connect().await?;
    assert_eq!(
        c.query("SELECT sum(monitored) FROM movies", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        0
    );
    assert_eq!(
        c.query("SELECT count(*) FROM episode_files", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        2
    );
    Ok(())
}
