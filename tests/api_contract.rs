//! Wire specimens are obtained from real handlers or serialized actual request/response DTOs.
use hrrdarr::{
    api, api_contract, db::Database, episodes, library, media_files, qualities, quality_profiles,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::Duration,
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-contract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
// Capture to a temporary file to avoid pipe backpressure; bound time and diagnostic reads.
fn run_bounded(command: &mut Command) -> std::process::Output {
    let dir = Sandbox::new();
    let path = dir.0.join("output");
    let output = std::fs::File::create(&path).unwrap();
    command.stdout(output.try_clone().unwrap()).stderr(output);
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            child.wait().unwrap();
            panic!("contract subprocess exceeded 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .unwrap();
    std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: bytes,
    }
}
fn round_trip<T: DeserializeOwned + Serialize>(input: Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(input).unwrap()).unwrap()
}
#[test]
fn deterministic_contract_and_stale_cli_check() {
    assert_eq!(api_contract::render(), api_contract::render());
    api_contract::check().expect("committed TypeScript must match actual Rust DTOs");
    let dir = Sandbox::new();
    let path = dir.0.join("contract.ts");
    std::fs::write(&path, api_contract::render()).unwrap();
    let run = || {
        run_bounded(
            Command::new(env!("CARGO_BIN_EXE_generate-api"))
                .arg("--check")
                .arg(&path),
        )
    };
    assert!(run().status.success());
    std::fs::write(&path, "stale").unwrap();
    let stale = run();
    assert!(!stale.status.success());
    assert!(String::from_utf8(stale.stderr).unwrap().contains("stale"));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "stale",
        "check must never repair/mutate output"
    );
}
#[test]
fn custom_serde_presence_flatten_and_numeric_wire_contracts() {
    for patch in [
        json!({}),
        json!({"quality_profile_id":null}),
        json!({"series_type":"anime","monitored":true}),
    ] {
        assert_eq!(round_trip::<library::Patch>(patch.clone()), patch);
    }
    for value in [
        json!({"id":7}),
        json!({"id":7,"quality":null,"languages":[0,1],"release_group":null}),
    ] {
        assert_eq!(round_trip::<media_files::FileUpdate>(value.clone()), value);
    }
    let editor =
        json!({"file_ids":[7,8],"edition":null,"quality":{"quality_id":1,"revision":null}});
    assert_eq!(
        round_trip::<media_files::FileEditor>(editor.clone()),
        editor
    );
    assert!(
        serde_json::from_value::<media_files::FileUpdate>(json!({"id":7,"typo":true})).is_err()
    );
    assert!(
        serde_json::from_value::<media_files::FileEditor>(json!({"file_ids":[7],"typo":true}))
            .is_err()
    );
    for cover in [
        json!({"coverType":"poster"}),
        json!({"coverType":"banner","url":null}),
        json!({"coverType":"screenshot","remoteUrl":"https://example.test/image"}),
    ] {
        assert_eq!(round_trip::<episodes::EpisodeCover>(cover.clone()), cover);
    }
    let request = api::ImportRequest {
        episode_id: i64::MAX,
        source: "s".into(),
        mode: "copy".into(),
        destination: "d".into(),
    };
    assert!(
        serde_json::to_string(&request)
            .unwrap()
            .contains("9223372036854775807")
    );
    // Numeric transport is unchanged; the frontend rejects this value after JSON decoding.
    assert_eq!(
        serde_json::to_value(request).unwrap()["episode_id"].as_i64(),
        Some(i64::MAX)
    );
}
async fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, Value) {
    let message = format!(
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
        stream.write_all(message.as_bytes()).unwrap();
        let mut response = String::new();
        stream
            .take(1024 * 1024)
            .read_to_string(&mut response)
            .unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        (
            headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            serde_json::from_str(body).unwrap(),
        )
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn actual_handler_wire_specimens_typecheck_against_generated_types() {
    let dir = Sandbox::new();
    let db = Arc::new(
        Database::open_local(dir.0.join("library.db"))
            .await
            .unwrap(),
    );
    let c = db.connect().await.unwrap();
    c.execute_batch(r#"
        INSERT INTO series(id,title,path) VALUES(1,'TV','/tv');
        INSERT INTO seasons VALUES(1,1,1);
        INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Episode'),(2,1,1,2,'Unknown images'),(3,1,1,3,'Empty images');
        INSERT INTO episode_files VALUES(7,1,'/tv/file.mkv');
        UPDATE episodes SET episode_file_id=7,images_json='[{"coverType":"poster","url":null},{"coverType":"banner"}]' WHERE id=1;
        UPDATE episodes SET images_json='[]' WHERE id=3;
        INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movie');
        INSERT INTO movie_files(id,movie_id,path) VALUES(7,1,'/movie/file.mkv');
    "#).await.unwrap();
    let app = library::router(db.clone())
        .merge(episodes::router(db.clone()))
        .merge(media_files::router(db.clone()))
        .merge(qualities::router(db.clone()))
        .merge(quality_profiles::router(db.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut specimens = Vec::<(String, Value)>::new();
    for (path, ty) in [
        ("/api/v1/series", "Array<LegacySeries>"),
        ("/api/v1/series/1/episodes", "Array<LegacyEpisode>"),
        ("/api/v1/tv/series", "LibraryPage"),
        ("/api/v1/movies/1", "LibraryItem"),
        ("/api/v1/episodes?series_id=1", "ApiPage<Episode>"),
        ("/api/v1/episodes/1", "Episode"),
        ("/api/v1/episodes/2", "Episode"),
        ("/api/v1/episodes/3", "Episode"),
        ("/api/v1/tv/files?series_id=1", "ApiPage<FileResource>"),
        ("/api/v1/movies/files/7", "MovieFileResource"),
        ("/api/v1/tv/quality-definitions", "Array<QualityDefinition>"),
        (
            "/api/v1/movies/quality-definitions/limits",
            "QualityDefinitionLimits",
        ),
        ("/api/v1/tv/quality-profiles", "QualityProfilePage"),
    ] {
        let (status, value) = request(addr, "GET", path, "").await;
        assert_eq!(status, 200, "{path}: {value}");
        if path == "/api/v1/episodes?series_id=1" {
            assert!(value["items"][0].get("images").is_none());
        }
        if path == "/api/v1/episodes/1" {
            assert_eq!(value["images"][0]["url"], Value::Null);
            assert!(value["images"][1].get("url").is_none());
        }
        if path == "/api/v1/episodes/2" {
            assert_eq!(value.get("images"), Some(&Value::Null));
        }
        if path == "/api/v1/episodes/3" {
            assert_eq!(value["images"], json!([]));
        }
        specimens.push((ty.into(), value));
    }
    // Serialize accepted request DTOs and compile both request and response envelopes.
    let bulk = round_trip::<media_files::FileBulk>(json!({"files":[{"id":7,"languages":null}]}));
    let (status, result) = request(addr, "PUT", "/api/v1/tv/files/bulk", &bulk.to_string()).await;
    assert_eq!(status, 200);
    specimens.push(("FileBulk".into(), bulk));
    specimens.push(("Array<FileResource>".into(), result));
    let bulk = json!({"items":[{"id":1,"patch":{"monitored":true}}]});
    let _: library::Bulk = serde_json::from_value(bulk.clone()).unwrap();
    let (status, result) = request(addr, "PUT", "/api/v1/movies/bulk", &bulk.to_string()).await;
    assert_eq!(status, 200);
    specimens.push(("LibraryBulk".into(), bulk));
    specimens.push(("Array<LibraryItem>".into(), result));
    specimens.push((
        "ImportRequest".into(),
        serde_json::to_value(api::ImportRequest {
            episode_id: 1,
            source: "/source".into(),
            mode: "copy".into(),
            destination: "/destination".into(),
        })
        .unwrap(),
    ));
    let input =
        json!({"name":"Profile","items":[{"kind":"quality","quality_id":1,"allowed":true}]});
    // The actual deserializer accepts omission while the actual response must emit explicit nulls.
    let parsed: quality_profiles::ProfileInput = serde_json::from_value(input.clone()).unwrap();
    specimens.push((
        "QualityProfileInput".into(),
        serde_json::to_value(parsed).unwrap(),
    ));
    let (status, profile) = request(
        addr,
        "POST",
        "/api/v1/tv/quality-profiles",
        &input.to_string(),
    )
    .await;
    assert_eq!(status, 201, "{profile}");
    assert_eq!(profile["items"][0].get("min_size"), Some(&Value::Null));
    specimens.push(("QualityProfile".into(), profile));
    // Presence is parsed first; null monitoring is rejected by the actual domain validator.
    let (status, _) = request(addr, "PUT", "/api/v1/tv/series/1", r#"{"monitored":null}"#).await;
    assert_eq!(status, 400);
    let (status, error) = request(addr, "GET", "/api/v1/episodes/999", "").await;
    assert_eq!(status, 404);
    specimens.push(("ApiErrorEnvelope".into(), error));
    specimens.push((
        "LegacyError".into(),
        serde_json::to_value(api::LegacyError {
            error: "Example".into(),
        })
        .unwrap(),
    ));
    specimens.push((
        "Operation".into(),
        serde_json::to_value(api::Operation {
            id: uuid::Uuid::nil(),
            target: hrrdarr::db::MediaTarget::Episode(1),
            status: "preview",
            message: "Example".into(),
        })
        .unwrap(),
    ));
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    std::fs::write(dir.0.join("api.generated.ts"), api_contract::render()).unwrap();
    let names = specimens
        .iter()
        .flat_map(|(ty, _)| ty.split(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|name| !name.is_empty() && *name != "Array")
        .collect::<std::collections::BTreeSet<_>>();
    let mut ts = format!(
        "import type {{ {} }} from './api.generated';\n",
        names.into_iter().collect::<Vec<_>>().join(",")
    );
    for (i, (ty, value)) in specimens.iter().enumerate() {
        ts.push_str(&format!("const specimen{i} = {value} satisfies {ty};\n"));
    }
    ts.push_str("// @ts-expect-error required nullable output fields cannot be omitted\nconst missing: LegacySeries = {id:1,title:'TV',path:'/tv'};\n// @ts-expect-error actual closed domain discriminators\nconst domain: LibraryItem['media_type'] = 'episode';\n");
    ts.push_str("// @ts-expect-error monitoring null is rejected by the handler\nconst patch: import('./api.generated').LibraryPatch = {monitored:null};\n// @ts-expect-error query null is not an omitted URL scalar\nconst query: import('./api.generated').EpisodeQuery = {limit:null};\n// @ts-expect-error profile response sizes are required even when null\nconst leaf: import('./api.generated').QualityProfileLeaf = {quality_id:1,allowed:true};\n");
    std::fs::write(dir.0.join("specimens.ts"), ts).unwrap();
    let compiler =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/node_modules/typescript/bin/tsc");
    assert!(
        compiler.exists(),
        "Install locked frontend dependencies before contract verification"
    );
    let result = run_bounded(
        Command::new("node")
            .arg(compiler)
            .args([
                "--strict",
                "--noEmit",
                "--skipLibCheck",
                "--target",
                "ES2022",
                "--module",
                "ESNext",
                "--moduleResolution",
                "bundler",
            ])
            .arg(dir.0.join("specimens.ts")),
    );
    assert!(
        result.status.success(),
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
