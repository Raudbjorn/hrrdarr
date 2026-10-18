//! Crash-injection and cross-device recovery tests for explicit deletion. The hook simulates
//! process death at a chosen cut point for one owned scratch path; restart recovery then runs
//! through the real command-worker startup.
use super::*;
use std::{os::unix::fs::MetadataExt, path::PathBuf, sync::Mutex, time::Duration};
use uuid::Uuid;

/// Paths (substring markers) whose deletion stops after the filesystem effect, before commit.
static CRASH_MARKERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub(super) fn crash_point(path: &str) -> bool {
    CRASH_MARKERS
        .lock()
        .map(|markers| markers.iter().any(|m| path.contains(m.as_str())))
        .unwrap_or(false)
}
fn arm(marker: &str) {
    CRASH_MARKERS.lock().unwrap().push(marker.to_owned());
}
fn disarm(marker: &str) {
    CRASH_MARKERS.lock().unwrap().retain(|m| m != marker);
}

struct Scratch(Vec<PathBuf>);
impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.0 {
            if let Err(error) = std::fs::remove_dir_all(path) {
                eprintln!("recycle test scratch cleanup failed: {error}");
            }
        }
    }
}
fn scratch_dir(base: &std::path::Path, tag: &str) -> PathBuf {
    let path = base.join(format!("hrrdarr-recycle-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    path
}
async fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap()
}
async fn text(c: &Connection, sql: &str) -> Option<String> {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Option<String>>(0)
        .unwrap()
}
/// TV series 1 and movie 1 each own file id 1 (colliding numeric ids across domains).
async fn library(db: &Database, root: &std::path::Path) -> (PathBuf, PathBuf) {
    let tv = root.join("tv");
    let movies = root.join("movies");
    std::fs::create_dir_all(tv.join("Show/Season 1")).unwrap();
    std::fs::create_dir_all(movies.join("Film (2020)")).unwrap();
    let episode = tv.join("Show/Season 1/Show.S01E01E02.mkv");
    let movie = movies.join("Film (2020)/Film.mkv");
    std::fs::write(&episode, b"episode bytes, shared by two episodes").unwrap();
    std::fs::write(&movie, b"movie bytes").unwrap();
    let c = db.connect().await.unwrap();
    let s = |p: &std::path::Path| p.to_str().unwrap().to_owned();
    c.execute(
        "INSERT INTO root_folders(media_type,path) VALUES('tv',?1),('movies',?2)",
        libsql::params![s(&tv), s(&movies)],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'Show',?)",
        [s(&tv.join("Show"))],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO seasons VALUES(1,1,1);")
        .await
        .unwrap();
    c.execute(
        "INSERT INTO episode_files(id,series_id,path) VALUES(1,1,?)",
        [s(&episode)],
    )
    .await
    .unwrap();
    c.execute_batch("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'One',1),(2,1,1,2,'Two',1);
        INSERT INTO file_metadata(media_type,episode_file_id,size) VALUES('tv',1,37);
        INSERT INTO movie_metadata(id,title) VALUES(1,'Film');").await.unwrap();
    c.execute(
        "INSERT INTO movies(id,metadata_id,path) VALUES(1,1,?)",
        [s(&movies.join("Film (2020)"))],
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,?)",
        [s(&movie)],
    )
    .await
    .unwrap();
    c.execute_batch(
        "INSERT INTO file_metadata(media_type,movie_file_id,size) VALUES('movies',1,11);",
    )
    .await
    .unwrap();
    (episode, movie)
}
async fn configure(db: &Database, media: MediaDomain, bin: &std::path::Path) {
    let c = db.connect().await.unwrap();
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    let revision = read(&tx, media).await.unwrap().revision;
    set(
        &tx,
        media,
        &Update {
            revision,
            path: Some(bin.to_str().unwrap().to_owned()),
            cleanup_days: 7,
        },
        crate::commands::now().unwrap(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn serve(db: Arc<Database>) -> (String, tokio::task::JoinHandle<()>) {
    let app = crate::media_files::router(db.clone()).merge(router(db));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, server)
}
async fn delete(base: &str, media: &str, id: i64, mode: &str) -> (u16, serde_json::Value) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    // Other lib tests share the process-wide file-effect permit; retry only its busy refusal.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let response = client
            .delete(format!("{base}/api/v1/{media}/files/{id}"))
            .header("content-type", "application/json")
            .body(serde_json::json!({ "mode": mode }).to_string())
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap_or_default();
        if status == 409
            && body["error"]["code"] == "import_busy"
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        return (status, body);
    }
}
async fn restart(db: Arc<Database>, path: &std::path::Path) -> Arc<Database> {
    drop(db);
    let db = Arc::new(Database::open_local(path).await.unwrap());
    let (_, refresh) = crate::providers::router_with_refresh(db.clone(), None);
    // Real worker startup runs deletion recovery before any command is claimed.
    let runtime = crate::commands::start(db.clone(), refresh).await.unwrap();
    runtime.shutdown().await;
    db
}

#[tokio::test]
async fn crash_between_move_and_commit_recovers_on_restart_in_both_domains() {
    let base = scratch_dir(&std::env::temp_dir(), "crash");
    let _scratch = Scratch(vec![base.clone()]);
    let dbpath = base.join("library.db");
    let mut db = Arc::new(Database::open_local(&dbpath).await.unwrap());
    let (episode, movie) = library(&db, &base).await;
    let bin = base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    configure(&db, MediaDomain::Tv, &bin).await;
    configure(&db, MediaDomain::Movies, &bin).await;
    let marker = base.file_name().unwrap().to_str().unwrap().to_owned();
    arm(&marker);
    let (url, server) = serve(db.clone()).await;
    let (status, body) = delete(&url, "tv", 1, "recycle").await;
    assert_eq!(status, 500, "{body}");
    let (status, body) = delete(&url, "movies", 1, "recycle").await;
    // The second request first reconciles the TV crash (same process, permit held), then crashes.
    assert_eq!(status, 500, "{body}");
    disarm(&marker);
    server.abort();
    let _ = server.await;
    let recycled_episode = bin.join("Show/Season 1/Show.S01E01E02.mkv");
    let recycled_movie = bin.join("Film (2020)/Film.mkv");
    assert!(!movie.exists());
    assert_eq!(std::fs::read(&recycled_movie).unwrap(), b"movie bytes");
    assert_eq!(
        std::fs::read(&recycled_episode).unwrap(),
        b"episode bytes, shared by two episodes"
    );
    assert!(!episode.exists());
    {
        let c = db.connect().await.unwrap();
        // TV was already rolled forward by the second request's recovery pass.
        assert_eq!(scalar(&c, "SELECT count(*) FROM episode_files").await, 0);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM episodes WHERE episode_file_id IS NOT NULL"
            )
            .await,
            0
        );
        // The movie crash is still pending: record and association intact, journal admitted.
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM movie_files WHERE id=1").await,
            1
        );
        assert_eq!(
            text(
                &c,
                "SELECT phase FROM file_deletions WHERE media_type='movies'"
            )
            .await,
            Some("admitted".into())
        );
    }
    db = restart(db, &dbpath).await;
    let c = db.connect().await.unwrap();
    assert_eq!(scalar(&c, "SELECT count(*) FROM movie_files").await, 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM file_metadata").await, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_deletions WHERE phase='complete'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_deletion_history WHERE media_type='episode'"
        )
        .await,
        2,
        "one event per detached episode"
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_deletion_history WHERE media_type='movie' AND movie_id=1"
        )
        .await,
        1
    );
    assert_eq!(std::fs::read(&recycled_movie).unwrap(), b"movie bytes");
    // Recovery is idempotent across another restart.
    drop(c);
    db = restart(db, &dbpath).await;
    let c = db.connect().await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM file_deletion_history").await,
        3
    );
}

