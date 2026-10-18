use axum::Router;
use hrrdarr::{custom_formats, db::Database, naming};
use libsql::{Connection, params};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const PREVIEW: &str = "/api/v1/movies/rename-preview";
const CONFIG: &str = "/api/v1/movies/config/naming";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-rename-preview-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Api {
    origin: String,
    task: Option<tokio::task::JoinHandle<()>>,
    http: reqwest::Client,
}
impl Api {
    async fn start(db: Arc<Database>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .merge(naming::router(db.clone()))
            .merge(custom_formats::router(db));
        Self {
            origin,
            task: Some(tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap()
            })),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
    async fn request(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let mut request = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.origin));
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        assert_eq!(status, expected, "{method} {path}: {text}");
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }
    async fn preview(&self, ids: &str) -> Value {
        self.request("GET", &format!("{PREVIEW}?movie_ids={ids}"), None, 200)
            .await
    }
    async fn config(&self, enabled: bool, format: Option<&str>) -> Value {
        let mut config = self.request("GET", CONFIG, None, 200).await;
        config["rename_enabled"] = json!(enabled);
        config["standard_movie_format"] = json!(format);
        self.request("PUT", CONFIG, Some(config), 200).await
    }
    async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn number(c: &Connection, sql: &str) -> i64 {
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
async fn seed(c: &Connection, id: i64, title: &str, root: &str, file: Option<&str>) -> Result {
    c.execute(
        "INSERT INTO movie_metadata(id,tmdb_id,title,year)VALUES(?,?,?,2020)",
        params![id + 1000, id + 10000, title],
    )
    .await?;
    c.execute(
        "INSERT INTO movies(id,metadata_id,path)VALUES(?,?,?)",
        params![id, id + 1000, root],
    )
    .await?;
    if let Some(file) = file {
        c.execute(
            "INSERT INTO movie_files(id,movie_id,path)VALUES(?,?,?)",
            params![id, id, file],
        )
        .await?;
    }
    Ok(())
}
fn counts(value: &Value) {
    let items = value["items"].as_array().unwrap();
    assert_eq!(
        value["unavailable_count"].as_u64().unwrap(),
        items
            .iter()
            .filter(|row| row["status"] == "unavailable")
            .count() as u64
    );
    assert_eq!(
        value["files_considered"].as_u64().unwrap(),
        items.len() as u64 + value["unchanged_count"].as_u64().unwrap()
    );
}
fn row(value: &Value, movie: i64) -> &Value {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["movie_id"] == movie)
        .unwrap()
}

