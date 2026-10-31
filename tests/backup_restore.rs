//! Backup/restore (job.002, scn.008) through real HTTP with scratch directories only. Instance A
//! runs in-process with a driven command worker; the isolated instance B is the real binary on a
//! fresh data directory. No installed service, library or live data directory is touched.
use axum::Router;
use hrrdarr::{
    backup::{self, ApiLimits, BackupLimits, BackupType, RestoreOutcome},
    commands,
    db::{Database, Error},
    episodes, library, providers,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const KEY_HEX: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const SECRET: &str = "BACKUP_PROVIDER_SECRET_SENTINEL_4417";

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> PathBuf {
        self.0.join("library.db")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- Independent stored-ZIP reader/writer (does not reuse the code under test) ----
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
fn u16_at(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}
fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}
fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let end = bytes.len() - 22;
    assert_eq!(&bytes[end..end + 4], b"PK\x05\x06", "end record");
    let count = u16_at(bytes, end + 10);
    let mut at = u32_at(bytes, end + 16);
    let mut out = Vec::new();
    for _ in 0..count {
        assert_eq!(&bytes[at..at + 4], b"PK\x01\x02");
        assert_eq!(u16_at(bytes, at + 10), 0, "stored entries only");
        let name_len = u16_at(bytes, at + 28);
        let size = u32_at(bytes, at + 24);
        let crc = u32_at(bytes, at + 16) as u32;
        let local = u32_at(bytes, at + 42);
        let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
        let data = local + 30 + u16_at(bytes, local + 26) + u16_at(bytes, local + 28);
        let content = bytes[data..data + size].to_vec();
        assert_eq!(crc32(&content), crc, "{name} crc");
        out.push((name, content));
        at += 46 + name_len + u16_at(bytes, at + 30) + u16_at(bytes, at + 32);
    }
    out
}
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let mut header = Vec::new();
        header.extend_from_slice(b"PK\x03\x04");
        header.extend_from_slice(&20u16.to_le_bytes());
        header.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x21, 0]);
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x21, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0; 8]);
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}
fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---- Instance A: in-process app over real HTTP ----
struct Instance {
    db: Arc<Database>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
    driver: commands::Driver,
}
impl Instance {
    async fn start(path: &Path, limits: ApiLimits) -> Self {
        let db = Arc::new(Database::open_local(path).await.unwrap());
        let key = Arc::new(providers::CredentialKey::from_hex(KEY_HEX).unwrap());
        let (provider_routes, refresh) = providers::router_with_refresh(db.clone(), Some(key));
        let app: Router = library::router(db.clone())
            .merge(episodes::router(db.clone()))
            .merge(commands::router(db.clone()))
            .merge(backup::router_with_limits(db.clone(), limits))
            .merge(provider_routes);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let driver = commands::start_driven(db.clone(), refresh).await.unwrap();
        Self {
            db,
            addr,
            server,
            driver,
        }
    }
    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }
    async fn stop(self) {
        self.server.abort();
        let _ = self.server.await;
        drop(self.driver);
        // Connection tasks spawned by the server release their router clones asynchronously.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&self.db) > 1 {
            assert!(Instant::now() < deadline, "database owners still alive");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(self.db);
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}
async fn call(origin: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut request = client().request(method.parse().unwrap(), format!("{origin}{path}"));
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        },
    )
}
async fn upload(origin: &str, bytes: Vec<u8>, content_type: &str) -> (u16, Value) {
    let response = client()
        .post(format!("{origin}/api/v1/system/restore/upload"))
        .header("Content-Type", content_type)
        .body(bytes)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}
/// Raw request so dot segments and encoded separators reach the server unnormalized.
async fn raw(addr: SocketAddr, method: &str, path: &str) -> u16 {
    raw_with(
        addr,
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        ),
    )
    .await
}
async fn raw_with(addr: SocketAddr, request: String) -> u16 {
    tokio::task::spawn_blocking(move || {
        let mut stream =
            std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        stream.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        text.split_whitespace().nth(1).unwrap().parse().unwrap()
    })
    .await
    .unwrap()
}