#[tokio::test]
async fn crash_before_rename_aborts_without_touching_disk_or_database() {
    // Simulates a crash after admission but before the effect: the journal row is admitted and the
    // file is still at its source. Recovery must abort, never delete.
    let base = scratch_dir(&std::env::temp_dir(), "abort");
    let _scratch = Scratch(vec![base.clone()]);
    let dbpath = base.join("library.db");
    let mut db = Arc::new(Database::open_local(&dbpath).await.unwrap());
    let (_, movie) = library(&db, &base).await;
    let bin = base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    configure(&db, MediaDomain::Movies, &bin).await;
    let identity = fsops::observe(movie.to_str().unwrap()).unwrap().unwrap();
    {
        let c = db.connect().await.unwrap();
        c.execute("INSERT INTO file_deletions(id,batch_id,media_type,file_id,owner_id,path,mode,bin_path,destination,source_dev,source_ino,source_size,source_mtime,source_mtime_ns,phase,created_at,updated_at) VALUES(?,?,'movies',1,1,?,'recycle',?,?,?,?,?,?,?,'admitted',1,1)", libsql::params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), movie.to_str().unwrap(), bin.to_str().unwrap(), format!("{}/Film (2020)/Film.mkv", bin.to_str().unwrap()), identity.dev.to_string(), identity.ino.to_string(), identity.size as i64, identity.mtime, identity.mtime_ns]).await.unwrap();
    }
    db = restart(db, &dbpath).await;
    let c = db.connect().await.unwrap();
    assert_eq!(
        text(&c, "SELECT phase||':'||error_code FROM file_deletions").await,
        Some("aborted:interrupted".into())
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM movie_files").await, 1);
    assert_eq!(std::fs::read(&movie).unwrap(), b"movie bytes");
}

