use hrrdarr::{
    db::{Database, Error, MediaTarget},
    import::{ImportInput, ManualImportRequest, Mode},
    remote_paths,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-mapping-{}", uuid::Uuid::new_v4()));
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
async fn call(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let r = client
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        status,
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
#[tokio::test]
async fn mappings_crud_provider_preview_and_safe_manual_boundary() -> Result<(), Error> {
    let s = Sandbox::new();
    let db = Arc::new(Database::open_local(s.0.join("db")).await?);
    let c = db.connect().await?;
    let tv = s.0.join("tv-in");
    let movie = s.0.join("movie-in");
    let tvlib = s.0.join("tv");
    let movielib = s.0.join("movies");
    for p in [&tv, &movie, &tvlib, &movielib] {
        std::fs::create_dir(p)?;
    }
    std::fs::write(tv.join("file.mkv"), b"tv media")?;
    std::fs::write(movie.join("file.mkv"), b"movie media")?;
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'TV',?)",
        [tvlib.to_str().unwrap()],
    )
    .await?;
    c.execute_batch("INSERT INTO seasons(series_id,number) VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title,monitored) VALUES(1,1,1,1,'Episode',1);INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');").await?;
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(1,1,?)",
        [movielib.to_str().unwrap()],
    )
    .await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = remote_paths::router(db.clone())
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::providers::router(db.clone(), None));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut ids = Vec::new();
    for (domain, local) in [("tv", &tv), ("movies", &movie)] {
        let body =
            json!({"host":"CLIENT.Example","remote_path":r"C:\Ä\Downloads\","local_path":local});
        let (status, m) = call(
            &client,
            &base,
            "POST",
            &format!("/{domain}/remote-path-mappings"),
            body.clone(),
        )
        .await;
        assert_eq!(status, 201, "{m}");
        ids.push(m["id"].as_i64().unwrap());
        assert_eq!(m["host"], "client.example");
        assert_eq!(
            call(
                &client,
                &base,
                "POST",
                &format!("/{domain}/remote-path-mappings"),
                json!({"host":"client.example","remote_path":"c:/ä/downloads","local_path":local})
            )
            .await
            .0,
            409
        );
        let (status,res)=call(&client,&base,"POST",&format!("/{domain}/remote-path-mappings/resolve"),json!({"host":"ClIent.example","path":"c:/ä/downloads/file.mkv","direction":"remote_to_local"})).await;
        assert_eq!(status, 200);
        assert_eq!(res["output"], local.join("file.mkv").to_str().unwrap());
        assert_eq!(res["mapping_id"], m["id"]);
        let (_,reverse)=call(&client,&base,"POST",&format!("/{domain}/remote-path-mappings/resolve"),json!({"host":"client.example","path":local.join("file.mkv"),"direction":"local_to_remote"})).await;
        assert_eq!(reverse["output"], r"C:\Ä\Downloads\file.mkv");
        let (_,alias)=call(&client,&base,"POST",&format!("/{domain}/remote-path-mappings/resolve"),json!({"host":"client.example","path":format!("/{}/file.mkv",local.to_str().unwrap().replace("/","//")),"direction":"local_to_remote"})).await;
        assert_eq!(alias["output"], reverse["output"]);
        let (_, page) = call(
            &client,
            &base,
            "GET",
            &format!("/{domain}/remote-path-mappings?limit=1"),
            Value::Null,
        )
        .await;
        assert_eq!(page["total"], 1);
    }
    assert_ne!(ids[0], ids[1]);
    assert_eq!(
        call(
            &client,
            &base,
            "GET",
            &format!("/movies/remote-path-mappings/{}", ids[0]),
            Value::Null
        )
        .await
        .0,
        404
    );
    for domain in ["tv", "movies"] {
        for path in ["/", "////"] {
            let (status, response) = call(
                &client,
                &base,
                "POST",
                &format!("/{domain}/remote-path-mappings/resolve"),
                json!({"host":"client.example","path":path,"direction":"local_to_remote"}),
            )
            .await;
            assert_eq!(status, 200, "{response}");
            assert_eq!(response["output"], path);
            assert!(response["mapping_id"].is_null());
        }
    }
    for (path, host) in [
        ("c:/ä/downloads2/file.mkv", "client.example"),
        ("c:/ä/downloads/file.mkv", "other.example"),
        ("", "client.example"),
    ] {
        let (_, r) = call(
            &client,
            &base,
            "POST",
            "/tv/remote-path-mappings/resolve",
            json!({"host":host,"path":path,"direction":"remote_to_local"}),
        )
        .await;
        assert_eq!(r["output"], path);
        assert!(r["mapping_id"].is_null())
    }
    // Nested mapping deliberately follows the older ID, not longest-prefix precedence.
    std::fs::create_dir(tv.join("nested"))?;
    let (_, nested) = call(
        &client,
        &base,
        "POST",
        "/tv/remote-path-mappings",
        json!({"host":"client.example","remote_path":r"C:\Ä\Downloads\nested","local_path":movie}),
    )
    .await;
    let (_,r)=call(&client,&base,"POST","/tv/remote-path-mappings/resolve",json!({"host":"client.example","path":r"C:\Ä\Downloads\nested\file","direction":"remote_to_local"})).await;
    assert_eq!(r["mapping_id"], ids[0]);
    assert_eq!(r["output"], tv.join("nested/file").to_str().unwrap());
    let scope = |category: &str| json!({"category":category,"imported_category":null,"recent_priority":0,"older_priority":0});
    let (status,provider)=call(&client,&base,"POST","/providers",json!({"name":"Shared client","enabled":false,"priority":1,"settings":{"implementation":"qbittorrent","endpoint":"http://client.example:8080","tv":scope("tv"),"movies":scope("movies")}})).await;
    assert_eq!(status, 201, "{provider}");
    let pid = provider["id"].as_str().unwrap();
    assert_eq!(
        call(
            &client,
            &base,
            "POST",
            &format!("/providers/{pid}/path-preview?unknown=true"),
            json!({"media_type":"tv","remote_path":"/remote"})
        )
        .await
        .0,
        400
    );
    for (domain, target, library) in [
        ("tv", MediaTarget::Episode(1), &tvlib),
        ("movies", MediaTarget::Movie(1), &movielib),
    ] {
        let (status, r) = call(
            &client,
            &base,
            "POST",
            &format!("/providers/{pid}/path-preview"),
            json!({"media_type":domain,"remote_path":r"c:\ä\downloads\file.mkv"}),
        )
        .await;
        assert_eq!(status, 200, "{r}");
        assert_eq!(r["provider_revision"], 1);
        assert_eq!(r["resolution"]["lexical_only"], true);
        let op = hrrdarr::import::preview(
            db.clone(),
            ImportInput::Typed(ManualImportRequest {
                target,
                source: r["resolution"]["output"].as_str().unwrap().into(),
                mode: Mode::Copy,
                destination: library.join("file.mkv").to_str().unwrap().into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(op.status, "preview");
    }
    std::os::unix::fs::symlink(movie.join("file.mkv"), tv.join("unsafe.mkv"))?;
    let (_, r) = call(
        &client,
        &base,
        "POST",
        &format!("/providers/{pid}/path-preview"),
        json!({"media_type":"tv","remote_path":r"c:\ä\downloads\unsafe.mkv"}),
    )
    .await;
    assert!(
        hrrdarr::import::preview(
            db.clone(),
            ImportInput::Typed(ManualImportRequest {
                target: MediaTarget::Episode(1),
                source: r["resolution"]["output"].as_str().unwrap().into(),
                mode: Mode::Copy,
                destination: tvlib.join("unsafe.mkv").to_str().unwrap().into()
            })
        )
        .await
        .is_err()
    );
    let update = json!({"revision":1,"host":"client.example","remote_path":r"C:\Ä\Downloads","local_path":tv});
    assert_eq!(
        call(
            &client,
            &base,
            "PUT",
            &format!("/tv/remote-path-mappings/{}", ids[0]),
            update.clone()
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &client,
            &base,
            "PUT",
            &format!("/tv/remote-path-mappings/{}", ids[0]),
            update
        )
        .await
        .0,
        409
    );
    for path in [
        "/tv/remote-path-mappings?extra=1",
        "/tv/remote-path-mappings?limit=0",
        "/tv/remote-path-mappings/0",
    ] {
        assert_eq!(call(&client, &base, "GET", path, Value::Null).await.0, 400)
    }
    for local in ["/", "/proc/test", "/usr/bin/test"] {
        assert_eq!(
            call(
                &client,
                &base,
                "POST",
                "/tv/remote-path-mappings",
                json!({"host":"client","remote_path":"/data","local_path":local})
            )
            .await
            .0,
            400
        )
    }
    assert_eq!(
        call(
            &client,
            &base,
            "POST",
            "/tv/remote-path-mappings",
            json!({"host":"client","remote_path":"/data","local_path":s.0.join("absent")})
        )
        .await
        .0,
        422
    );
    assert_eq!(
        call(
            &client,
            &base,
            "DELETE",
            &format!("/tv/remote-path-mappings/{}?revision=1", ids[0]),
            Value::Null
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            &client,
            &base,
            "DELETE",
            &format!("/tv/remote-path-mappings/{}?revision=2", ids[0]),
            Value::Null
        )
        .await
        .0,
        204
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM remote_path_mappings WHERE media_type='movies'"
        )
        .await,
        1
    );
    assert_eq!(std::fs::read(tv.join("file.mkv"))?, b"tv media");
    assert!(nested["id"].as_i64().is_some());
    for (domain, remote, child) in [
        ("tv", "relative/downloads", "relative/downloads/file.mkv"),
        (
            "movies",
            r"relative\downloads",
            r"relative\downloads\file.mkv",
        ),
        ("tv", r"relative\", r"relative\file.mkv"),
    ] {
        let (status, m) = call(
            &client,
            &base,
            "POST",
            &format!("/{domain}/remote-path-mappings"),
            json!({"host":"relative.example","remote_path":remote,"local_path":tv}),
        )
        .await;
        assert_eq!(status, 201, "{m}");
        let (_, forward) = call(
            &client,
            &base,
            "POST",
            &format!("/{domain}/remote-path-mappings/resolve"),
            json!({"host":"relative.example","path":child,"direction":"remote_to_local"}),
        )
        .await;
        assert_eq!(forward["output"], child);
        assert!(forward["mapping_id"].is_null());
        let (_,reverse)=call(&client,&base,"POST",&format!("/{domain}/remote-path-mappings/resolve"),json!({"host":"relative.example","path":tv.join("file.mkv"),"direction":"local_to_remote"})).await;
        assert_eq!(reverse["output"], child, "{reverse}");
        assert_eq!(reverse["mapping_id"], m["id"]);
        assert_eq!(
            call(
                &client,
                &base,
                "DELETE",
                &format!("/{domain}/remote-path-mappings/{}?revision=1", m["id"]),
                Value::Null
            )
            .await
            .0,
            204
        );
    }
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let db = Database::open_local(s.0.join("db")).await?;
    assert_eq!(
        scalar(
            &db.connect().await?,
            "SELECT count(*) FROM remote_path_mappings"
        )
        .await,
        2
    );
    Ok(())
}

async fn snapshot(path: &std::path::Path, app: Application, root: &str) -> Vec<u8> {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(if matches!(app,Application::Sonarr){"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"}else{"CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(242);CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"}).await.unwrap();
    c.execute_batch("CREATE TABLE RemotePathMappings(Id INTEGER,Host TEXT,RemotePath TEXT,LocalPath TEXT,Unused TEXT);")
        .await
        .unwrap();
    c.execute("INSERT INTO RemotePathMappings VALUES(20,'client.example','/remote/nested',?,'private'),(10,'client.example','/remote',?,'private')", [format!("{root}/nested"),root.to_string()])
        .await
        .unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
async fn private_db(path: &Path) -> Database {
    use std::os::unix::fs::PermissionsExt;
    let db = Database::open_local(path).await.unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    db
}
#[tokio::test]
async fn snapshot_mapping_order_domains_replay_and_rollback() -> Result<(), Error> {
    let s = Sandbox::new();
    let source = s.0.join("source");
    let bytes = snapshot(&source, Application::Sonarr, "/offline/local").await;
    let db = private_db(&s.0.join("dest")).await;
    let c = db.connect().await?;
    let r = snapshots::import(&db, Application::Sonarr, bytes.clone(), true).await?;
    assert!(!r.applied && r.mapped == 2 && r.conflicts == 0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM remote_path_mappings").await,
        0
    );
    for preview in [false, false] {
        let r = snapshots::import(&db, Application::Sonarr, bytes.clone(), preview).await?;
        assert!(r.applied && r.conflicts == 0);
    }
    assert_eq!(
        scalar(
            &c,
            "SELECT id FROM remote_path_mappings WHERE remote_path='/remote'"
        )
        .await,
        1
    );
    let movies = snapshot(&s.0.join("movies"), Application::Radarr, "/offline/local").await;
    let r = snapshots::import(&db, Application::Radarr, movies, false).await?;
    assert!(r.applied && r.mapped == 2 && r.conflicts == 0);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM remote_path_mappings").await,
        4
    );
    c.execute(
        "UPDATE remote_path_mappings SET revision=revision+1,local_path='/edited' WHERE id=1",
        (),
    )
    .await?;
    let r = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!r.applied && r.conflicts > 0);
    c.execute("DELETE FROM remote_path_mappings WHERE id=1", ())
        .await?;
    c.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','client.example','/remote','posix','/remote','/offline/local')",()).await?;
    let r = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await?;
    assert!(!r.applied && r.conflicts > 0);
    // Existing reversed destination precedence must not silently change source policy.
    for reverse_only in [false, true] {
        let path = s.0.join(format!("order-{reverse_only}"));
        let order = private_db(&path).await;
        let oc = order.connect().await?;
        let sourcepath = s.0.join(format!("order-source-{reverse_only}"));
        let mut ordered = snapshot(&sourcepath, Application::Sonarr, "/offline/local").await;
        if reverse_only {
            let raw = libsql::Builder::new_local(&sourcepath).build().await?;
            raw.connect()?
                .execute(
                    "UPDATE RemotePathMappings SET RemotePath='/independent' WHERE Id=20",
                    (),
                )
                .await?;
            drop(raw);
            ordered = std::fs::read(&sourcepath)?;
        }
        let remote = if reverse_only {
            "/independent"
        } else {
            "/remote/nested"
        };
        oc.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','client.example',?,'posix',?,'/offline/local/nested')",[remote,remote]).await?;
        let r = snapshots::import(&order, Application::Sonarr, ordered, false).await?;
        assert!(!r.applied && r.conflicts > 0);
        assert_eq!(
            scalar(&oc, "SELECT count(*) FROM remote_path_mappings").await,
            1
        );
        assert_eq!(
            scalar(&oc, "SELECT count(*) FROM snapshot_imports").await,
            0
        );
    }
    let compatible = private_db(&s.0.join("compatible")).await;
    let cc = compatible.connect().await?;
    cc.execute("INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','client.example','/distinct','posix','/distinct','/separate')",()).await?;
    let r = snapshots::import(&compatible, Application::Sonarr, bytes.clone(), false).await?;
    assert!(r.applied && r.conflicts == 0 && r.mapped == 2);
    // A changed archive fingerprint reconciles exact settings without new native IDs.
    let fresh_path = s.0.join("new-backup");
    snapshot(&fresh_path, Application::Sonarr, "/offline/local").await;
    let raw = libsql::Builder::new_local(&fresh_path).build().await?;
    raw.connect()?
        .execute("UPDATE RemotePathMappings SET Unused='new backup'", ())
        .await?;
    drop(raw);
    let r = snapshots::import(
        &compatible,
        Application::Sonarr,
        std::fs::read(fresh_path)?,
        false,
    )
    .await?;
    assert!(r.applied && r.conflicts == 0 && r.mapped == 0 && r.duplicates == 2);
    assert_eq!(
        scalar(&cc, "SELECT count(*) FROM remote_path_mappings").await,
        3
    );
    let failed = private_db(&s.0.join("failed")).await;
    let fc = failed.connect().await?;
    fc.execute_batch("CREATE TRIGGER fail_archive BEFORE INSERT ON snapshot_records BEGIN SELECT RAISE(ABORT,'failure');END;").await?;
    assert!(
        snapshots::import(&failed, Application::Sonarr, bytes.clone(), false)
            .await
            .is_err()
    );
    for table in [
        "remote_path_mappings",
        "snapshot_imports",
        "snapshot_records",
        "snapshot_mappings",
    ] {
        assert_eq!(
            scalar(&fc, &format!("SELECT count(*) FROM {table}")).await,
            0
        );
    }
    assert_eq!(std::fs::read(source)?, bytes);
    Ok(())
}