/// TV and movie rows deliberately share numeric ids (series/episode/movie 1, files 7).
async fn seed(db: &Database) {
    db.connect()
        .await
        .unwrap()
        .execute_batch(
            r#"
        INSERT INTO series(id,tvdb_id,title,path,monitored) VALUES(1,4401,'Backup Harbor','/fictional/tv/Backup Harbor',1);
        INSERT INTO seasons VALUES(1,1,1);
        INSERT INTO episode_files VALUES(7,1,'/fictional/tv/Backup Harbor/S01E01.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id,monitored) VALUES(1,1,1,1,'Backup pilot',7,1);
        INSERT INTO movie_metadata(id,tmdb_id,title,year) VALUES(1,9901,'Backup Movie',2021);
        INSERT INTO movies(id,metadata_id,path,monitored) VALUES(1,1,'/fictional/movies/Backup Movie (2021)',1);
        INSERT INTO movie_files(id,movie_id,path) VALUES(7,1,'/fictional/movies/Backup Movie (2021)/movie.mkv');
    "#,
        )
        .await
        .unwrap();
}
async fn add_provider(origin: &str) -> String {
    let (status, created) = call(origin, "POST", "/api/v1/providers", Some(json!({"name":"Backup indexer","enabled":true,"priority":10,"settings":{"implementation":"torznab","endpoint":"http://127.0.0.1:9/not-contacted","tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2000]}},"credentials":{"kind":"api_key","api_key":SECRET}}))).await;
    assert_eq!(status, 201, "{created}");
    created["id"].as_str().unwrap().to_owned()
}
async fn scalar(db: &Database, sql: &str) -> i64 {
    db.connect()
        .await
        .unwrap()
        .query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn ciphertext(db: &Database, id: &str) -> Vec<u8> {
    db.connect()
        .await
        .unwrap()
        .query("SELECT credentials FROM providers WHERE id=?1", [id])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
/// Enqueues a manual backup through the API and runs exactly one worker claim.
async fn manual_backup(a: &Instance) -> Value {
    let (status, queued) = call(
        &a.origin(),
        "POST",
        "/api/v1/system/backup-commands",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 202, "{queued}");
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["backup_type"], "manual");
    assert!(a.driver.step().await.unwrap(), "backup command claimed");
    let (status, done) = call(
        &a.origin(),
        "GET",
        &format!(
            "/api/v1/system/backup-commands/{}",
            queued["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(done["status"], "succeeded", "{done}");
    done
}
async fn download(origin: &str, id: &str) -> Vec<u8> {
    let response = client()
        .get(format!("{origin}/api/v1/system/backups/{id}/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["content-type"], "application/zip");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let disposition = response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        disposition.starts_with("attachment; filename=\"hrrdarr_backup_"),
        "{disposition}"
    );
    response.bytes().await.unwrap().to_vec()
}
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
fn old_name(year: u32, random: &str) -> String {
    format!("hrrdarr_backup_{year}0101T000000Z_{random}.zip")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_and_scheduled_backups_publish_verified_archives_without_key_material()
-> Result<(), Error> {
    let files = Sandbox::new();
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    seed(&a.db).await;
    let provider = add_provider(&a.origin()).await;
    let sealed = ciphertext(&a.db, &provider).await;
    assert!(
        !contains(&sealed, SECRET.as_bytes()),
        "credentials are sealed at the source"
    );

    let (status, settings) = call(&a.origin(), "GET", "/api/v1/system/backup-settings", None).await;
    assert_eq!(status, 200);
    let folder = files.0.join("Backups");
    assert_eq!(settings["folder"], Value::Null);
    assert_eq!(settings["effective_folder"], folder.to_str().unwrap());
    assert_eq!(
        (
            settings["interval_days"].as_i64(),
            settings["retention_days"].as_i64()
        ),
        (Some(7), Some(28))
    );

    // Manual backup through the durable command.
    let done = manual_backup(&a).await;
    let id = done["backup_id"].as_str().unwrap().to_owned();
    assert_eq!(done["pruned"], 0);
    let (status, page) = call(&a.origin(), "GET", "/api/v1/system/backups?limit=10", None).await;
    assert_eq!(status, 200);
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], id.as_str());
    assert_eq!(page["items"][0]["backup_type"], "manual");
    assert_eq!(page["items"][0]["name"], done["backup_name"]);
    assert_eq!(
        names_in(&folder.join(".staging")),
        Vec::<String>::new(),
        "staging is cleaned"
    );

    let bytes = download(&a.origin(), &id).await;
    assert_eq!(bytes.len() as i64, done["size_bytes"].as_i64().unwrap());
    // Secrets: ciphertext stays, plaintext and every encoding of the key are absent.
    assert!(
        contains(&bytes, &sealed),
        "encrypted credentials are retained as stored"
    );
    assert!(!contains(&bytes, SECRET.as_bytes()));
    assert!(!contains(&bytes, KEY_HEX.as_bytes()));
    assert!(!contains(&bytes, KEY_HEX.to_uppercase().as_bytes()));
    assert!(!contains(&bytes, &[0x5a; 32]), "raw key bytes absent");
    let entries = unzip(&bytes);
    assert_eq!(
        entries.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["manifest.json", "database.db"]
    );
    let manifest: Value = serde_json::from_slice(&entries[0].1).unwrap();
    let database = &entries[1].1;
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["backup_type"], "manual");
    assert_eq!(manifest["backend"], "local-libsql");
    assert_eq!(
        manifest["schema_version"],
        hrrdarr::db::latest_schema_version()
    );
    assert_eq!(
        manifest["migrations"].as_array().unwrap().len(),
        hrrdarr::db::latest_schema_version()
    );
    assert_eq!(manifest["migrations"][0]["name"], "prototype");
    assert_eq!(manifest["files"][0]["name"], "database.db");
    assert_eq!(manifest["files"][0]["size"], database.len());
    assert_eq!(manifest["files"][0]["sha256"], sha256(database));
    assert_eq!(manifest["secrets"]["key_included"], false);
    assert_eq!(manifest["secrets"]["encrypted_credentials"], 1);
    assert!(
        manifest["secrets"]["key_source"]
            .as_str()
            .unwrap()
            .contains("HRRDARR_PROVIDER_KEY")
    );
    assert!(
        manifest.to_string().contains("media files"),
        "excludes media"
    );
    let (status, detail) = call(
        &a.origin(),
        "GET",
        &format!("/api/v1/system/backups/{id}"),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(detail["manifest"], manifest);

    // The snapshot is a self-contained database holding both domains with colliding ids.
    let copy = files.0.join("extracted.db");
    std::fs::write(&copy, database)?;
    {
        let snapshot = libsql::Builder::new_local(&copy).build().await?;
        let c = snapshot.connect()?;
        let row = c.query("SELECT (SELECT title FROM series WHERE id=1),(SELECT title FROM episodes WHERE id=1),(SELECT m.title FROM movies v JOIN movie_metadata m ON m.id=v.metadata_id WHERE v.id=1),(SELECT path FROM episode_files WHERE id=7),(SELECT path FROM movie_files WHERE id=7)", ()).await?.next().await?.unwrap();
        assert_eq!(row.get::<String>(0)?, "Backup Harbor");
        assert_eq!(row.get::<String>(1)?, "Backup pilot");
        assert_eq!(row.get::<String>(2)?, "Backup Movie");
        assert!(row.get::<String>(3)?.ends_with("S01E01.mkv"));
        assert!(row.get::<String>(4)?.ends_with("movie.mkv"));
        let stored: Vec<u8> = c
            .query("SELECT credentials FROM providers", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        assert_eq!(stored, sealed);
        let journal: String = c
            .query("PRAGMA journal_mode", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        assert_eq!(
            journal, "delete",
            "self-contained rollback-journal snapshot"
        );
    }
    std::fs::remove_file(&copy)?;

    // Scheduled backup: the interval elapses, the worker enqueues and runs it, then retention
    // prunes only expired scheduled backups (manual ones are never pruned automatically).
    std::fs::write(
        folder.join("scheduled").join(old_name(2020, "0000aaaa")),
        b"expired scheduled",
    )
    .or_else(|_| {
        std::fs::create_dir(folder.join("scheduled"))?;
        std::fs::write(
            folder.join("scheduled").join(old_name(2020, "0000aaaa")),
            b"expired scheduled",
        )
    })?;
    std::fs::write(
        folder.join("manual").join(old_name(2020, "0000bbbb")),
        b"old manual",
    )?;
    std::fs::write(folder.join("scheduled").join("notes.txt"), b"not a backup")?;
    a.db.connect()
        .await?
        .execute("UPDATE backup_settings SET next_backup_at=1 WHERE id=1", ())
        .await?;
    let before = unix_now();
    assert!(a.driver.step().await.unwrap());
    let (_, commands) = call(&a.origin(), "GET", "/api/v1/system/backup-commands", None).await;
    let scheduled = &commands["items"][0];
    assert_eq!(scheduled["backup_type"], "scheduled", "{commands}");
    assert_eq!(scheduled["status"], "succeeded", "{scheduled}");
    assert_eq!(scheduled["pruned"], 1);
    let next = scalar(&a.db, "SELECT next_backup_at FROM backup_settings").await;
    assert!(
        next >= before + 7 * 86_400,
        "next scheduled slot is one interval later"
    );
    let scheduled_names = names_in(&folder.join("scheduled"));
    assert!(
        !scheduled_names.contains(&old_name(2020, "0000aaaa")),
        "expired scheduled pruned"
    );
    assert!(
        scheduled_names.contains(&"notes.txt".to_owned()),
        "foreign files are never pruned"
    );
    assert!(
        names_in(&folder.join("manual")).contains(&old_name(2020, "0000bbbb")),
        "manual kept"
    );
    let (_, typed) = call(
        &a.origin(),
        "GET",
        "/api/v1/system/backups?backup_type=scheduled",
        None,
    )
    .await;
    assert_eq!(typed["total"], 1);

    // A failure mid-archive publishes nothing and prunes nothing.
    std::fs::write(
        folder.join("scheduled").join(old_name(2021, "0000cccc")),
        b"expired again",
    )?;
    let listed = names_in(&folder.join("scheduled"));
    let failure = backup::create_backup(
        &a.db,
        BackupType::Scheduled,
        BackupLimits {
            max_archive_bytes: 64 * 1024,
            ..BackupLimits::command()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(failure.0, "backup_too_large");
    assert_eq!(
        names_in(&folder.join("scheduled")),
        listed,
        "nothing published or pruned"
    );
    assert_eq!(
        names_in(&folder.join(".staging")),
        Vec::<String>::new(),
        "partial archive removed"
    );
    // A snapshot that exceeds the database bound is refused before any copy is made.
    let failure = backup::create_backup(
        &a.db,
        BackupType::Manual,
        BackupLimits {
            max_database_bytes: 1024,
            ..BackupLimits::command()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(failure.0, "backup_too_large");
    assert_eq!(names_in(&folder.join(".staging")), Vec::<String>::new());

    // Explicit delete removes exactly one archive.
    let (status, _) = call(
        &a.origin(),
        "DELETE",
        &format!("/api/v1/system/backups/{id}"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _) = call(
        &a.origin(),
        "DELETE",
        &format!("/api/v1/system/backups/{id}"),
        None,
    )
    .await;
    assert_eq!(status, 404);
    a.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_validate_folder_paths_and_move_publication() -> Result<(), Error> {
    let files = Sandbox::new();
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    seed(&a.db).await;
    let library_root = files.0.join("media");
    std::fs::create_dir(&library_root)?;
    a.db.connect()
        .await?
        .execute(
            "INSERT INTO root_folders(media_type,path) VALUES('tv',?1)",
            [library_root.to_str().unwrap()],
        )
        .await?;
    let update = |folder: Value, revision: i64| json!({"revision":revision,"folder":folder,"interval_days":3,"retention_days":10});
    for (folder, status, code) in [
        (json!("relative/backups"), 400, "invalid_backup_request"),
        (json!("/"), 400, "invalid_backup_request"),
        (
            json!(format!("{}/../escape", files.0.display())),
            400,
            "invalid_backup_request",
        ),
        (
            json!(library_root.join("Backups").to_str().unwrap()),
            409,
            "backup_folder_overlaps_library",
        ),
        (
            json!(files.0.to_str().unwrap()),
            409,
            "backup_folder_overlaps_library",
        ),
    ] {
        let (got, body) = call(
            &a.origin(),
            "PUT",
            "/api/v1/system/backup-settings",
            Some(update(folder.clone(), 1)),
        )
        .await;
        assert_eq!(
            (got, body["error"]["code"].as_str().unwrap_or("")),
            (status, code),
            "{folder}"
        );
    }
    // A path below a regular file can never be created.
    std::fs::write(files.0.join("plain-file"), b"x")?;
    let (got, body) = call(
        &a.origin(),
        "PUT",
        "/api/v1/system/backup-settings",
        Some(update(
            json!(files.0.join("plain-file/sub").to_str().unwrap()),
            1,
        )),
    )
    .await;
    assert_eq!(
        (got, body["error"]["code"].as_str().unwrap()),
        (422, "backup_folder_unwritable")
    );
    let (got, _) = call(
        &a.origin(),
        "PUT",
        "/api/v1/system/backup-settings",
        Some(json!({"revision":1,"interval_days":3,"retention_days":10})),
    )
    .await;
    assert_eq!(got, 400, "folder is required (null selects the default)");
    for (interval, retention) in [(0, 10), (8, 10), (3, 0), (3, 91)] {
        let (got, _) = call(&a.origin(), "PUT", "/api/v1/system/backup-settings", Some(json!({"revision":1,"folder":null,"interval_days":interval,"retention_days":retention}))).await;
        assert_eq!(got, 400, "{interval}/{retention}");
    }
    assert_eq!(
        scalar(&a.db, "SELECT revision FROM backup_settings").await,
        1,
        "rejections change nothing"
    );

    let custom = files.0.join("elsewhere").join("archives");
    let (got, saved) = call(
        &a.origin(),
        "PUT",
        "/api/v1/system/backup-settings",
        Some(update(json!(format!("{}/", custom.display())), 1)),
    )
    .await;
    assert_eq!(got, 200, "{saved}");
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["folder"], custom.to_str().unwrap());
    assert_eq!(saved["effective_folder"], custom.to_str().unwrap());
    assert!(saved["next_backup_at"].as_i64().unwrap() > unix_now() + 2 * 86_400);
    assert!(
        custom.is_dir() && names_in(&custom).is_empty(),
        "probe leaves nothing behind"
    );
    let (got, _) = call(
        &a.origin(),
        "PUT",
        "/api/v1/system/backup-settings",
        Some(update(Value::Null, 1)),
    )
    .await;
    assert_eq!(got, 409, "stale revision");
    let done = manual_backup(&a).await;
    assert_eq!(
        names_in(&custom.join("manual")),
        [done["backup_name"].as_str().unwrap().to_owned()]
    );
    // A library root added later under the folder makes backups refuse to write there.
    let nested = custom.join("tv-library");
    a.db.connect()
        .await?
        .execute(
            "INSERT INTO root_folders(media_type,path) VALUES('movies',?1)",
            [nested.to_str().unwrap()],
        )
        .await?;
    let failure = backup::create_backup(&a.db, BackupType::Manual, BackupLimits::command())
        .await
        .unwrap_err();
    assert_eq!(failure.0, "backup_folder_unsafe");
    a.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_refuses_corrupt_tampered_newer_and_traversal_inputs_without_live_change()
-> Result<(), Error> {
    let files = Sandbox::new();
    let a = Instance::start(
        &files.db(),
        ApiLimits {
            max_upload_bytes: 32 * 1024 * 1024,
            ..ApiLimits::default()
        },
    )
    .await;
    seed(&a.db).await;
    add_provider(&a.origin()).await;
    let done = manual_backup(&a).await;
    let id = done["backup_id"].as_str().unwrap().to_owned();
    let good = download(&a.origin(), &id).await;
    let entries = unzip(&good);
    let manifest: Value = serde_json::from_slice(&entries[0].1).unwrap();
    let database = entries[1].1.clone();

    let edited_db = |sql: &'static str| {
        let path = files.0.join(format!("edit-{}.db", uuid::Uuid::new_v4()));
        let database = database.clone();
        async move {
            std::fs::write(&path, &database).unwrap();
            {
                let copy = libsql::Builder::new_local(&path).build().await.unwrap();
                copy.connect().unwrap().execute_batch(sql).await.unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            bytes
        }
    };
    let with_manifest = |db: &[u8], edit: &dyn Fn(&mut Value)| {
        let mut m = manifest.clone();
        m["files"][0]["sha256"] = json!(sha256(db));
        m["files"][0]["size"] = json!(db.len());
        edit(&mut m);
        zip(&[
            ("manifest.json", &serde_json::to_vec(&m).unwrap()),
            ("database.db", db),
        ])
    };
    // Rebuilding the untouched archive with the independent writer is accepted structurally;
    // it is used below only as a control.
    let control = with_manifest(&database, &|_| {});
    let orphan = edited_db("PRAGMA foreign_keys=OFF; INSERT INTO episodes(id,series_id,season,number,title) VALUES(99,999,1,1,'orphan');").await;
    // Reasoning: one version past whatever this build's latest is, so appended migrations never collide.
    let future = edited_db("INSERT INTO schema_migrations(version,name,checksum,sql) SELECT max(version)+1,'future','unknown','unknown' FROM schema_migrations;").await;
    let mut flipped = good.clone();
    let middle = good.len() / 2;
    flipped[middle] ^= 0xff;
    let mut stale_sha = manifest.clone();
    stale_sha["files"][0]["sha256"] = json!("0".repeat(64));
    let stale = zip(&[
        ("manifest.json", &serde_json::to_vec(&stale_sha).unwrap()),
        ("database.db", &database),
    ]);
    let cases: Vec<(&str, Vec<u8>, &str, u16, &str)> = vec![
        (
            "garbage",
            b"definitely not an archive".to_vec(),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "empty",
            Vec::new(),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "truncated",
            good[..good.len() - 100].to_vec(),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "flipped byte",
            flipped,
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "stale manifest checksum",
            stale,
            "application/zip",
            422,
            "backup_checksum_mismatch",
        ),
        (
            "foreign key corruption",
            with_manifest(&orphan, &|_| {}),
            "application/zip",
            422,
            "backup_integrity_failed",
        ),
        (
            "newer database history",
            with_manifest(&future, &|_| {}),
            "application/zip",
            422,
            "backup_schema_unsupported",
        ),
        (
            "newer manifest schema",
            with_manifest(&database, &|m| {
                m["schema_version"] = json!(hrrdarr::db::latest_schema_version() + 1);
                m["migrations"].as_array_mut().unwrap().push(json!({"version":hrrdarr::db::latest_schema_version()+1,"name":"future","checksum":"0"}));
            }),
            "application/zip",
            422,
            "backup_schema_unsupported",
        ),
        (
            "altered migration checksum",
            with_manifest(&database, &|m| {
                m["migrations"][3]["checksum"] = json!("0".repeat(64))
            }),
            "application/zip",
            422,
            "backup_history_mismatch",
        ),
        (
            "manifest claims key material",
            with_manifest(&database, &|m| m["secrets"]["key_included"] = json!(true)),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "traversal entry",
            zip(&[
                ("manifest.json", &entries[0].1),
                ("../escape.db", &database),
            ]),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "extra entry",
            zip(&[
                ("manifest.json", &entries[0].1),
                ("database.db", &database),
                ("extra", b"x"),
            ]),
            "application/zip",
            422,
            "backup_archive_invalid",
        ),
        (
            "wrong media type",
            // Refused from the headers before any body byte is read; a small body keeps the
            // client from racing the early response with a broken pipe.
            b"PK".to_vec(),
            "text/plain",
            415,
            "backup_upload_unsupported",
        ),
    ];
    for (label, bytes, content_type, status, code) in cases {
        let (got, body) = upload(&a.origin(), bytes, content_type).await;
        assert_eq!(
            (got, body["error"]["code"].as_str().unwrap_or("")),
            (status, code),
            "{label}: {body}"
        );
        let (_, pending) = call(&a.origin(), "GET", "/api/v1/system/restore", None).await;
        assert_eq!(pending["pending"], Value::Null, "{label}: nothing staged");
        // The permanent lock is ownership metadata, not leaked staging data.
        let leftovers: Vec<String> = names_in(&files.0)
            .into_iter()
            .filter(|n| n.contains(".restore-") && n != ".library.db.restore-lock")
            .collect();
        assert!(
            leftovers.is_empty(),
            "{label}: staging cleaned {leftovers:?}"
        );
        assert!(
            !files.0.join("escape.db").exists() && !std::env::temp_dir().join("escape.db").exists()
        );
        assert_eq!(
            scalar(&a.db, "SELECT count(*) FROM series").await,
            1,
            "{label}: live data unchanged"
        );
    }
    // A declared length over the bound is refused before any byte is read or staged. (The
    // streamed bound for undeclared lengths is covered by the restore unit test.)
    let declared = raw_with(
        a.addr,
        "POST /api/v1/system/restore/upload HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/zip\r\nContent-Length: 34359738368\r\nConnection: close\r\n\r\n".to_owned(),
    )
    .await;
    assert_eq!(declared, 413);
    // The stable lock intentionally survives; only unpublished restore data must be cleaned.
    assert!(
        names_in(&files.0)
            .iter()
            .all(|n| !n.contains(".restore-") || n == ".library.db.restore-lock")
    );
    // The independently rebuilt archive is accepted, proving the cases above fail for the
    // tampering, not for the rebuild; it is cancelled explicitly.
    let (got, staged) = upload(&a.origin(), control, "application/zip").await;
    assert_eq!(got, 202, "{staged}");
    assert_eq!(staged["source"], "upload");
    assert_eq!(staged["restart_required"], true);
    let (got, _) = call(&a.origin(), "DELETE", "/api/v1/system/restore", None).await;
    assert_eq!(got, 204);
    let (got, _) = call(&a.origin(), "DELETE", "/api/v1/system/restore", None).await;
    assert_eq!(got, 404);

    // Path traversal and path-shaped ids never resolve outside the listed archives.
    let unknown = "0123456789abcdef0123456789abcdef";
    for (method, path, status) in [
        (
            "GET",
            "/api/v1/system/backups/..%2F..%2Flibrary.db/download".to_owned(),
            400,
        ),
        (
            "GET",
            "/api/v1/system/backups/%2e%2e/download".to_owned(),
            400,
        ),
        (
            "GET",
            "/api/v1/system/backups/..%2f..%2f..%2fetc%2fpasswd".to_owned(),
            400,
        ),
        (
            "DELETE",
            "/api/v1/system/backups/..%2Flibrary.db".to_owned(),
            400,
        ),
        (
            "POST",
            "/api/v1/system/backups/..%2F..%2Flibrary.db/restore".to_owned(),
            400,
        ),
        (
            "GET",
            format!("/api/v1/system/backups/{}/download", id.to_uppercase()),
            400,
        ),
        (
            "GET",
            format!("/api/v1/system/backups/{unknown}/download"),
            404,
        ),
        (
            "POST",
            format!("/api/v1/system/backups/{unknown}/restore"),
            404,
        ),
        ("DELETE", format!("/api/v1/system/backups/{unknown}"), 404),
    ] {
        assert_eq!(raw(a.addr, method, &path).await, status, "{method} {path}");
    }
    // A symlink planted with a backup-shaped name is neither listed nor served.
    let planted = old_name(2030, "0000dddd");
    std::os::unix::fs::symlink(files.db(), files.0.join("Backups/manual").join(&planted))?;
    let (_, page) = call(&a.origin(), "GET", "/api/v1/system/backups", None).await;
    assert_eq!(page["total"], 1, "{page}");
    assert!(!page.to_string().contains(&planted));
    assert!(files.db().exists(), "symlink target untouched");
    a.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_by_id_applies_on_restart_retains_original_and_resumes_or_quarantines()
-> Result<(), Error> {
    let files = Sandbox::new();
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    seed(&a.db).await;
    let id = manual_backup(&a).await["backup_id"]
        .as_str()
        .unwrap()
        .to_owned();
    a.db.connect()
        .await?
        .execute(
            "INSERT INTO series(id,title,path) VALUES(2,'After backup','/fictional/tv/After')",
            (),
        )
        .await?;
    let restore = format!("/api/v1/system/backups/{id}/restore");
    let (got, staged) = call(&a.origin(), "POST", &restore, None).await;
    assert_eq!(got, 202, "{staged}");
    assert_eq!(
        (staged["source"].as_str(), staged["backup_id"].as_str()),
        (Some("backup"), Some(id.as_str()))
    );
    assert_eq!(
        staged["schema_version"],
        hrrdarr::db::latest_schema_version()
    );
    let (got, body) = call(&a.origin(), "POST", &restore, None).await;
    assert_eq!(
        (got, body["error"]["code"].as_str().unwrap()),
        (409, "restore_already_pending")
    );
    let (_, pending) = call(&a.origin(), "GET", "/api/v1/system/restore", None).await;
    assert_eq!(pending["pending"]["backup_id"], id.as_str());
    assert_eq!(
        scalar(&a.db, "SELECT count(*) FROM series").await,
        2,
        "nothing applied while running"
    );
    a.stop().await;

    let base = ".library.db";
    let originals = |dir: &Path| -> Vec<PathBuf> {
        names_in(dir)
            .into_iter()
            .filter(|n| n.starts_with(&format!("{base}.restore-original-")))
            .map(|n| dir.join(n))
            .collect()
    };
    // Applied before open: live now matches the backup; the replaced database is retained.
    let outcome = backup::apply_pending_restore(&files.db()).await?;
    let RestoreOutcome::Applied { original } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(originals(&files.0), [original.clone()]);
    assert!(original.join("database.db").is_file());
    assert!(original.join("restore-applied.json").is_file());
    assert!(!files.0.join(format!("{base}.restore-pending")).exists());
    assert_eq!(
        backup::apply_pending_restore(&files.db()).await?,
        RestoreOutcome::None,
        "consumed once"
    );
    {
        let db = Database::open_local(files.db()).await?;
        assert_eq!(scalar(&db, "SELECT count(*) FROM series").await, 1);
        assert_eq!(
            scalar(&db, "SELECT count(*) FROM movies WHERE id=1").await,
            1
        );
        // The snapshot truthfully captured its own command mid-run; worker start recovers it.
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM backup_commands WHERE status='running'"
            )
            .await,
            1
        );
    }
    {
        // The retained original is a recoverable database with the post-backup change.
        let kept = libsql::Builder::new_local(original.join("database.db"))
            .build()
            .await?;
        let n: i64 = kept
            .connect()?
            .query("SELECT count(*) FROM series WHERE id=2", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        assert_eq!(n, 1);
    }

    // Resume: an apply interrupted after the live database was moved aside finishes using the
    // recorded original directory instead of creating another one.
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    assert_eq!(scalar(&a.db, "SELECT count(*) FROM backup_commands WHERE status='failed' AND error_code='interrupted'").await, 1, "never retried");
    a.db.connect()
        .await?
        .execute(
            "INSERT INTO series(id,title,path) VALUES(3,'Second change','/fictional/tv/Second')",
            (),
        )
        .await?;
    let (got, _) = call(&a.origin(), "POST", &restore, None).await;
    assert_eq!(got, 202);
    a.stop().await;
    let pending_dir = files.0.join(format!("{base}.restore-pending"));
    let interrupted = files
        .0
        .join(format!("{base}.restore-original-19700101T000000Z-0000eeee"));
    std::fs::create_dir(&interrupted)?;
    std::fs::write(
        pending_dir.join("original"),
        interrupted.file_name().unwrap().to_str().unwrap(),
    )?;
    for suffix in ["", "-wal", "-shm"] {
        let from = files.0.join(format!("library.db{suffix}"));
        if from.exists() {
            std::fs::rename(&from, interrupted.join(format!("database.db{suffix}")))?;
        }
    }
    assert!(
        !files.db().exists(),
        "simulated crash: live database moved, candidate not installed"
    );
    assert_eq!(
        backup::apply_pending_restore(&files.db()).await?,
        RestoreOutcome::Applied {
            original: interrupted.clone()
        }
    );
    assert_eq!(
        originals(&files.0).len(),
        2,
        "no extra original created on resume"
    );
    {
        let db = Database::open_local(files.db()).await?;
        assert_eq!(scalar(&db, "SELECT count(*) FROM series").await, 1);
    }

    // Quarantine: a staged candidate that changed after validation is never applied.
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    a.db.connect()
        .await?
        .execute(
            "INSERT INTO series(id,title,path) VALUES(4,'Kept live','/fictional/tv/Kept')",
            (),
        )
        .await?;
    let (got, _) = call(&a.origin(), "POST", &restore, None).await;
    assert_eq!(got, 202);
    a.stop().await;
    std::fs::OpenOptions::new()
        .append(true)
        .open(pending_dir.join("database.db"))?
        .write_all(b"tamper")?;
    let outcome = backup::apply_pending_restore(&files.db()).await?;
    let RestoreOutcome::Rejected {
        quarantined,
        reason,
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(reason, "staged_checksum_mismatch");
    assert!(quarantined.join("database.db").is_file());
    assert!(!pending_dir.exists());
    let db = Database::open_local(files.db()).await?;
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM series WHERE id=4").await,
        1,
        "live database unchanged"
    );
    // A live owner blocks apply; a pending directory without its database is set aside.
    std::fs::create_dir(&pending_dir)?;
    std::fs::copy(
        quarantined.join("restore.json"),
        pending_dir.join("restore.json"),
    )?;
    assert!(
        backup::apply_pending_restore(&files.db()).await.is_err(),
        "owned database is never moved"
    );
    drop(db);
    let outcome = backup::apply_pending_restore(&files.db()).await?;
    assert!(
        matches!(
            outcome,
            RestoreOutcome::Rejected {
                reason: "staged_database_missing",
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(!pending_dir.exists() && files.db().is_file());
    Ok(())
}

// ---- Isolated instance B: the real binary on a fresh data directory ----
struct Process {
    child: std::process::Child,
    output: PathBuf,
}
impl Process {
    fn spawn(directory: &Path, run: u32) -> Self {
        let output = directory.join(format!("output-{run}"));
        let file = std::fs::File::create(&output).unwrap();
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
            .env_clear()
            .env("HRRDARR_DATABASE_PATH", directory.join("instance.db"))
            .env("HRRDARR_BIND", "127.0.0.1:0")
            .env("HRRDARR_PROVIDER_KEY", KEY_HEX)
            .current_dir(directory)
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .spawn()
            .unwrap();
        Self { child, output }
    }
    fn output(&self) -> String {
        std::fs::read_to_string(&self.output).unwrap_or_default()
    }
    async fn ready(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "exited: {}",
                self.output()
            );
            if let Some(origin) = self
                .output()
                .lines()
                .find_map(|l| l.strip_prefix("hrrdarr listening on "))
                .map(str::to_owned)
            {
                if client()
                    .get(format!("{origin}/api/v1/system/backup-settings"))
                    .send()
                    .await
                    .is_ok()
                {
                    return origin;
                }
            }
            assert!(
                Instant::now() < deadline,
                "startup timeout: {}",
                self.output()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploaded_backup_restores_into_an_isolated_binary_instance_on_restart() -> Result<(), Error>
{
    let source = Sandbox::new();
    let a = Instance::start(&source.db(), ApiLimits::default()).await;
    seed(&a.db).await;
    let provider = add_provider(&a.origin()).await;
    let sealed = ciphertext(&a.db, &provider).await;
    let id = manual_backup(&a).await["backup_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let archive = download(&a.origin(), &id).await;
    a.stop().await;

    let isolated = Sandbox::new();
    let mut b = Process::spawn(&isolated.0, 1);
    let origin = b.ready().await;
    let (_, series) = call(&origin, "GET", "/api/v1/tv/series", None).await;
    assert_eq!(series["total"], 0, "fresh isolated instance");
    let (got, staged) = upload(&origin, archive, "application/zip").await;
    assert_eq!(got, 202, "{staged}");
    assert_eq!(staged["restart_required"], true);
    let (_, still) = call(&origin, "GET", "/api/v1/tv/series", None).await;
    assert_eq!(still["total"], 0, "restore applies only on restart");
    b.stop();

    let mut b = Process::spawn(&isolated.0, 2);
    let origin = b.ready().await;
    assert!(
        b.output()
            .contains("event=backup_restore_applied original=.instance.db.restore-original-"),
        "{}",
        b.output()
    );
    let (_, series) = call(&origin, "GET", "/api/v1/tv/series", None).await;
    assert_eq!(series["total"], 1, "{series}");
    assert_eq!(series["items"][0]["title"], "Backup Harbor");
    assert_eq!(series["items"][0]["id"], 1);
    let (status, movie) = call(&origin, "GET", "/api/v1/movies/1", None).await;
    assert_eq!(status, 200, "{movie}");
    assert_eq!(
        (movie["title"].as_str(), movie["media_type"].as_str()),
        (Some("Backup Movie"), Some("movies"))
    );
    let (status, episode) = call(&origin, "GET", "/api/v1/episodes/1", None).await;
    assert_eq!(status, 200, "{episode}");
    assert_eq!(episode["title"], "Backup pilot");
    let (_, providers) = call(&origin, "GET", "/api/v1/providers?media_type=movies", None).await;
    assert_eq!(providers["total"], 1, "{providers}");
    assert_eq!(providers["items"][0]["id"], provider.as_str());
    assert_eq!(providers["items"][0]["has_credentials"], true);
    assert!(!providers.to_string().contains(SECRET));
    let (_, pending) = call(&origin, "GET", "/api/v1/system/restore", None).await;
    assert_eq!(pending["pending"], Value::Null);
    b.stop();

    // Credentials arrive as the same ciphertext; the key only ever came from the environment.
    let restored = libsql::Builder::new_local(isolated.0.join("instance.db"))
        .build()
        .await?;
    let stored: Vec<u8> = restored
        .connect()?
        .query("SELECT credentials FROM providers", ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert_eq!(stored, sealed);
    let originals: Vec<String> = names_in(&isolated.0)
        .into_iter()
        .filter(|n| n.starts_with(".instance.db.restore-original-"))
        .collect();
    assert_eq!(originals.len(), 1);
    assert!(
        isolated.0.join(&originals[0]).join("database.db").is_file(),
        "replaced database retained"
    );
    Ok(())
}

/// Update backups: an upgrade publishes the pre-migration snapshot as a typed `update` archive;
/// restoring that older-schema archive is accepted and migrates again on the next start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_publishes_an_update_backup_that_restores_and_remigrates() -> Result<(), Error> {
    let files = Sandbox::new();
    let path = files.0.join("instance.db");
    {
        let db = Database::open_local(&path).await?;
        seed(&db).await;
        let c = db.connect().await?;
        let (version, name): (i64, String) = {
            let row = c
                .query(
                    "SELECT version,name FROM schema_migrations ORDER BY version DESC LIMIT 1",
                    (),
                )
                .await?
                .next()
                .await?
                .unwrap();
            (row.get(0)?, row.get(1)?)
        };
        // Reasoning: receipts are now the last migration; removing only their empty tables
        // reconstructs the preceding schema while retaining the backup engine under test.
        assert_eq!(
            name, "wanted_search_receipts",
            "the receipt migration must be the latest for this fixture"
        );
        assert_eq!(version as usize, hrrdarr::db::latest_schema_version());
        c.execute_batch(
            "DROP TABLE wanted_search_receipt_targets; DROP TABLE wanted_search_receipts;",
        )
        .await?;
        c.execute("DELETE FROM schema_migrations WHERE version=?1", [version])
            .await?;
    }
    let mut b = Process::spawn(&files.0, 1);
    let origin = b.ready().await;
    assert!(
        b.output().contains("event=migration_backup_created"),
        "{}",
        b.output()
    );
    assert!(
        b.output().contains("event=backup_published"),
        "{}",
        b.output()
    );
    let (_, page) = call(
        &origin,
        "GET",
        "/api/v1/system/backups?backup_type=update",
        None,
    )
    .await;
    assert_eq!(page["total"], 1, "{page}");
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (_, detail) = call(
        &origin,
        "GET",
        &format!("/api/v1/system/backups/{id}"),
        None,
    )
    .await;
    let older = hrrdarr::db::latest_schema_version() - 1;
    assert_eq!(detail["manifest"]["backup_type"], "update");
    assert_eq!(detail["manifest"]["schema_version"], older);
    // The runner's own recovery directory is retained alongside the typed archive.
    assert!(
        names_in(&files.0)
            .iter()
            .any(|n| n.starts_with(".hrrdarr-migration-"))
    );
    let (got, staged) = call(
        &origin,
        "POST",
        &format!("/api/v1/system/backups/{id}/restore"),
        None,
    )
    .await;
    assert_eq!(got, 202, "{staged}");
    assert_eq!(staged["schema_version"], older);
    b.stop();
    let mut b = Process::spawn(&files.0, 2);
    let origin = b.ready().await;
    let output = b.output();
    assert!(output.contains("event=backup_restore_applied"), "{output}");
    assert!(
        output.contains("event=migration_backup_created"),
        "restored older schema migrates again: {output}"
    );
    let (_, series) = call(&origin, "GET", "/api/v1/tv/series", None).await;
    assert_eq!(series["items"][0]["title"], "Backup Harbor");
    let (_, page) = call(
        &origin,
        "GET",
        "/api/v1/system/backups?backup_type=update",
        None,
    )
    .await;
    assert_eq!(page["total"], 2);
    b.stop();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inventory_over_one_thousand_retains_pagination_and_id_operations() -> Result<(), Error> {
    let files = Sandbox::new();
    let a = Instance::start(&files.db(), ApiLimits::default()).await;
    let published =
        backup::create_backup(&a.db, BackupType::Manual, BackupLimits::command()).await?;
    let folder = files.0.join("Backups/manual");
    let source = folder.join(&published.backup.name);
    // Hard links keep this fixture small while exercising real directory enumeration and archives.
    for n in 0..1002 {
        std::fs::hard_link(&source, folder.join(old_name(2025, &format!("{n:08x}"))))?;
        std::fs::write(folder.join(format!("foreign-{n}")), b"ignored")?;
    }
    let origin = a.origin();
    let (status, first) = call(&origin, "GET", "/api/v1/system/backups?limit=100", None).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(
        first["total"], 1003,
        "foreign files cannot hide retained archives"
    );
    assert_eq!(first["items"][0]["id"], published.backup.id);
    let (status, last) = call(
        &origin,
        "GET",
        "/api/v1/system/backups?offset=1001&limit=100",
        None,
    )
    .await;
    assert_eq!(status, 200, "{last}");
    assert_eq!(last["total"], 1003);
    assert_eq!(last["items"].as_array().unwrap().len(), 2);
    let id = last["items"][1]["id"].as_str().unwrap();
    let path = format!("/api/v1/system/backups/{id}");
    let (status, detail) = call(&origin, "GET", &path, None).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["backup"]["id"], id);
    assert_eq!(download(&origin, id).await, std::fs::read(&source)?);
    let (status, restore) = call(&origin, "POST", &format!("{path}/restore"), None).await;
    assert_eq!(status, 202, "{restore}");
    assert_eq!(restore["backup_id"], id);
    assert_eq!(
        call(&origin, "DELETE", "/api/v1/system/restore", None)
            .await
            .0,
        204
    );
    assert_eq!(call(&origin, "DELETE", &path, None).await.0, 204);
    assert_eq!(call(&origin, "GET", &path, None).await.0, 404);
    assert!(
        source.exists(),
        "deleting one name preserves the original backup"
    );
    a.stop().await;
    Ok(())
}