#[tokio::test]
async fn complete_multi_movie_preview_bounds_relative_paths_and_http_read_only() -> Result {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    seed(
        &c,
        1,
        "Movie",
        "/owned/movie",
        Some("/owned/movie/nested/old.MKV"),
    )
    .await?;
    seed(&c, 2, "Same", "/owned/two", Some("/owned/two/Same.MKV")).await?;
    seed(&c, 3, "Case", "/owned/three", Some("/owned/three/case.MKV")).await?;
    seed(&c, 4, "No file", "/owned/four", None).await?;
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/tv');INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/TV.mkv');").await?;
    let api = Api::start(db.clone()).await;
    let config = api.config(true, Some("{Movie Title}")).await;
    let tv = api
        .request("GET", "/api/v1/tv/config/naming", None, 200)
        .await;
    let before = number(&c, "PRAGMA data_version").await;
    let result = api.preview("4,3,2,01").await;
    counts(&result);
    assert_eq!(result["movie_ids"], json!([1, 2, 3, 4]));
    assert_eq!(result["naming_revision"], config["revision"]);
    assert_eq!(result["standard_movie_format"], "{Movie Title}");
    assert_eq!(result["rename_enabled"], true);
    assert_eq!(result["files_considered"], 3);
    assert_eq!(result["unchanged_count"], 1);
    assert_eq!(result["unavailable_count"], 0);
    assert_eq!(
        result["items"],
        json!([{"movie_id":1,"movie_file_id":1,"existing_path":"nested/old.MKV","new_path":"Movie.MKV","status":"change","reasons":[]},{"movie_id":3,"movie_file_id":3,"existing_path":"case.MKV","new_path":"Case.MKV","status":"change","reasons":[]}])
    );
    assert_eq!(api.preview("4").await["files_considered"], 0);
    for query in [
        "",
        "movie_ids=",
        "movie_ids=0",
        "movie_ids=-1",
        "movie_ids=+1",
        "movie_ids=1.0",
        "movie_ids=9007199254740992",
        "movie_ids=1,",
        "movie_ids=1,,2",
        "movie_ids=1,01",
        "movie_ids=1&movie%5fids=2",
        "movie_ids=1&limit=1",
        "movie_ids=%FF",
        "movie_ids=%ZZ",
        "movie_ids=%201",
    ] {
        let response = api
            .request("GET", &format!("{PREVIEW}?{query}"), None, 400)
            .await;
        assert_eq!(response["error"]["code"], "invalid_request");
    }
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?movie_ids=1,999"), None, 404)
            .await["error"]["code"],
        "movie_not_found"
    );
    assert_eq!(
        api.http
            .post(format!("{}{PREVIEW}?movie_ids=1", api.origin))
            .send()
            .await?
            .status(),
        405
    );
    assert_eq!(
        api.http
            .get(format!(
                "{}/api/v1/tv/rename-preview?movie_ids=1",
                api.origin
            ))
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(number(&c, "PRAGMA data_version").await, before);
    assert_eq!(
        api.request("GET", "/api/v1/tv/config/naming", None, 200)
            .await,
        tv
    );
    assert_eq!(number(&c, "SELECT count(*) FROM commands").await, 0);
    assert_eq!(number(&c, "SELECT count(*) FROM operations").await, 0);
    c.execute_batch("INSERT INTO movie_metadata(id,title)VALUES(9007199254740991,'Maximum identity');INSERT INTO movies(id,metadata_id,path)VALUES(9007199254740991,9007199254740991,'/owned/maximum');INSERT INTO movie_files(id,movie_id,path)VALUES(9007199254740991,9007199254740991,'/owned/maximum/old.mkv');").await?;
    let maximum = api.preview("9007199254740991").await;
    assert_eq!(maximum["movie_ids"], json!([9007199254740991_i64]));
    assert_eq!(maximum["items"][0]["movie_file_id"], 9007199254740991_i64);
    // Input and output caps cover the complete selection, not a truncated first page.
    for id in 5..=201 {
        seed(
            &c,
            id,
            "Batch",
            &format!("/owned/{id}"),
            Some(&format!("/owned/{id}/old.mkv")),
        )
        .await?;
    }
    let ids = (1..=200)
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let all = api.preview(&ids).await;
    counts(&all);
    assert_eq!(all["files_considered"], 199);
    assert_eq!(all["items"].as_array().unwrap().len(), 198);
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?movie_ids={ids},201"), None, 400)
            .await["error"]["code"],
        "invalid_request"
    );
    let oversized = format!("movie_ids={}1", "0".repeat(4096));
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?{oversized}"), None, 400)
            .await["error"]["code"],
        "invalid_request"
    );
    api.stop().await;
    Ok(())
}

#[tokio::test]
async fn disabled_original_name_and_required_only_fact_validation() -> Result {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    seed(&c, 1, "Title", "/owned/a", Some("/owned/a/current.mkv")).await?;
    seed(
        &c,
        2,
        "Other",
        "/owned/b",
        Some("/owned/b/nested/current.mkv"),
    )
    .await?;
    seed(
        &c,
        3,
        "Bare colon",
        "/owned/c",
        Some("/owned/c/current.mkv"),
    )
    .await?;
    c.execute_batch("INSERT INTO file_metadata(media_type,movie_file_id,original_release_title,original_file_path,quality_id,revision_json)VALUES('movies',1,'Scene: Name','/downloads/WRONG.mkv',3,'{}'),('movies',2,NULL,'/downloads/WRONG.mkv',NULL,'{}'),('movies',3,'Scene:Name',NULL,NULL,NULL);").await?;
    let api = Api::start(db.clone()).await;
    // Disabled cleanup uses its source-defined defaults; unused pattern/revision must not block it.
    c.execute("UPDATE naming_settings SET standard_movie_format='{Unsupported}',replace_illegal_characters=0,revision=revision+1 WHERE domain='movies'",()).await?;
    let disabled = api.preview("1,2,3").await;
    counts(&disabled);
    assert_eq!(disabled["rename_enabled"], false);
    // Pinned default cleanup distinguishes ': ' from ':'; configured enabled
    // Smart rendering is a separate accepted policy and must remain unchanged.
    assert_eq!(row(&disabled, 1)["new_path"], "Scene - Name.mkv");
    assert_eq!(row(&disabled, 3)["new_path"], "Scene-Name.mkv");
    assert_eq!(row(&disabled, 2)["new_path"], "current.mkv");
    c.execute(
        "UPDATE naming_settings SET rename_enabled=1,revision=revision+1 WHERE domain='movies'",
        (),
    )
    .await?;
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?movie_ids=1"), None, 422)
            .await["error"]["code"],
        "naming_configuration_invalid"
    );
    c.execute("UPDATE naming_settings SET standard_movie_format=NULL,revision=revision+1 WHERE domain='movies'",()).await?;
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?movie_ids=1"), None, 422)
            .await["error"]["code"],
        "naming_configuration_invalid"
    );
    api.config(true, Some("{Movie Title}")).await;
    assert_eq!(row(&api.preview("1").await, 1)["new_path"], "Title.mkv");
    api.config(true, Some("{Movie Title} {Quality Full}")).await;
    let invalid = api.preview("1").await;
    counts(&invalid);
    assert_eq!(row(&invalid, 1)["status"], "unavailable");
    assert_eq!(row(&invalid, 1)["reasons"], json!(["invalid_file_facts"]));
    assert_eq!(row(&invalid, 1)["new_path"], Value::Null);
    c.execute(
        "UPDATE file_metadata SET quality_id=NULL,revision_json=NULL WHERE movie_file_id=1",
        (),
    )
    .await?;
    assert_eq!(row(&api.preview("1").await, 1)["new_path"], "Title.mkv");
    api.stop().await;
    Ok(())
}