#[tokio::test]
async fn cross_device_recycle_copies_verifies_and_removes_source_after_commit() {
    let shm = std::path::Path::new("/dev/shm");
    let base = scratch_dir(&std::env::temp_dir(), "xdev");
    if !shm.is_dir()
        || std::fs::metadata(shm).unwrap().dev() == std::fs::metadata(&base).unwrap().dev()
    {
        std::fs::remove_dir_all(&base).unwrap();
        eprintln!(
            "cross-device scratch unavailable: /dev/shm shares the temp device; not simulated"
        );
        return;
    }
    let bin_root = scratch_dir(shm, "xdev-bin");
    let _scratch = Scratch(vec![base.clone(), bin_root.clone()]);
    let dbpath = base.join("library.db");
    let mut db = Arc::new(Database::open_local(&dbpath).await.unwrap());
    let (episode, movie) = library(&db, &base).await;
    let bin = bin_root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    configure(&db, MediaDomain::Tv, &bin).await;
    configure(&db, MediaDomain::Movies, &bin).await;
    let (url, server) = serve(db.clone()).await;
    let (status, body) = delete(&url, "movies", 1, "recycle").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["effect"], "recycled");
    assert_eq!(
        body["targets"],
        serde_json::json!([{"media_type":"movie","id":1}])
    );
    assert!(!movie.exists());
    let recycled = bin.join("Film (2020)/Film.mkv");
    assert_eq!(std::fs::read(&recycled).unwrap(), b"movie bytes");
    assert_ne!(
        std::fs::metadata(&recycled).unwrap().dev(),
        std::fs::metadata(&base).unwrap().dev()
    );
    // Crash after the cross-device copy is published but before the commit: the source stays.
    let marker = base.file_name().unwrap().to_str().unwrap().to_owned();
    arm(&marker);
    let (status, body) = delete(&url, "tv", 1, "recycle").await;
    disarm(&marker);
    assert_eq!(status, 500, "{body}");
    server.abort();
    let _ = server.await;
    {
        let c = db.connect().await.unwrap();
        assert_eq!(
            text(&c, "SELECT phase FROM file_deletions WHERE media_type='tv'").await,
            Some("copied".into())
        );
    }
    assert_eq!(
        std::fs::read(&episode).unwrap(),
        b"episode bytes, shared by two episodes",
        "source retained until the commit"
    );
    db = restart(db, &dbpath).await;
    let c = db.connect().await.unwrap();
    assert_eq!(
        text(
            &c,
            "SELECT phase||':'||transfer FROM file_deletions WHERE media_type='tv'"
        )
        .await,
        Some("complete:copy".into())
    );
    assert!(!episode.exists());
    assert_eq!(
        std::fs::read(bin.join("Show/Season 1/Show.S01E01E02.mkv")).unwrap(),
        b"episode bytes, shared by two episodes"
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM episode_files").await, 0);
    // No private partial copies remain in the bin.
    for entry in walk(&bin) {
        assert!(
            !entry
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(fsops::PARTIAL_PREFIX),
            "{entry:?}"
        );
    }
}
fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
