//! Host prerequisite only: real binary, owned loopback sockets and scratch databases.
use hrrdarr::{db::Database, host::HostConfig};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-host-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Process {
    child: std::process::Child,
    output: PathBuf,
}
impl Process {
    fn spawn(directory: &Path, bind: &OsStr, hosts: Option<&OsStr>) -> Self {
        let output = directory.join("output");
        let file = std::fs::File::create(&output).unwrap();
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_hrrdarr"));
        command
            .env_clear()
            .env("HRRDARR_DATABASE_PATH", directory.join("db"))
            .env("HRRDARR_BIND", bind)
            .env("HRRDARR_PROVIDER_KEY", "11".repeat(32))
            .env("HOST_TEST_SECRET", "not-for-host-response")
            .current_dir(directory)
            .stdout(file.try_clone().unwrap())
            .stderr(file);
        if let Some(hosts) = hosts {
            command.env("HRRDARR_ALLOWED_HOSTS", hosts);
        }
        Self {
            child: command.spawn().unwrap(),
            output,
        }
    }
    fn output(&self) -> String {
        let mut bytes = Vec::new();
        std::fs::File::open(&self.output)
            .unwrap()
            .take(65537)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 65536, "Unbounded startup output");
        String::from_utf8_lossy(&bytes).into_owned()
    }
    async fn ready(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "Exited: {}",
                self.output()
            );
            if let Some(origin) = self
                .output()
                .lines()
                .find_map(|line| line.strip_prefix("hrrdarr listening on "))
            {
                let origin = origin.to_owned();
                // The line precedes runtime startup: wait for an actual HTTP response.
                if reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_millis(300))
                    .build()
                    .unwrap()
                    .get(format!("{origin}/api/v1/config/host"))
                    .send()
                    .await
                    .is_ok()
                {
                    return origin;
                }
            }
            assert!(
                Instant::now() < deadline,
                "Startup timeout: {}",
                self.output()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn rejected(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(!status.success());
                return;
            }
            assert!(Instant::now() < deadline, "Invalid startup did not exit");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn request(
    origin: &str,
    method: &str,
    path: &str,
    host: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut req = client
        .request(method.parse().unwrap(), format!("{origin}{path}"))
        .header("Host", host)
        .header("X-Api-Key", "not-an-authority-bypass")
        .header("X-Forwarded-Host", "allowed.example")
        .header("X-Forwarded-For", "127.0.0.1")
        .header(
            "Forwarded",
            "for=127.0.0.1;host=allowed.example;proto=https",
        );
    if let Some(body) = body {
        req = req
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = req.send().await.unwrap();
    let code = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        code,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}
async fn raw(origin: &str, target: &str, headers: &str) -> u16 {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
            .await
            .unwrap();
        stream
            .write_all(
                format!("GET {target} HTTP/1.1\r\n{headers}Connection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        stream.take(65537).read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.len() <= 65536);
        let text = String::from_utf8_lossy(&bytes);
        text.split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("No HTTP response: {text}"))
            .parse()
            .unwrap()
    })
    .await
    .expect("Raw request timeout")
}

#[test]
fn host_startup_parser_preserves_default_bind_and_rejects_invalid_configuration() {
    assert_eq!(
        HostConfig::from_values(None, None).unwrap().bind_address(),
        "127.0.0.1:8760".parse().unwrap()
    );
    assert!(HostConfig::from_values(Some("0.0.0.0:0"), None).is_ok());
    assert_eq!(
        HostConfig::from_values(Some("[::1]:0"), Some("[::1]"))
            .unwrap()
            .bind_address(),
        "[::1]:0".parse().unwrap()
    );
    for bind in [
        "",
        "localhost:8760",
        "127.0.0.1",
        "127.0.0.1:65536",
        "http://127.0.0.1:8760",
        " 127.0.0.1:8760",
    ] {
        assert!(
            HostConfig::from_values(Some(bind), None).is_err(),
            "Accepted bind {bind:?}"
        );
    }
    for hosts in [
        "*",
        "*.example.com",
        "allowed.example:80",
        "https://allowed.example",
        "user@allowed.example",
        "allowed.example/path",
        "allowed.example?x",
        "allowed.example#x",
        "allowed.example\n",
        "\t",
        "allowed.example\r",
        "éxample.com",
        "127.1",
        "::1",
        "-bad.example",
        "bad-.example",
        "allowed.example\0",
        "allowed.example,,other.example",
    ] {
        assert!(
            HostConfig::from_values(None, Some(hosts)).is_err(),
            "Accepted hosts {hosts:?}"
        );
    }
    let boundary = [
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61),
    ]
    .join(".");
    assert_eq!(boundary.len(), 253);
    assert!(HostConfig::from_values(None, Some(&boundary)).is_ok());
    assert!(HostConfig::from_values(None, Some(&(boundary + "x"))).is_err());
    assert!(HostConfig::from_values(None, Some(&"a".repeat(64))).is_err());
    assert!(HostConfig::from_values(None, Some(&vec!["a.example"; 64].join(","))).is_ok());
    assert!(HostConfig::from_values(None, Some(&vec!["a.example"; 65].join(","))).is_err());
    assert!(HostConfig::from_values(None, Some(&" ".repeat(16384))).is_ok());
    assert!(HostConfig::from_values(None, Some(&" ".repeat(16385))).is_err());
}

