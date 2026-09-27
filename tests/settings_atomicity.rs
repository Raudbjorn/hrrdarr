//! Native DB settings contracts: concurrent HTTP writes, late rollback and orderly reopen.
use hrrdarr::{db::Database, library, naming, providers, quality_profiles};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hrrdarr-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn serve(db: Arc<Database>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let key = Arc::new(providers::CredentialKey::from_hex(&"12".repeat(32)).unwrap());
    let app = library::router(db.clone())
        .merge(naming::router(db.clone()))
        .merge(quality_profiles::router(db.clone()))
        .merge(providers::router(db, Some(key)));
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn request(base: &str, method: &str, route: &str, body: Value) -> (u16, Value) {
    // Each invocation owns a client/connection; no shared HTTP pool serializes competing writes.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let response = client
        .request(method.parse().unwrap(), format!("{base}{route}"))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn race(base: &str, route: &str, a: Value, b: Value) -> [(u16, Value); 2] {
    let barrier = tokio::sync::Barrier::new(2);
    let send = |body| async {
        tokio::time::timeout(Duration::from_secs(10), barrier.wait())
            .await
            .unwrap();
        request(base, "PUT", route, body).await
    };
    let (a, b) = tokio::join!(send(a), send(b));
    [a, b]
}
async fn stop(server: tokio::task::JoinHandle<()>) {
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_library_patches_survive_and_failed_multirow_write_reopens_old_state() {
    let scratch = Sandbox::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    let mut saved = Vec::new();
    for (domain, collection, setting, value) in [
        ("tv", "/api/v1/tv/series", "series_type", "anime"),
        (
            "movies",
            "/api/v1/movies",
            "minimum_availability",
            "released",
        ),
    ] {
        let (status, item) = request(
            &base,
            "POST",
            collection,
            json!({"title":domain,"path":format!("/declared/{domain}"),if domain == "tv" { "tvdb_id" } else { "tmdb_id" }:100}),
        )
        .await;
        assert_eq!(status, 201, "{item}");
        let route = format!("{collection}/{}", item["id"]);
        let results = race(
            &base,
            &route,
            json!({"monitored":false}),
            json!({setting:value}),
        )
        .await;
        for result in results {
            assert_eq!(result.0, 200, "{result:?}");
        }
        let (status, committed) = request(&base, "GET", &route, Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(committed["monitored"], false);
        assert_eq!(committed["settings"][setting], value);
        // patch() changes the settings row first, then the parent monitoring row: fail late.
        let table = if domain == "tv" { "series" } else { "movies" };
        let conn = db.connect().await.unwrap();
        conn.execute_batch(&format!("CREATE TRIGGER fail_settings BEFORE UPDATE OF monitored ON {table} BEGIN SELECT RAISE(ABORT,'injected settings failure'); END;")).await.unwrap();
        let (status, _) =
            request(&base, "PUT", &route, json!({"monitored":true,setting:null})).await;
        assert_eq!(status, 500);
        conn.execute_batch("DROP TRIGGER fail_settings")
            .await
            .unwrap();
        assert_eq!(
            request(&base, "GET", &route, Value::Null).await.1,
            committed
        );
        saved.push((route, committed));
    }
    stop(server).await;
    drop(db);
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db).await;
    for (route, expected) in saved {
        assert_eq!(
            request(&base, "GET", &route, Value::Null).await,
            (200, expected)
        );
    }
    stop(server).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn naming_and_shared_provider_same_revision_choose_one_complete_winner() {
    let scratch = Sandbox::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    let mut saved = Vec::new();
    for domain in ["tv", "movies"] {
        let route = format!("/api/v1/{domain}/config/naming");
        let (status, mut a) = request(&base, "GET", &route, Value::Null).await;
        assert_eq!(status, 200);
        let mut b = a.clone();
        a["colon_replacement"] = json!("delete");
        b["colon_replacement"] = json!("dash");
        b["replace_illegal_characters"] = json!(false);
        let inputs = [a.clone(), b.clone()];
        let results = race(&base, &route, a, b).await;
        let mut statuses = results.each_ref().map(|r| r.0);
        statuses.sort();
        assert_eq!(statuses, [200, 409], "{results:?}");
        let winner_index = results.iter().position(|r| r.0 == 200).unwrap();
        let mut expected = inputs[winner_index].clone();
        expected["revision"] = json!(2);
        let winner = results.into_iter().find(|r| r.0 == 200).unwrap().1;
        assert_eq!(winner, expected);
        assert_eq!(winner["revision"], 2);
        saved.push((route, winner));
    }
    let input = json!({"name":"Shared","enabled":true,"priority":10,"settings":{"implementation":"torznab","endpoint":format!("{base}/owned-no-network"),"tv":{"categories":[5030],"anime_categories":[]},"movies":{"categories":[2000]}},"credentials":{"kind":"api_key","api_key":"ORIGINAL_PRIVATE"}});
    let (status, created) = request(&base, "POST", "/api/v1/providers", input.clone()).await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap();
    let route = format!("/api/v1/providers/{id}");
    let mut a = input.clone();
    a["revision"] = json!(1);
    a["name"] = json!("A");
    a["settings"]["tv"]["categories"] = json!([5040]);
    a["credentials"] = json!({"kind":"api_key","api_key":"A_REPLACEMENT_PRIVATE"});
    let mut b = input;
    b["revision"] = json!(1);
    b["name"] = json!("B");
    b["settings"]["movies"]["categories"] = json!([2040]);
    b["credentials"] = json!({"kind":"api_key","api_key":"REPLACEMENT_PRIVATE"});
    let results = race(&base, &route, a, b).await;
    let mut statuses = results.each_ref().map(|r| r.0);
    statuses.sort();
    assert_eq!(statuses, [200, 409], "{results:?}");
    let winner = results.into_iter().find(|r| r.0 == 200).unwrap().1;
    assert_eq!(winner["revision"], 2);
    let is_a = winner["name"] == "A";
    assert_eq!(winner["has_credentials"], true);
    assert_eq!(
        winner["settings"]["tv"]["categories"],
        if is_a { json!([5040]) } else { json!([5030]) }
    );
    assert_eq!(
        winner["settings"]["movies"]["categories"],
        if is_a { json!([2000]) } else { json!([2040]) }
    );
    let conn = db.connect().await.unwrap();
    let encrypted = conn
        .query("SELECT credentials FROM providers WHERE id=?", [id])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Option<Vec<u8>>>(0)
        .unwrap();
    assert!(encrypted.is_some());
    {
        let envelope = encrypted.unwrap();
        // Independently verify the persisted v1 AEAD contract and the exact winning secret.
        use ring::aead;
        assert_eq!(envelope[0], 1);
        let key =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &[0x12; 32]).unwrap());
        let nonce = aead::Nonce::assume_unique_for_key(envelope[1..13].try_into().unwrap());
        let mut ciphertext = envelope[13..].to_vec();
        let plaintext = key
            .open_in_place(
                nonce,
                aead::Aad::from(format!("hrrdarr/provider-credentials/v1/torznab/{id}")),
                &mut ciphertext,
            )
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(plaintext).unwrap(),
            json!({"kind":"api_key","api_key":if is_a {"A_REPLACEMENT_PRIVATE"} else {"REPLACEMENT_PRIVATE"}})
        );
    }
    saved.push((route, winner));
    drop(conn);
    stop(server).await;
    drop(db);
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db).await;
    for (route, expected) in saved {
        assert_eq!(
            request(&base, "GET", &route, Value::Null).await,
            (200, expected)
        );
    }
    stop(server).await;
}

