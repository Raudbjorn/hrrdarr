use serde_json::Value;
use std::{
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
    path::PathBuf,
    time::Duration,
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-filesystem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn path(&self, n: &str) -> String {
        self.0.join(n).to_str().unwrap().into()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn get(
    client: &reqwest::Client,
    base: &str,
    endpoint: &str,
    query: &[(&str, &str)],
) -> (u16, Value) {
    let mut url = url::Url::parse(&format!("{base}/api/v1/filesystem{endpoint}")).unwrap();
    url.query_pairs_mut().extend_pairs(query.iter().copied());
    let response = client.get(url).send().await.unwrap();
    (
        response.status().as_u16(),
        serde_json::from_str(&response.text().await.unwrap()).unwrap(),
    )
}
#[tokio::test]
async fn native_lookup_classification_and_both_media_inventories_use_only_scratch_paths() {
    let s = Scratch::new();
    for dir in [
        "root",
        "root/A",
        "root/z",
        "root/cache",
        "outside",
        "denied",
    ] {
        std::fs::create_dir(s.path(dir)).unwrap();
    }
    for (path, bytes) in [
        ("root/plain.txt", b"text".as_slice()),
        ("root/Movie.MKV", b"media"),
        ("root/A/deep.mp4", b"deep"),
        ("root/stereo.MK3D", b"stereo"),
        ("root/cache/hidden-folder.mp4", b"video"),
        ("outside/escape.mkv", b"outside"),
    ] {
        std::fs::write(s.path(path), bytes).unwrap();
    }
    symlink(s.path("outside"), s.path("root/escape")).unwrap();
    symlink(s.path("outside/escape.mkv"), s.path("root/link.mkv")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(s.path("root/socket")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, hrrdarr::filesystem::router())
            .await
            .unwrap()
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let root = s.path("root");
    let (code, body) = get(&client, &base, "", &[("path", &format!("{root}/"))]).await;
    assert_eq!(code, 200);
    assert!(body["files"].as_array().unwrap().is_empty());
    assert_eq!(
        body["directories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["A", "z"]
    );
    assert_eq!(body["parent"], format!("{}/", s.0.display()));
    assert!(
        body["directories"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with('/')
    );
    assert!(body["directories"][0]["last_modified"].is_string());
    assert!(body["directories"][0]["size"].is_null());
    let (_, body) = get(
        &client,
        &base,
        "",
        &[("path", &format!("{root}/")), ("include_files", "true")],
    )
    .await;
    assert_eq!(body["files"].as_array().unwrap().len(), 3);
    assert_eq!(body["files"][0]["name"], "Movie.MKV");
    assert_eq!(body["files"][0]["extension"], ".MKV");
    assert_eq!(body["files"][0]["size"], 5);
    // An unfinished basename requests its parent; there is no implicit prefix filter.
    let (_, body) = get(
        &client,
        &base,
        "",
        &[("path", &format!("{root}/not-a-prefix"))],
    )
    .await;
    assert_eq!(body["path"], format!("{root}/"));
    assert_eq!(body["directories"].as_array().unwrap().len(), 2);
    let (_, body) = get(&client, &base, "", &[("path", &format!("{root}/A"))]).await;
    assert_eq!(body["path"], format!("{root}/"));
    let (_, body) = get(
        &client,
        &base,
        "",
        &[
            ("path", &format!("{root}/A")),
            ("allow_folders_without_trailing_slashes", "true"),
            ("include_files", "true"),
        ],
    )
    .await;
    assert_eq!(body["path"], format!("{root}/A/"));
    assert_eq!(body["files"][0]["name"], "deep.mp4");
    for (p, expected) in [
        ("root/Movie.MKV", "file"),
        ("root/Movie.MKV/", "folder"),
        ("root/A", "folder"),
        ("root/absent", "folder"),
        ("root/link.mkv", "folder"),
        ("root/socket", "folder"),
    ] {
        let (code, body) = get(&client, &base, "/type", &[("path", &s.path(p))]).await;
        assert_eq!(code, 200);
        assert_eq!(body["type"], expected);
    }
    for (domain, count) in [("tv", 3), ("movies", 4)] {
        let (code, body) = get(
            &client,
            &base,
            "/media-files",
            &[("path", &root), ("media_type", domain)],
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(body["files"].as_array().unwrap().len(), count);
        assert_eq!(body["files"][0]["relative_path"], "A/deep.mp4");
        assert!(
            body["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["relative_path"] == "cache/hidden-folder.mp4")
        );
        assert_eq!(
            body["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "stereo.MK3D"),
            domain == "movies"
        );
    }
    for endpoint in ["", "/media-files"] {
        let (code, body) = get(
            &client,
            &base,
            endpoint,
            &[("path", format!("{root}/escape/").as_str())]
                .into_iter()
                .chain((endpoint == "/media-files").then_some(("media_type", "tv")))
                .collect::<Vec<_>>(),
        )
        .await;
        assert_eq!(code, 503);
        assert_eq!(body["error"]["code"], "filesystem_unavailable");
    }
    for args in [
        vec![("path", "relative")],
        vec![("path", "/a/../b")],
        vec![("path", "/a//b")],
        vec![("path", "/a/./b")],
        vec![("path", root.as_str()), ("include_files", "yes")],
        vec![("path", root.as_str()), ("extra", "true")],
    ] {
        assert_eq!(get(&client, &base, "", &args).await.0, 400);
    }
    assert_eq!(
        get(&client, &base, "/media-files", &[("path", &root)])
            .await
            .0,
        400
    );
    let (code, body) = get(
        &client,
        &base,
        "/media-files",
        &[("path", &s.path("missing")), ("media_type", "movies")],
    )
    .await;
    assert_eq!(code, 503);
    assert!(body.get("files").is_none());
    // Real permission failure while running as the ordinary project user.
    std::fs::set_permissions(s.path("denied"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = get(
        &client,
        &base,
        "",
        &[("path", &format!("{}/", s.path("denied")))],
    )
    .await;
    std::fs::set_permissions(s.path("denied"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(denied.0, 503);
    std::fs::write(
        s.0.join("root")
            .join(std::ffi::OsString::from_vec(vec![0xff])),
        b"bad-name",
    )
    .unwrap();
    let (code, body) = get(&client, &base, "", &[("path", &format!("{root}/"))]).await;
    assert_eq!(code, 503);
    assert_eq!(body["error"]["code"], "filesystem_non_utf8_name");
    server.abort();
    let _ = server.await;
}