#[tokio::test]
async fn path_diagnostics_unicode_collisions_and_real_bytes_survive_preview_and_reopen() -> Result {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await?);
    let c = db.connect().await?;
    let root = scratch.0.join("root");
    std::fs::create_dir(&root)?;
    let source = root.join("original.mkv");
    std::fs::write(&source, b"original immutable media")?;
    let inode = std::fs::metadata(&source)?;
    seed(
        &c,
        1,
        "Changed",
        root.to_str().unwrap(),
        Some(source.to_str().unwrap()),
    )
    .await?;
    seed(
        &c,
        2,
        "Escape",
        "/owned/root",
        Some("/owned/root-other/file.mkv"),
    )
    .await?;
    seed(
        &c,
        3,
        "Traversal",
        "/owned/three",
        Some("/owned/three/../file.mkv"),
    )
    .await?;
    seed(
        &c,
        4,
        "No extension",
        "/owned/four",
        Some("/owned/four/file"),
    )
    .await?;
    seed(
        &c,
        5,
        &"界".repeat(120),
        "/owned/five",
        Some("/owned/five/old.mkv"),
    )
    .await?;
    seed(
        &c,
        6,
        "Collision",
        "/owned/shared",
        Some("/owned/shared/a.mkv"),
    )
    .await?;
    seed(
        &c,
        7,
        "Collision",
        "/owned/shared/",
        Some("/owned/shared/b.mkv"),
    )
    .await?;
    let api = Api::start(db.clone()).await;
    api.config(true, Some("{Movie Title}")).await;
    let before = number(&c, "PRAGMA data_version").await;
    let value = api.preview("1,2,3,4,5,6,7").await;
    counts(&value);
    assert_eq!(row(&value, 2)["reasons"], json!(["invalid_path"]));
    assert_eq!(row(&value, 2)["existing_path"], Value::Null);
    assert_eq!(row(&value, 3)["reasons"], json!(["invalid_path"]));
    assert_eq!(row(&value, 4)["reasons"], json!(["missing_extension"]));
    let unicode = row(&value, 5)["new_path"].as_str().unwrap();
    assert!(unicode.len() <= 255);
    assert!(unicode.ends_with(".mkv"));
    assert!(unicode.starts_with('界'));
    assert_eq!(row(&value, 6)["new_path"], "Collision.mkv");
    assert_eq!(row(&value, 7)["new_path"], "Collision.mkv");
    assert_eq!(row(&value, 6)["status"], "change");
    assert_eq!(row(&value, 7)["status"], "change");
    assert_eq!(number(&c, "PRAGMA data_version").await, before);
    assert_eq!(std::fs::read(&source)?, b"original immutable media");
    assert!(!root.join("Changed.mkv").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&source)?.ino(), inode.ino());
    }
    assert_eq!(number(&c, "SELECT count(*) FROM operations").await, 0);
    assert_eq!(number(&c, "SELECT count(*) FROM import_journal").await, 0);
    api.stop().await;
    drop(c);
    drop(db);
    let reopened = Database::open_local(path).await?;
    let c = reopened.connect().await?;
    assert_eq!(number(&c, "SELECT count(*) FROM movie_files").await, 7);
    assert_eq!(
        number(
            &c,
            "SELECT count(*) FROM movie_files WHERE id=1 AND path LIKE '%/original.mkv'"
        )
        .await,
        1
    );
    Ok(())
}