#[tokio::test]
async fn configured_host_filter_guards_real_routes_and_preserves_both_domains() {
    let scratch = Scratch::new();
    let mut process = Process::spawn(
        &scratch.0,
        OsStr::new("127.0.0.1:0"),
        Some(OsStr::new(
            "Allowed.Example,allowed.example,127.0.0.1,[0:0:0:0:0:0:0:1]",
        )),
    );
    let origin = process.ready().await;
    let (code, settings) = request(
        &origin,
        "GET",
        "/api/v1/config/host",
        "ALLOWED.EXAMPLE.:443",
        None,
    )
    .await;
    assert_eq!(code, 200, "{settings}");
    assert_eq!(
        settings,
        json!({"authentication":"none","configured_bind":"127.0.0.1:0","bound_address":origin.strip_prefix("http://").unwrap(),"bind_source":"environment","allowed_hosts":["127.0.0.1","[::1]","allowed.example"],"allowed_hosts_source":"environment","filtering_enabled":true,"mutability":"deployment","apply_mode":"process_restart","capabilities":{"persisted_edits":false,"tls":false,"url_base":false,"trusted_forwarding":false}})
    );
    let serialized = settings.to_string();
    for secret in [
        "not-for-host-response",
        &"11".repeat(32),
        scratch.0.to_str().unwrap(),
        "HRRDARR_PROVIDER_KEY",
    ] {
        assert!(!serialized.contains(secret));
    }
    for host in [
        "allowed.example",
        "allowed.example:1",
        "127.0.0.1:65535",
        "[::1]",
        "[0:0:0:0:0:0:0:1]:8080",
    ] {
        assert_eq!(
            request(&origin, "GET", "/api/v1/config/host", host, None)
                .await
                .0,
            200,
            "{host}"
        );
    }
    for rejected in [
        "evilallowed.example",
        "allowed.example.evil",
        "sub.allowed.example",
    ] {
        let (code, body) = request(&origin, "GET", "/api/v1/config/host", rejected, None).await;
        assert_eq!(code, 403, "{rejected}: {body}");
        assert_eq!(body["error"]["code"], "host_not_allowed");
    }
    for domain in ["tv", "movies"] {
        let path = format!("/api/v1/{domain}/tags");
        assert_eq!(
            request(
                &origin,
                "POST",
                &path,
                "evil.example",
                Some(json!({"label":"denied"}))
            )
            .await
            .0,
            403
        );
        assert_eq!(
            request(&origin, "GET", &path, "allowed.example", None)
                .await
                .0,
            200
        );
        let (code, tag) = request(
            &origin,
            "POST",
            &path,
            "allowed.example",
            Some(json!({"label":"permitted"})),
        )
        .await;
        assert_eq!(code, 201, "{tag}");
        assert_eq!(tag["label"], "permitted");
    }
    for (method, path) in [
        ("GET", "/unknown-host-test"),
        (
            "POST",
            "/api/v1/migrations?application=sonarr&dry_run=false",
        ),
        ("GET", "/api/v1/filesystem?path=/"),
        ("POST", "/api/v1/imports"),
        ("POST", "/api/v1/providers"),
        ("PUT", "/api/v1/config/host"),
    ] {
        let (code, body) = request(&origin, method, path, "evil.example", Some(json!({}))).await;
        assert_eq!(code, 403, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], "host_not_allowed");
        assert!(!body.to_string().contains("evil.example"));
    }
    assert_eq!(
        request(
            &origin,
            "PUT",
            "/api/v1/config/host",
            "allowed.example",
            Some(json!({}))
        )
        .await
        .0,
        405
    );
    // Read the actual database: disallowed mutations never reached either domain handler.
    drop(process);
    let db = Database::open_local(scratch.0.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let mut rows = c
        .query("SELECT media_type,label FROM tags ORDER BY media_type", ())
        .await
        .unwrap();
    for domain in ["movies", "tv"] {
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), domain);
        assert_eq!(row.get::<String>(1).unwrap(), "permitted");
    }
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    drop(c);
    drop(db);
    // Deployment changes become effective only in a new process, preserving persisted domain data.
    let mut restarted = Process::spawn(
        &scratch.0,
        OsStr::new("127.0.0.1:0"),
        Some(OsStr::new("replacement.example")),
    );
    let origin = restarted.ready().await;
    assert_eq!(
        request(
            &origin,
            "GET",
            "/api/v1/config/host",
            "allowed.example",
            None
        )
        .await
        .0,
        403
    );
    for domain in ["tv", "movies"] {
        assert_eq!(
            request(
                &origin,
                "GET",
                &format!("/api/v1/{domain}/tags"),
                "replacement.example",
                None
            )
            .await
            .0,
            200
        );
    }
}

