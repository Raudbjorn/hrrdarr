use hrrdarr::{
    db::{Database, Error},
    import,
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
            std::env::temp_dir().join(format!("hrrdarr-import-api-{}", uuid::Uuid::new_v4()));
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
            serde_json::from_str(body).expect("import response must be JSON"),
        )
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn initial_import_http_both_domains_modes_and_preservation() -> Result<(), Error> {
    use std::os::unix::fs::{MetadataExt, symlink};
    let scratch = Sandbox::new();
    let db_path = scratch.0.join("library.db");
    let db = Arc::new(Database::open_local(&db_path).await?);
    let c = db.connect().await?;
    let tv_root = scratch.0.join("tv");
    let movie_root = scratch.0.join("movies");
    let incoming = scratch.0.join("incoming");
    for dir in [&tv_root, &movie_root, &incoming] {
        std::fs::create_dir(dir)?;
    }
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'TV',?1)",
        libsql::params![tv_root.to_str().unwrap()],
    )
    .await?;
    c.execute_batch("INSERT INTO seasons VALUES(1,1,1);")
        .await?;
    for id in 1..=8 {
        c.execute(
            "INSERT INTO episodes(id,series_id,season,number,title) VALUES(?1,1,1,?1,'Episode')",
            [id],
        )
        .await?;
        c.execute(
            "INSERT INTO movie_metadata(id,title) VALUES(?1,'Movie')",
            [id],
        )
        .await?;
        let root = movie_root.join(id.to_string());
        std::fs::create_dir(&root)?;
        c.execute(
            "INSERT INTO movies(id,metadata_id,path) VALUES(?1,?1,?2)",
            libsql::params![id, root.to_str().unwrap()],
        )
        .await?;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = import::router(db.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut completed = Vec::new();
    for media in ["episode", "movie"] {
        for (index, mode) in ["copy", "move", "hardlink"].iter().enumerate() {
            let id = index as i64 + 1;
            let source = incoming.join(format!("{media}-{mode}.mkv"));
            let destination = if media == "episode" {
                tv_root.join(format!("{mode}.mkv"))
            } else {
                movie_root.join(id.to_string()).join("file.mkv")
            };
            let bytes = format!("unique {media} {mode} bytes").into_bytes();
            std::fs::write(&source, &bytes)?;
            let source_inode = std::fs::metadata(&source)?.ino();
            let body = json!({"target":{"media_type":media,"id":id},"source":source,"destination":destination,"mode":mode});
            let (status, preview) =
                request(address, "POST", "/api/v1/imports", &body.to_string()).await;
            assert_eq!(status, 202, "{preview}");
            assert_eq!(preview["target"], body["target"]);
            assert_eq!(preview["status"], "preview");
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&source)?, bytes);
            let op = preview["id"].as_str().unwrap();
            let execute = format!("/api/v1/imports/{op}/execute");
            let (status, done) = request(address, "POST", &execute, "").await;
            assert_eq!(status, 200, "{done}");
            assert_eq!(done["status"], "complete");
            assert!(done["error_code"].is_null());
            assert_eq!(std::fs::read(&destination)?, bytes);
            if *mode == "move" {
                assert!(!source.exists());
            } else {
                assert_eq!(std::fs::read(&source)?, bytes);
            }
            if *mode == "hardlink" {
                assert_eq!(std::fs::metadata(&destination)?.ino(), source_inode);
            }
            let (status, retry) = request(address, "POST", &execute, "").await;
            assert_eq!(status, 200, "{retry}");
            assert_eq!(retry["id"], preview["id"]);
            let (_, read) = request(address, "GET", &format!("/api/v1/imports/{op}"), "").await;
            assert_eq!(read["status"], "complete");
            assert_eq!(read["target"], body["target"]);
            let row = c
                .query(
                    "SELECT media_type,size,destination FROM import_history WHERE operation_id=?1",
                    [op],
                )
                .await?
                .next()
                .await?
                .unwrap();
            assert_eq!(row.get::<String>(0)?, media);
            assert_eq!(row.get::<i64>(1)?, bytes.len() as i64);
            assert_eq!(row.get::<String>(2)?, destination.to_str().unwrap());
            // Existing associations are rejected; this proves refusal safety, not replacement support.
            let replacement = incoming.join(format!("replacement-{media}-{mode}"));
            std::fs::write(&replacement, b"replacement")?;
            let mut rejected = body.clone();
            rejected["source"] = json!(replacement);
            let (status, error) =
                request(address, "POST", "/api/v1/imports", &rejected.to_string()).await;
            assert_eq!(status, 409, "{error}");
            assert_eq!(std::fs::read(&destination)?, bytes);
            assert_eq!(std::fs::read(&replacement)?, b"replacement");
            completed.push(op.to_owned());
        }
    }
    assert_eq!(
        c.query("SELECT count(*) FROM import_history", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        6
    );
    assert_eq!(
        c.query(
            "SELECT count(*) FROM episodes WHERE episode_file_id IS NOT NULL",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        3
    );
    assert_eq!(
        c.query("SELECT count(*) FROM movie_files", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        3
    );
    // Managed media must never be used as a move source for another unassociated target.
    let protected_source = tv_root.join("copy.mkv");
    let protected_bytes = std::fs::read(&protected_source)?;
    let managed = json!({"target":{"media_type":"movie","id":4},"source":protected_source,"destination":movie_root.join("4/new.mkv"),"mode":"move"});
    let (status, error) = request(address, "POST", "/api/v1/imports", &managed.to_string()).await;
    assert_eq!(status, 409, "{error}");
    assert_eq!(error["error"]["code"], "source_is_library");
    assert_eq!(std::fs::read(&protected_source)?, protected_bytes);
    for alias in [
        protected_source.to_str().unwrap().replace("/tv/", "//tv/"),
        protected_source.to_str().unwrap().replace("/tv/", "/tv/./"),
    ] {
        let mut aliased = managed.clone();
        aliased["source"] = json!(alias);
        let (status, error) =
            request(address, "POST", "/api/v1/imports", &aliased.to_string()).await;
        assert_eq!(status, 400, "{error}");
        assert_eq!(std::fs::read(&protected_source)?, protected_bytes);
    }
    // Two simultaneous executions can serialize or return busy, but cannot duplicate effects.
    let concurrent_source = incoming.join("concurrent.mkv");
    std::fs::write(&concurrent_source, b"concurrent bytes")?;
    let concurrent = json!({"target":{"media_type":"episode","id":6},"source":concurrent_source,"destination":tv_root.join("concurrent.mkv"),"mode":"copy"});
    let (status, preview) =
        request(address, "POST", "/api/v1/imports", &concurrent.to_string()).await;
    assert_eq!(status, 202, "{preview}");
    let execute = format!(
        "/api/v1/imports/{}/execute",
        preview["id"].as_str().unwrap()
    );
    let (first, second) = tokio::join!(
        request(address, "POST", &execute, ""),
        request(address, "POST", &execute, "")
    );
    for (status, response) in [first, second] {
        if status == 409 {
            assert_eq!(response["error"]["code"], "import_busy");
        } else {
            assert_eq!(status, 200, "{response}");
        }
    }
    assert_eq!(request(address, "POST", &execute, "").await.0, 200);
    assert_eq!(
        c.query("SELECT count(*) FROM import_history WHERE episode_id=6", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
        1
    );
    let source = incoming.join("untouched.mkv");
    std::fs::write(&source, b"original")?;
    let destination = tv_root.join("occupied.mkv");
    std::fs::write(&destination, b"keep")?;
    let base = json!({"target":{"media_type":"episode","id":4},"source":source,"destination":destination,"mode":"copy"});
    assert_eq!(
        request(address, "POST", "/api/v1/imports", &base.to_string())
            .await
            .0,
        409
    );
    assert_eq!(std::fs::read(&destination)?, b"keep");
    for patch in [
        json!({"mode":"delete"}),
        json!({"target":{"media_type":"movie","id":0}}),
        json!({"unknown":true}),
    ] {
        let mut bad = base.clone();
        for (key, value) in patch.as_object().unwrap() {
            bad[key] = value.clone();
        }
        assert_eq!(
            request(address, "POST", "/api/v1/imports", &bad.to_string())
                .await
                .0,
            400
        );
    }
    let mut outside = base.clone();
    outside["destination"] = json!(scratch.0.join("escape.mkv"));
    assert_eq!(
        request(address, "POST", "/api/v1/imports", &outside.to_string())
            .await
            .0,
        409
    );
    let alias = incoming.join("symlink.mkv");
    symlink(&source, &alias)?;
    let mut linked = base.clone();
    linked["source"] = json!(alias);
    linked["destination"] = json!(tv_root.join("linked.mkv"));
    assert_eq!(
        request(address, "POST", "/api/v1/imports", &linked.to_string())
            .await
            .0,
        409
    );
    // A legacy-shaped new preview gets a journal; an old stored preview cannot execute.
    let destination = tv_root.join("changed.mkv");
    let legacy = json!({"episode_id":4,"source":source,"destination":destination,"mode":"move"});
    let (status, preview) = request(address, "POST", "/api/v1/imports", &legacy.to_string()).await;
    assert_eq!(status, 202, "{preview}");
    std::fs::write(&source, b"changed after preview")?;
    let (status, error) = request(
        address,
        "POST",
        &format!(
            "/api/v1/imports/{}/execute",
            preview["id"].as_str().unwrap()
        ),
        "",
    )
    .await;
    assert_eq!(status, 409, "{error}");
    assert_eq!(std::fs::read(&source)?, b"changed after preview");
    assert!(!destination.exists());
    let legacy_id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES(?1,'episode',5,?2,'copy',?3,'preview','legacy')",libsql::params![legacy_id.clone(),source.to_str().unwrap(),tv_root.join("legacy.mkv").to_str().unwrap()]).await?;
    let (status, error) = request(
        address,
        "POST",
        &format!("/api/v1/imports/{legacy_id}/execute"),
        "",
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(error["error"]["code"], "preview_required");
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let reopened = Arc::new(Database::open_local(&db_path).await?);
    for op in completed {
        assert_eq!(
            import::status(reopened.clone(), &op).await.unwrap().status,
            "complete"
        );
    }
    assert_eq!(std::fs::read(source)?, b"changed after preview");
    Ok(())
}