fn format_definition(name: &str, pattern: &str, include: bool) -> Value {
    json!({"name":name,"include_when_renaming":include,"specifications":[{"name":"Immutable title","required":false,"negate":false,"condition":{"kind":"release_title","pattern":pattern}}]})
}
#[tokio::test]
async fn current_cold_custom_formats_and_factual_quality_revision_drive_names() -> Result {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    seed(
        &c,
        1,
        "Film",
        "/owned/film",
        Some("/owned/film/Misleading.mkv"),
    )
    .await?;
    // Movie ReleaseTitle conditions use the simplified technical release title.
    // Keep Preferred after the year: putting it in the movie-title prefix tests
    // title stripping instead of immutable-original evidence versus the renamed file.
    c.execute_batch("UPDATE movie_files SET edition='Extended' WHERE id=1;INSERT INTO file_metadata(media_type,movie_file_id,original_release_title,quality_id,revision_json)VALUES('movies',1,'Original.2020.1080p.WEB-DL.Preferred',3,'{\"version\":2,\"real\":1,\"is_repack\":false}');").await?;
    let api = Api::start(db.clone()).await;
    let a = api
        .request(
            "POST",
            "/api/v1/movies/custom-formats",
            Some(format_definition("Alpha", "Preferred", true)),
            201,
        )
        .await;
    let b = api
        .request(
            "POST",
            "/api/v1/movies/custom-formats",
            Some(format_definition("Zulu", "Preferred", true)),
            201,
        )
        .await;
    api.request(
        "POST",
        "/api/v1/movies/custom-formats",
        Some(format_definition("Hidden", "Preferred", false)),
        201,
    )
    .await;
    api.request(
        "POST",
        "/api/v1/tv/custom-formats",
        Some(format_definition("TV only", "Preferred", true)),
        201,
    )
    .await;
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name)VALUES(1,'movies','Score independence');INSERT INTO library_settings(media_type,movie_id,quality_profile_id)VALUES('movies',1,1);").await?;
    c.execute("INSERT INTO quality_profile_format_scores(profile_id,format_id,media_type,score)VALUES(1,?,'movies',-999),(1,?,'movies',999)",params![a["id"].as_i64().unwrap(),b["id"].as_i64().unwrap()]).await?;
    api.config(
        true,
        Some("{Movie Title} ({Release Year}) {Edition Tags} {Quality Full} {[Custom Formats]}"),
    )
    .await;
    let first = api.preview("1").await;
    counts(&first);
    let name = row(&first, 1)["new_path"].as_str().unwrap();
    assert!(name.starts_with("Film (2020) Extended "), "{name}");
    assert!(name.contains("Proper"), "{name}");
    assert!(name.contains("REAL"), "{name}");
    assert!(name.ends_with("[Alpha Zulu].mkv"), "{name}");
    assert!(!name.contains("Hidden") && !name.contains("TV only"));
    c.execute(
        "UPDATE quality_profile_format_scores SET score=-score WHERE profile_id=1",
        (),
    )
    .await?;
    assert_eq!(
        api.preview("1").await,
        first,
        "Rename membership is independent of profile score weights"
    );
    // No score/profile warmup precedes that preview. Each live definition edit must change membership.
    api.request(
        "PUT",
        &format!("/api/v1/movies/custom-formats/{}", a["id"]),
        Some(format_definition("Changed", "Preferred", true)),
        200,
    )
    .await;
    api.config(true, Some("{Movie Title} {Custom Formats:-Zulu}"))
        .await;
    assert_eq!(
        row(&api.preview("1").await, 1)["new_path"],
        "Film Changed.mkv"
    );
    api.config(true, Some("{Movie Title} {Custom Format:Changed}"))
        .await;
    assert_eq!(
        row(&api.preview("1").await, 1)["new_path"],
        "Film Changed.mkv"
    );
    api.request(
        "PUT",
        &format!("/api/v1/movies/custom-formats/{}", a["id"]),
        Some(format_definition("Changed", "DoesNotMatch", true)),
        200,
    )
    .await;
    assert_eq!(row(&api.preview("1").await, 1)["new_path"], "Film.mkv");
    api.config(true, Some("{Movie Title} {Custom Formats}"))
        .await;
    assert_eq!(row(&api.preview("1").await, 1)["new_path"], "Film Zulu.mkv");
    api.request(
        "DELETE",
        &format!("/api/v1/movies/custom-formats/{}", b["id"]),
        None,
        204,
    )
    .await;
    assert_eq!(row(&api.preview("1").await, 1)["new_path"], "Film.mkv");
    api.config(true, Some("{Movie Title} {Quality Title}"))
        .await;
    c.execute(
        "UPDATE file_metadata SET revision_json='{}' WHERE movie_file_id=1",
        (),
    )
    .await?;
    assert_eq!(
        row(&api.preview("1").await, 1)["status"],
        "change",
        "Unused corrupt revision does not poison Quality Title"
    );
    c.execute_batch("DELETE FROM quality_profile_format_scores;DROP TABLE custom_formats;")
        .await?;
    assert_eq!(
        row(&api.preview("1").await, 1)["status"],
        "change",
        "Unused CF storage does not affect Quality Title"
    );
    api.config(true, Some("{Movie Title} {Custom Formats}"))
        .await;
    assert_eq!(
        api.request("GET", &format!("{PREVIEW}?movie_ids=1"), None, 500)
            .await["error"]["code"],
        "database_error"
    );
    api.stop().await;
    Ok(())
}