#[tokio::test]
async fn raw_authority_validation_rejects_ambiguity_and_ignores_forwarding() {
    let scratch = Scratch::new();
    let mut process = Process::spawn(
        &scratch.0,
        OsStr::new("127.0.0.1:0"),
        Some(OsStr::new("allowed.example")),
    );
    let origin = process.ready().await;
    for headers in [
        "",
        "Host: allowed.example\r\nHost: allowed.example\r\n",
        "Host: allowed.example\r\nHost: evil.example\r\n",
        "Host: allowed.example,evil.example\r\n",
        "Host: user@allowed.example\r\n",
        "Host: allowed.example/path\r\n",
        "Host: allowed.example:65536\r\n",
        "Host: [::1\r\n",
        "Host: allowed.example:\r\n",
        "Host: allowed.example\x01\r\n",
    ] {
        assert_eq!(
            raw(&origin, "/api/v1/config/host", headers).await,
            400,
            "{headers:?}"
        );
    }
    assert_eq!(
        raw(
            &origin,
            "http://evil.example/api/v1/config/host",
            "Host: allowed.example\r\n"
        )
        .await,
        400
    );
    assert_eq!(
        raw(
            &origin,
            "http://allowed.example/api/v1/config/host",
            "Host: allowed.example\r\n"
        )
        .await,
        200
    );
    // Scheme default ports compare equal; a genuinely different authority port is ambiguous.
    assert_eq!(
        raw(
            &origin,
            "http://allowed.example/api/v1/config/host",
            "Host: allowed.example:80\r\n"
        )
        .await,
        200
    );
    assert_eq!(
        raw(
            &origin,
            "http://allowed.example:81/api/v1/config/host",
            "Host: allowed.example:80\r\n"
        )
        .await,
        400
    );
    assert_eq!(raw(&origin,"/api/v1/config/host","Host: evil.example\r\nX-Forwarded-Host: allowed.example\r\nForwarded: host=allowed.example\r\nX-Api-Key: sentinel\r\n").await,403);
}

#[tokio::test]
async fn absent_and_empty_allowlists_disable_filtering() {
    for hosts in [None, Some(OsStr::new("")), Some(OsStr::new("   "))] {
        let scratch = Scratch::new();
        let mut process = Process::spawn(&scratch.0, OsStr::new("127.0.0.1:0"), hosts);
        let origin = process.ready().await;
        let (code, settings) = request(
            &origin,
            "GET",
            "/api/v1/config/host",
            "arbitrary.example:456",
            None,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(settings["filtering_enabled"], false);
        assert_eq!(settings["allowed_hosts"], json!([]));
        assert_eq!(
            settings["allowed_hosts_source"],
            if hosts.is_some() {
                "environment"
            } else {
                "default"
            }
        );
    }
}

#[tokio::test]
async fn invalid_startup_is_rejected_before_database_creation_or_modification() {
    let mut invalid = vec![
        (OsString::from("not-a-bind"), None),
        (
            OsString::from("127.0.0.1:0"),
            Some(OsString::from("https://private-sentinel.invalid")),
        ),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        invalid.push((OsString::from_vec(vec![0xff]), None));
        invalid.push((
            OsString::from("127.0.0.1:0"),
            Some(OsString::from_vec(vec![0xff])),
        ));
    }
    for (bind, hosts) in invalid {
        for existing in [false, true] {
            let scratch = Scratch::new();
            let path = scratch.0.join("db");
            if existing {
                std::fs::write(&path, b"sentinel existing database must not be opened").unwrap();
            }
            let mut process = Process::spawn(&scratch.0, &bind, hosts.as_deref());
            process.rejected().await;
            let output = process.output();
            assert!(
                output
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|event| event["event"] == "process_failed"
                        && event["phase"] == "host_configuration"),
                "{output}"
            );
            assert!(!output.contains("private-sentinel"));
            assert!(!output.contains("hrrdarr listening on"));
            if existing {
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    b"sentinel existing database must not be opened"
                );
            } else {
                assert!(!path.exists());
            }
            let mut files = std::fs::read_dir(&scratch.0)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>();
            files.sort();
            assert_eq!(
                files,
                if existing {
                    vec![OsString::from("db"), OsString::from("output")]
                } else {
                    vec![OsString::from("output")]
                }
            );
        }
    }
}