fn profile(domain: &str, name: &str, quality: i64) -> Value {
    json!({"name":name,"items":[{"kind":"group","name":name,"allowed":true,"items":[{"quality_id":quality,"allowed":true}]}],"policy":{"upgrade_allowed":name != "B","cutoff":{"kind":"group","position":0},"min_format_score":0,"cutoff_format_score":0,"min_upgrade_format_score":1,"language_id":if domain == "movies" {json!(-2)} else {Value::Null},"format_items":[]}})
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_profile_replacements_never_mix_parent_children_or_policy_and_rollback_reopens() {
    let scratch = Sandbox::new();
    let path = scratch.0.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db.clone()).await;
    let mut saved = Vec::new();
    for domain in ["tv", "movies"] {
        let collection = format!("/api/v1/{domain}/quality-profiles");
        let (status, created) =
            request(&base, "POST", &collection, profile(domain, "Original", 1)).await;
        assert_eq!(status, 201, "{created}");
        let route = format!("{collection}/{}", created["id"]);
        let results = race(
            &base,
            &route,
            profile(domain, "A", 1),
            profile(domain, "B", 4),
        )
        .await;
        for result in &results {
            assert_eq!(result.0, 200, "{result:?}");
        }
        let (status, winner) = request(&base, "GET", &route, Value::Null).await;
        assert_eq!(status, 200);
        assert!(results.iter().any(|r| r.1 == winner));
        let expected_quality = if winner["name"] == "A" { 1 } else { 4 };
        assert_eq!(winner["policy"]["upgrade_allowed"], winner["name"] == "A");
        assert_eq!(winner["items"][0]["name"], winner["name"]);
        assert_eq!(
            winner["items"][0]["items"][0]["quality_id"],
            expected_quality
        );
        // Policy is written after deleting/rebuilding children: no hybrid survives this late failure.
        let conn = db.connect().await.unwrap();
        conn.execute_batch("CREATE TRIGGER fail_policy BEFORE INSERT ON quality_profile_policies BEGIN SELECT RAISE(ABORT,'injected policy failure'); END;").await.unwrap();
        assert_eq!(
            request(&base, "PUT", &route, profile(domain, "Failed", 7))
                .await
                .0,
            500
        );
        conn.execute_batch("DROP TRIGGER fail_policy")
            .await
            .unwrap();
        assert_eq!(
            request(&base, "GET", &route, Value::Null).await,
            (200, winner.clone())
        );
        saved.push((route, winner));
    }
    stop(server).await;
    drop(db);
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let (base, server) = serve(db).await;
    for (route, expected) in saved {
        assert_eq!(
            request(&base, "GET", &route, Value::Null).await,
            (200, expected)
        );
    }
    stop(server).await;
}