#[tokio::test]
async fn snapshot_never_combines_different_committed_naming_and_movie_facts() -> Result {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    seed(&c, 1, "A", "/owned/root", Some("/owned/root/old.mkv")).await?;
    let api = Api::start(db.clone()).await;
    api.config(true, Some("A-{Movie Title}")).await;
    let writer = db.connect().await?;
    let edits = tokio::spawn(async move {
        for turn in 0..20 {
            let label = if turn % 2 == 0 { "B" } else { "A" };
            let tx = writer.transaction().await.unwrap();
            tx.execute("UPDATE naming_settings SET standard_movie_format=?,revision=revision+1 WHERE domain='movies'",[format!("{label}-{{Movie Title}}")]).await.unwrap();
            tx.execute("UPDATE movie_metadata SET title=? WHERE id=1001", [label])
                .await
                .unwrap();
            tx.commit().await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    for _ in 0..20 {
        let value = api.preview("1").await;
        let pattern = value["standard_movie_format"].as_str().unwrap();
        let expected = if pattern.starts_with('A') {
            "A-A.mkv"
        } else {
            "B-B.mkv"
        };
        assert_eq!(
            row(&value, 1)["new_path"],
            expected,
            "Captured pattern and title must belong to one committed snapshot"
        );
    }
    edits.await?;
    api.stop().await;
    Ok(())
}

#[tokio::test]
async fn captured_resource_limit_is_explicit_and_does_not_write_or_touch_media() -> Result {
    let scratch = Scratch::new();
    let db = Arc::new(Database::open_local(scratch.0.join("db")).await?);
    let c = db.connect().await?;
    let original = scratch.0.join("original.mkv");
    std::fs::write(&original, b"bounded preview preserves bytes")?;
    seed(
        &c,
        1,
        "Film",
        scratch.0.to_str().unwrap(),
        Some(original.to_str().unwrap()),
    )
    .await?;
    // Naming storage accepts TEXT without a length CHECK; disabled mode need not parse it,
    // but capturing an oversized historical pattern must fail the aggregate resource bound.
    c.execute("UPDATE naming_settings SET standard_movie_format=printf('%.*c',33554433,'x'),revision=revision+1 WHERE domain='movies'",()).await?;
    let version = number(&c, "PRAGMA data_version").await;
    let api = Api::start(db.clone()).await;
    let response = api
        .request("GET", &format!("{PREVIEW}?movie_ids=1"), None, 503)
        .await;
    assert_eq!(response["error"]["code"], "rename_preview_unavailable");
    assert_eq!(number(&c, "PRAGMA data_version").await, version);
    assert_eq!(
        number(
            &c,
            "SELECT length(standard_movie_format) FROM naming_settings WHERE domain='movies'"
        )
        .await,
        33554433
    );
    assert_eq!(
        std::fs::read(&original)?,
        b"bounded preview preserves bytes"
    );
    assert_eq!(number(&c, "SELECT count(*) FROM operations").await, 0);
    api.stop().await;
    Ok(())
}
