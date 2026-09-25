use hrrdarr::{
    db::{Database, Error},
    root_folders,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-roots-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap()
    }
}
async fn scalar(c: &libsql::Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let response = client
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let code = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        code,
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
#[tokio::test]
async fn scoped_root_http_observations_and_config_only_deletion() -> Result<(), Error> {
    let s = Sandbox::new();
    let root = s.0.join("media");
    std::fs::create_dir(&root)?;
    for name in [
        "Known TV",
        "Known Movie",
        "Other",
        ".grab",
        ".hidden",
        "lost+found",
    ] {
        std::fs::create_dir(root.join(name))?;
    }
    std::fs::write(root.join("Other/file.mkv"), b"untouched")?;
    std::os::unix::fs::symlink(&s.0, root.join("outside"))?;
    let db = Arc::new(Database::open_local(s.0.join("db")).await?);
    let c = db.connect().await?;
    c.execute(
        "INSERT INTO series(title,path) VALUES('TV',?)",
        [format!("{}//Known TV/", root.display())],
    )
    .await?;
    c.execute("INSERT INTO movie_metadata(id,title) VALUES(1,'Film')", ())
        .await?;
    c.execute(
        "INSERT INTO movies(metadata_id,path) VALUES(1,?)",
        [root.join("Known Movie").to_str().unwrap()],
    )
    .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = root_folders::router(db.clone()).merge(hrrdarr::library::router(db.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut ids = Vec::new();
    for domain in ["tv", "movies"] {
        let (status, result) = request(
            &client,
            &base,
            "POST",
            &format!("/{domain}/root-folders"),
            json!({"path":format!("{}//",root.display())}),
        )
        .await;
        assert_eq!(status, 201, "{result}");
        assert_eq!(result["path"], root.to_str().unwrap());
        assert_eq!(result["accessible"], true);
        assert_eq!(result["writable"], true);
        assert_eq!(result["observation"], "available");
        assert!(result["total_space"].as_u64().unwrap() > 0);
        let names = result["unmapped_folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(names.contains(&"Other"));
        assert!(!names.contains(&".grab"));
        assert!(!names.contains(&"outside"));
        assert!(!names.contains(&"lost+found"));
        assert!(!names.contains(&if domain == "tv" {
            "Known TV"
        } else {
            "Known Movie"
        }));
        assert_eq!(names.contains(&".hidden"), domain == "tv");
        let id = result["id"].as_i64().unwrap();
        ids.push(id);
        assert_eq!(
            request(
                &client,
                &base,
                "POST",
                &format!("/{domain}/root-folders"),
                json!({"path":root})
            )
            .await
            .0,
            409
        );
        let (status, page) = request(
            &client,
            &base,
            "GET",
            &format!("/{domain}/root-folders?limit=1&offset=0"),
            Value::Null,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["id"], id);
    }
    assert_ne!(ids[0], ids[1]);
    assert_eq!(
        request(
            &client,
            &base,
            "GET",
            &format!("/movies/root-folders/{}", ids[0]),
            Value::Null
        )
        .await
        .0,
        404
    );
    for path in [
        "/tv/root-folders?unknown=1",
        "/tv/root-folders?limit=0",
        "/tv/root-folders/0",
        "/tv/root-folders/abc",
        "/tv/root-folders/1?unknown=1",
    ] {
        assert_eq!(
            request(&client, &base, "GET", path, Value::Null).await.0,
            400,
            "{path}"
        );
    }
    for path in ["/", "relative", "/tmp/../elsewhere"] {
        assert_eq!(
            request(
                &client,
                &base,
                "POST",
                "/tv/root-folders",
                json!({"path":path})
            )
            .await
            .0,
            400
        );
    }
    let alias = s.0.join("alias");
    std::os::unix::fs::symlink(&root, &alias)?;
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/tv/root-folders",
            json!({"path":alias})
        )
        .await
        .0,
        422
    );
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/tv/root-folders",
            json!({"path":s.0.join("missing")})
        )
        .await
        .0,
        422
    );
    let readonly = s.0.join("readonly");
    std::fs::create_dir(&readonly)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o500))?;
    // Root/CAP_DAC_OVERRIDE can write despite mode bits; assert denial only where the OS denies it.
    let probe = readonly.join("test-permission");
    if std::fs::File::create(&probe).is_err() {
        assert_eq!(
            request(
                &client,
                &base,
                "POST",
                "/tv/root-folders",
                json!({"path":readonly})
            )
            .await
            .0,
            422
        );
    } else {
        std::fs::remove_file(probe)?;
    }
    std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o700))?;
    // Escaped JSON can exceed the response bound even below the raw scan-memory budget.
    let escaped = s.0.join("escaped");
    std::fs::create_dir(&escaped)?;
    for n in 0..400 {
        std::fs::create_dir(escaped.join(format!("{}-{n}", "\u{1}".repeat(200))))?;
    }
    assert_eq!(
        request(
            &client,
            &base,
            "POST",
            "/tv/root-folders",
            json!({"path":escaped})
        )
        .await
        .0,
        413
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM root_folders").await, 2);
    // Unavailable observations remain readable/removable and never masquerade as an empty scan.
    let moved = s.0.join("moved");
    std::fs::rename(&root, &moved)?;
    let (status, result) = request(
        &client,
        &base,
        "GET",
        &format!("/tv/root-folders/{}", ids[0]),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(result["accessible"], false);
    assert_eq!(result["unmapped_folders"], Value::Null);
    assert_eq!(result["free_space"], Value::Null);
    assert_eq!(
        request(
            &client,
            &base,
            "DELETE",
            &format!("/tv/root-folders/{}", ids[0]),
            Value::Null
        )
        .await
        .0,
        204
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM root_folders WHERE media_type='movies'"
        )
        .await,
        1
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM series").await, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM movies").await, 1);
    assert_eq!(std::fs::read(moved.join("Other/file.mkv"))?, b"untouched");
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let db = Database::open_local(s.0.join("db")).await?;
    assert_eq!(
        scalar(&db.connect().await?, "SELECT count(*) FROM root_folders").await,
        1
    );
    Ok(())
}
async fn snapshot(path: &std::path::Path, app: Application, root: &str) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(if matches!(app,Application::Sonarr){"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"}else{"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"}).await.unwrap();
    c.execute_batch("CREATE TABLE RootFolders(Id INTEGER,Path TEXT,Unused TEXT);")
        .await
        .unwrap();
    c.execute("INSERT INTO RootFolders VALUES(1,?,'private')", [root])
        .await
        .unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
#[tokio::test]
async fn root_snapshot_mapping_is_offline_atomic_and_replay_safe() -> Result<(), Error> {
    let s = Sandbox::new();
    let absent = s.0.join("never-created");
    let tvpath = s.0.join("tv.db");
    let tv = snapshot(&tvpath, Application::Sonarr, absent.to_str().unwrap()).await;
    let movie = snapshot(
        &s.0.join("movie.db"),
        Application::Radarr,
        absent.to_str().unwrap(),
    )
    .await;
    let dest = s.0.join("db");
    let db = Database::open_local(&dest).await?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600))?;
    let c = db.connect().await?;
    let report = snapshots::import(&db, Application::Sonarr, tv.clone(), true).await?;
    assert!(!report.applied);
    assert_eq!(report.mapped, 1);
    assert_eq!(scalar(&c, "SELECT count(*) FROM root_folders").await, 0);
    for (app, bytes) in [
        (Application::Sonarr, tv.clone()),
        (Application::Radarr, movie),
    ] {
        let report = snapshots::import(&db, app, bytes, false).await?;
        assert!(report.applied);
        assert_eq!(report.mapped, 1);
        assert!(
            report
                .unsupported
                .iter()
                .any(|u| u.table == "RootFolders" && u.columns == ["Unused"])
        );
    }
    assert_eq!(scalar(&c, "SELECT count(*) FROM root_folders").await, 2);
    assert!(!absent.exists());
    let legacy_path = s.0.join("legacy.db");
    snapshot(&legacy_path, Application::Radarr, absent.to_str().unwrap()).await;
    let raw = libsql::Builder::new_local(&legacy_path).build().await?;
    let rc = raw.connect()?;
    rc.execute_batch("UPDATE VersionInfo SET Version=206;DROP TABLE Movies;DROP TABLE MovieMetadata;CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);").await?;
    drop(rc);
    drop(raw);
    let legacy =
        snapshots::import(&db, Application::Radarr, std::fs::read(legacy_path)?, false).await?;
    assert!(
        legacy.applied && legacy.conflicts == 0 && legacy.duplicates == 1 && legacy.mapped == 0
    );

    let report = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(report.applied && report.conflicts == 0 && report.duplicates == 1);
    c.execute(
        "UPDATE root_folders SET path=path||'-edited' WHERE media_type='tv'",
        (),
    )
    .await?;
    let report = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!report.applied && report.conflicts == 1);
    c.execute("DELETE FROM root_folders WHERE media_type='tv'", ())
        .await?;
    let report = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!report.applied && report.conflicts == 1);
    // AUTOINCREMENT prevents a deleted mapped root identity being silently reused.
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('tv',?)",
        [absent.to_str().unwrap()],
    )
    .await?;
    let report = snapshots::import(&db, Application::Sonarr, tv.clone(), false).await?;
    assert!(!report.applied && report.conflicts == 1);
    let failure_path = s.0.join("failure.db");
    let failure = Database::open_local(&failure_path).await?;
    std::fs::set_permissions(&failure_path, std::fs::Permissions::from_mode(0o600))?;
    let fc = failure.connect().await?;
    fc.execute_batch("CREATE TRIGGER late_root_failure BEFORE INSERT ON snapshot_records BEGIN SELECT RAISE(ABORT,'late failure');END;").await?;
    assert!(
        snapshots::import(&failure, Application::Sonarr, tv.clone(), false)
            .await
            .is_err()
    );
    for table in [
        "root_folders",
        "snapshot_imports",
        "snapshot_mappings",
        "snapshot_records",
    ] {
        assert_eq!(
            scalar(&fc, &format!("SELECT count(*) FROM {table}")).await,
            0
        )
    }
    assert_eq!(std::fs::read(tvpath)?, tv);
    Ok(())
}
