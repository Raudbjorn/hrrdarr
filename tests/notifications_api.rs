//! Notification registry, provider protocols (against an in-process fake HTTP server), the
//! trigger-fed outbox, retry/backoff and script execution. No real third-party service is used.
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use hrrdarr::{
    db::Database,
    notifications::{api, dispatcher::Dispatcher, events, transport::Transport},
    providers::CredentialKey,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const FIXED_ORIGINS: &[&str] = &[
    "https://api.telegram.org",
    "https://api.pushover.net",
    "https://api.pushbullet.com",
    "https://www.pushsafer.com",
    "https://api.simplepush.io",
    "https://api.prowlapp.com",
    "https://joinjoaomgcd.appspot.com",
    "https://api.pushcut.io",
    "https://api.sendgrid.com",
    "https://api.mailgun.net",
    "https://api.eu.mailgun.net",
    "https://notifiarr.com",
];

#[derive(Clone, Debug)]
struct Captured {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}
impl Captured {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
    fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .into_owned()
            .collect()
    }
}
#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<Captured>>>,
    /// Status returned for targets containing "/fail".
    fail_status: Arc<Mutex<u16>>,
}
async fn fake_handler(
    State(fake): State<Fake>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let target = uri
        .path_and_query()
        .map_or(uri.path().to_owned(), |p| p.as_str().to_owned());
    fake.seen.lock().unwrap().push(Captured {
        method: method.to_string(),
        target: target.clone(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    if target.contains("/fail") {
        let status = *fake.fail_status.lock().unwrap();
        return (StatusCode::from_u16(status).unwrap(), "no").into_response();
    }
    if uri.path() == "/library/sections" {
        return (
            StatusCode::OK,
            json!({"MediaContainer":{"Directory":[
                {"key":"1","type":"show","Location":[{"path":"/tv"}]},
                {"key":"2","type":"movie","Location":[{"path":"/movies"}]},
                {"key":"3","type":"show","Location":[{"path":"/elsewhere"}]}
            ]}})
            .to_string(),
        )
            .into_response();
    }
    (StatusCode::OK, "ok").into_response()
}

struct Harness {
    _dir: std::path::PathBuf,
    db: Arc<Database>,
    dispatcher: Arc<Dispatcher>,
    fake: Fake,
    fake_base: String,
    api_base: String,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        for t in &self._tasks {
            t.abort();
        }
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}
async fn harness() -> Harness {
    let dir = std::env::temp_dir().join(format!("hrrdarr-ntf-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let db = Arc::new(Database::open_local(dir.join("db")).await.unwrap());
    let fake = Fake::default();
    *fake.fail_status.lock().unwrap() = 500;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_base = format!("http://{}", l.local_addr().unwrap());
    let app = Router::new()
        .fallback(fake_handler)
        .with_state(fake.clone());
    let t1 = tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let map = FIXED_ORIGINS
        .iter()
        .map(|o| ((*o).to_owned(), fake_base.clone()))
        .collect();
    let dispatcher = Arc::new(Dispatcher::new(
        db.clone(),
        Some(Arc::new(CredentialKey::from_hex(KEY_HEX).unwrap())),
        Transport::new().unwrap().with_origin_map(map),
    ));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_base = format!("http://{}", l.local_addr().unwrap());
    let router = api::router(dispatcher.clone());
    let t2 = tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    Harness {
        _dir: dir,
        db,
        dispatcher,
        fake,
        fake_base,
        api_base,
        _tasks: vec![t1, t2],
    }
}
impl Harness {
    async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap()
            .request(method.parse().unwrap(), format!("{}{path}", self.api_base))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    fn seen(&self) -> Vec<Captured> {
        self.fake.seen.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.fake.seen.lock().unwrap().clear();
    }
    async fn sql(&self, sql: &str) {
        self.db
            .connect()
            .await
            .unwrap()
            .execute_batch(sql)
            .await
            .unwrap();
    }
    async fn count(&self, sql: &str) -> i64 {
        let c = self.db.connect().await.unwrap();
        let mut rows = c.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }
    async fn pump(&self) {
        self.dispatcher.fan_out().await.unwrap();
        self.dispatcher.deliver_due().await.unwrap();
    }
}
fn both(triggers_tv: Value, triggers_movies: Value) -> Value {
    json!({"tv":{"triggers":triggers_tv},"movies":{"triggers":triggers_movies}})
}
fn input(name: &str, implementation: &str, settings: Value, secrets: Value, scope: Value) -> Value {
    let mut v =
        json!({"name":name,"implementation":implementation,"settings":settings,"secrets":secrets});
    for (k, val) in scope.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}
fn tv_only(triggers: Value) -> Value {
    json!({"tv":{"triggers":triggers}})
}

#[tokio::test]
async fn crud_redacts_secrets_and_validates_strictly() {
    let h = harness().await;
    let (code, schema) = h
        .call("GET", "/api/v1/notifications/schema", json!(null))
        .await;
    assert_eq!(code, 200);
    let names: Vec<_> = schema["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["implementation"].as_str().unwrap().to_owned())
        .collect();
    for expected in [
        "webhook",
        "discord",
        "slack",
        "telegram",
        "ntfy",
        "gotify",
        "pushover",
        "pushbullet",
        "pushsafer",
        "simplepush",
        "prowl",
        "join",
        "pushcut",
        "sendgrid",
        "mailgun",
        "notifiarr",
        "apprise",
        "signal",
        "custom_script",
        "kodi",
        "media_browser",
        "plex",
    ] {
        assert!(names.contains(&expected.to_owned()), "{expected}");
    }
    let create = input(
        "Phone",
        "gotify",
        json!({"server_url":"http://127.0.0.1:9/"}),
        json!({"app_token":"SUPER-SECRET-TOKEN"}),
        tv_only(json!({"on_grab":true,"on_import":true,"on_upgrade":true})),
    );
    let (code, n) = h
        .call("POST", "/api/v1/notifications", create.clone())
        .await;
    assert_eq!(code, 201, "{n}");
    let id = n["id"].as_str().unwrap().to_owned();
    assert_eq!(n["secrets_configured"], json!(["app_token"]));
    assert!(n["movies"].is_null());
    assert_eq!(n["settings"]["priority"], 5, "defaults are applied");
    assert!(!n.to_string().contains("SUPER-SECRET"));
    // The secret is encrypted at rest and bound to the notification identity.
    let c = h.db.connect().await.unwrap();
    let mut rows = c
        .query("SELECT credentials,settings_json FROM notifications", ())
        .await
        .unwrap();
    let r = rows.next().await.unwrap().unwrap();
    let blob: Vec<u8> = r.get(0).unwrap();
    let settings: String = r.get(1).unwrap();
    assert!(!blob.windows(6).any(|w| w == b"SECRET") && !settings.contains("SECRET"));
    let (_, listed) = h.call("GET", "/api/v1/notifications", json!(null)).await;
    assert!(!listed.to_string().contains("SUPER-SECRET"));

    // Strict validation.
    for (case, body, expect) in [
        ("duplicate name", create.clone(), 409),
        (
            "unknown impl",
            input("X", "nope", json!({}), json!({}), tv_only(json!({}))),
            400,
        ),
        (
            "no scope",
            json!({"name":"S","implementation":"gotify","settings":{"server_url":"http://127.0.0.1:9/"},"secrets":{"app_token":"t"}}),
            400,
        ),
        (
            "pushsafer is movie only",
            input(
                "P",
                "pushsafer",
                json!({}),
                json!({"private_key":"k"}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "secret in settings",
            input(
                "Y",
                "gotify",
                json!({"server_url":"http://127.0.0.1:9/","app_token":"t"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "unknown setting",
            input(
                "Z",
                "gotify",
                json!({"server_url":"http://127.0.0.1:9/","bogus":1}),
                json!({"app_token":"t"}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "missing required secret",
            input(
                "W",
                "gotify",
                json!({"server_url":"http://127.0.0.1:9/"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "metadata address",
            input(
                "M",
                "webhook",
                json!({"url":"http://169.254.169.254/latest"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "url credentials",
            input(
                "U",
                "webhook",
                json!({"url":"http://u:p@example.com/"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "scheme",
            input(
                "F",
                "webhook",
                json!({"url":"ftp://example.com/"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "range",
            input(
                "R",
                "gotify",
                json!({"server_url":"http://127.0.0.1:9/","priority":99}),
                json!({"app_token":"t"}),
                tv_only(json!({})),
            ),
            400,
        ),
        (
            "upgrade needs import",
            input(
                "G",
                "webhook",
                json!({"url":"http://127.0.0.1:9/"}),
                json!({}),
                tv_only(json!({"on_upgrade":true})),
            ),
            400,
        ),
        (
            "movie trigger on tv",
            input(
                "H",
                "webhook",
                json!({"url":"http://127.0.0.1:9/"}),
                json!({}),
                tv_only(json!({"on_movie_added":true})),
            ),
            400,
        ),
        (
            "missing tag",
            input(
                "T",
                "webhook",
                json!({"url":"http://127.0.0.1:9/"}),
                json!({}),
                json!({"tv":{"tag_ids":[999]}}),
            ),
            400,
        ),
        (
            "control chars",
            input(
                "bad\nname",
                "webhook",
                json!({"url":"http://127.0.0.1:9/"}),
                json!({}),
                tv_only(json!({})),
            ),
            400,
        ),
    ] {
        let (code, body) = h.call("POST", "/api/v1/notifications", body).await;
        assert_eq!(code, expect, "{case}: {body}");
        assert!(!body.to_string().contains("SUPER-SECRET"));
    }

    // Update: revision fence, secret retained, then cleared.
    let mut update = create.clone();
    update["secrets"] = json!({});
    update["name"] = json!("Phone 2");
    let (code, _) = h
        .call(
            "PUT",
            &format!("/api/v1/notifications/{id}"),
            update.clone(),
        )
        .await;
    assert_eq!(code, 400, "revision required");
    update["revision"] = json!(7);
    let (code, _) = h
        .call(
            "PUT",
            &format!("/api/v1/notifications/{id}"),
            update.clone(),
        )
        .await;
    assert_eq!(code, 409);
    update["revision"] = json!(1);
    let (code, n2) = h
        .call(
            "PUT",
            &format!("/api/v1/notifications/{id}"),
            update.clone(),
        )
        .await;
    assert_eq!(code, 200, "{n2}");
    assert_eq!(n2["revision"], 2);
    assert_eq!(
        n2["secrets_configured"],
        json!(["app_token"]),
        "absent secret keeps the stored one"
    );
    update["revision"] = json!(2);
    update["secrets"] = json!({"app_token":null});
    let (code, _) = h
        .call(
            "PUT",
            &format!("/api/v1/notifications/{id}"),
            update.clone(),
        )
        .await;
    assert_eq!(code, 400, "cannot clear a required secret");
    update["secrets"] = json!({"app_token":"ROTATED"});
    let (code, n3) = h
        .call("PUT", &format!("/api/v1/notifications/{id}"), update)
        .await;
    assert_eq!(code, 200, "{n3}");
    let (code, _) = h
        .call(
            "DELETE",
            &format!("/api/v1/notifications/{id}"),
            json!(null),
        )
        .await;
    assert_eq!(code, 204);
    assert_eq!(
        h.call("GET", &format!("/api/v1/notifications/{id}"), json!(null))
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn provider_protocols_against_fake_server() {
    let h = harness().await;
    let base = h.fake_base.clone();
    // (implementation, settings, secrets, assertions)
    type Check = Box<dyn Fn(&Captured)>;
    let cases: Vec<(&str, Value, Value, Check)> = vec![
        (
            "webhook",
            json!({"url":format!("{base}/hook?x=1"),"username":"u"}),
            json!({"password":"p"}),
            Box::new(|c| {
                assert_eq!(
                    (c.method.as_str(), c.target.as_str()),
                    ("POST", "/hook?x=1")
                );
                assert_eq!(c.headers["authorization"], "Basic dTpw");
                let j = c.json();
                assert_eq!(j["eventType"], "Test");
                assert_eq!(j["mediaType"], "tv");
            }),
        ),
        (
            "discord",
            json!({"username":"bot"}),
            json!({"webhook_url":format!("{base}/discord")}),
            Box::new(|c| {
                assert_eq!(c.target, "/discord");
                let j = c.json();
                assert_eq!(j["username"], "bot");
                assert_eq!(j["embeds"][0]["title"], "Test Notification");
            }),
        ),
        (
            "slack",
            json!({"icon":":tv:"}),
            json!({"webhook_url":format!("{base}/slack")}),
            Box::new(|c| {
                let j = c.json();
                assert_eq!(j["icon_emoji"], ":tv:");
                assert_eq!(j["attachments"][0]["title"], "Test Notification");
            }),
        ),
        (
            "telegram",
            json!({"chat_id":"42","topic_id":7,"send_silently":true}),
            json!({"bot_token":"123:ABC"}),
            Box::new(|c| {
                assert_eq!(c.target, "/bot123:ABC/sendMessage");
                let j = c.json();
                assert_eq!(j["chat_id"], "42");
                assert_eq!(j["parse_mode"], "HTML");
                assert_eq!(j["message_thread_id"], 7);
                assert_eq!(j["disable_notification"], true);
                assert!(
                    j["text"]
                        .as_str()
                        .unwrap()
                        .starts_with("<b>Test Notification</b>")
                );
            }),
        ),
        (
            "ntfy",
            json!({"server_url":base,"topics":"a, b","priority":"4","tags":"tv,x"}),
            json!({"access_token":"tok"}),
            Box::new(|c| {
                assert_eq!(c.headers["authorization"], "Bearer tok");
                let j = c.json();
                assert!(["a", "b"].contains(&j["topic"].as_str().unwrap()));
                assert_eq!(j["priority"], 4);
                assert_eq!(j["tags"], json!(["tv", "x"]));
            }),
        ),
        (
            "gotify",
            json!({"server_url":format!("{base}/gotify/"),"priority":8}),
            json!({"app_token":"gt"}),
            Box::new(|c| {
                assert_eq!(c.target, "/gotify/message");
                assert_eq!(c.headers["x-gotify-key"], "gt");
                assert_eq!(c.json()["priority"], 8);
            }),
        ),
        (
            "pushover",
            json!({"priority":"2","devices":"d1,d2"}),
            json!({"user_key":"uk","api_token":"at"}),
            Box::new(|c| {
                assert_eq!(c.target, "/1/messages.json");
                let f = c.form();
                assert_eq!(
                    (
                        f["token"].as_str(),
                        f["user"].as_str(),
                        f["device"].as_str()
                    ),
                    ("at", "uk", "d1,d2")
                );
                assert_eq!(f["priority"], "2");
                assert_eq!(
                    f["retry"], "60",
                    "emergency priority carries retry and expire"
                );
                assert_eq!(f["expire"], "3600");
            }),
        ),
        (
            "pushbullet",
            json!({"device_ids":"dev1","channel_tags":"chan"}),
            json!({"api_key":"key"}),
            Box::new(|c| {
                assert_eq!(c.target, "/v2/pushes");
                assert_eq!(c.headers["authorization"], "Basic a2V5Og==");
                assert_eq!(c.json()["type"], "note");
            }),
        ),
        (
            "simplepush",
            json!({"event":"ev"}),
            json!({"key":"sk"}),
            Box::new(|c| {
                assert_eq!(c.target, "/send");
                let f = c.form();
                assert_eq!(
                    (f["key"].as_str(), f["event"].as_str(), f["title"].as_str()),
                    ("sk", "ev", "Test Notification")
                );
            }),
        ),
        (
            "prowl",
            json!({"priority":"1"}),
            json!({"api_key":"pk"}),
            Box::new(|c| {
                assert_eq!(c.target, "/publicapi/add");
                let f = c.form();
                assert_eq!(
                    (
                        f["apikey"].as_str(),
                        f["application"].as_str(),
                        f["priority"].as_str()
                    ),
                    ("pk", "hrrdarr", "1")
                );
            }),
        ),
        (
            "join",
            json!({"device_names":"phone","priority":"1"}),
            json!({"api_key":"jk"}),
            Box::new(|c| {
                assert_eq!(c.method, "GET");
                assert!(c.target.starts_with("/_ah/api/messaging/v1/sendPush?"));
                assert!(c.target.contains("apikey=jk") && c.target.contains("deviceNames=phone"));
            }),
        ),
        (
            "pushcut",
            json!({"notification_name":"My Alert"}),
            json!({"api_key":"pc"}),
            Box::new(|c| {
                assert_eq!(c.target, "/v1/notifications/My%20Alert");
                assert_eq!(c.headers["api-key"], "pc");
                assert_eq!(c.json()["title"], "Test Notification");
            }),
        ),
        (
            "sendgrid",
            json!({"from":"a@example.com","recipients":"b@example.com, c@example.com"}),
            json!({"api_key":"sg"}),
            Box::new(|c| {
                assert_eq!(c.target, "/v3/mail/send");
                assert_eq!(c.headers["authorization"], "Bearer sg");
                assert_eq!(
                    c.json()["personalizations"][0]["to"]
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );
            }),
        ),
        (
            "mailgun",
            json!({"sender_domain":"mg.example.com","from":"a@example.com","recipients":"b@example.com"}),
            json!({"api_key":"mgk"}),
            Box::new(|c| {
                assert_eq!(c.target, "/v3/mg.example.com/messages");
                assert_eq!(c.headers["authorization"], "Basic YXBpOm1naw==");
                assert_eq!(c.form()["to"], "b@example.com");
            }),
        ),
        (
            "notifiarr",
            json!({}),
            json!({"api_key":"nk"}),
            Box::new(|c| {
                assert_eq!(c.target, "/api/v1/notification/sonarr");
                assert_eq!(c.headers["x-api-key"], "nk");
            }),
        ),
        (
            "apprise",
            json!({"server_url":base,"configuration_key":"cfg key"}),
            json!({}),
            Box::new(|c| {
                assert_eq!(c.target, "/notify/cfg%20key");
                assert_eq!(c.json()["type"], "success");
            }),
        ),
        (
            "signal",
            json!({"server_url":base,"sender_number":"+100","receivers":"+200,+300"}),
            json!({}),
            Box::new(|c| {
                assert_eq!(c.target, "/v2/send");
                let j = c.json();
                assert_eq!(j["number"], "+100");
                assert_eq!(j["recipients"], json!(["+200", "+300"]));
            }),
        ),
        (
            "kodi",
            json!({"url":base,"username":"k"}),
            json!({"password":"kp"}),
            Box::new(|c| {
                assert_eq!(c.target, "/jsonrpc");
                assert_eq!(c.json()["method"], "GUI.ShowNotification");
            }),
        ),
        (
            "media_browser",
            json!({"url":base,"notify":true,"update_library":true}),
            json!({"api_key":"mb"}),
            Box::new(|c| {
                assert_eq!(c.target, "/Notifications/Admin");
                assert_eq!(c.headers["x-emby-token"], "mb");
            }),
        ),
    ];
    for (implementation, settings, secrets, check) in cases {
        h.clear();
        let draft = input("T", implementation, settings, secrets, tv_only(json!({})));
        let (code, result) = h
            .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
            .await;
        assert_eq!(code, 200, "{implementation}: {result}");
        assert_eq!(
            result["results"][0]["status"], "delivered",
            "{implementation}: {result}"
        );
        let seen = h.seen();
        let request = seen
            .iter()
            .find(|c| !c.target.is_empty())
            .unwrap_or_else(|| panic!("{implementation}: nothing sent"));
        check(request);
    }
    // Pushsafer is a movie-only implementation.
    h.clear();
    let draft = input(
        "T",
        "pushsafer",
        json!({"priority":"1","sound":3}),
        json!({"private_key":"psk"}),
        json!({"movies":{}}),
    );
    let (_, result) = h
        .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
        .await;
    assert_eq!(result["results"][0]["status"], "delivered", "{result}");
    let f = h.seen()[0].form();
    assert_eq!(
        (f["k"].as_str(), f["p"].as_str(), f["s"].as_str()),
        ("psk", "1", "3")
    );
    // A movie draft renders the Radarr application contract.
    h.clear();
    let draft = input(
        "T",
        "webhook",
        json!({"url":format!("{base}/m")}),
        json!({}),
        json!({"movies":{}}),
    );
    h.call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
        .await;
    assert_eq!(h.seen()[0].json()["mediaType"], "movies");
}

#[tokio::test]
async fn test_endpoint_reports_failure_classes_without_leaking() {
    let h = harness().await;
    let base = h.fake_base.clone();
    for (status, class) in [
        (401u16, "authentication"),
        (400, "rejected"),
        (503, "server_error"),
        (429, "rate_limited"),
    ] {
        *h.fake.fail_status.lock().unwrap() = status;
        let draft = input(
            "T",
            "webhook",
            json!({"url":format!("{base}/fail?secret=hunter2")}),
            json!({}),
            tv_only(json!({})),
        );
        let (code, result) = h
            .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
            .await;
        assert_eq!(code, 200);
        assert_eq!(result["results"][0]["status"], "failed");
        assert_eq!(result["results"][0]["error"], class);
        assert!(!result.to_string().contains("hunter2"));
    }
    // Unreachable target: transport class, still no URL in the result.
    let draft = input(
        "T",
        "webhook",
        json!({"url":"http://127.0.0.1:1/x?token=zzz"}),
        json!({}),
        tv_only(json!({})),
    );
    let (_, result) = h
        .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
        .await;
    assert_eq!(result["results"][0]["error"], "transport");
    assert!(!result.to_string().contains("zzz"));
}

#[tokio::test]
async fn outbox_delivers_domain_events_with_tag_scope_and_idle_install_records_nothing() {
    let h = harness().await;
    let base = h.fake_base.clone();
    // No enabled notification: triggers record nothing.
    h.sql("INSERT INTO series(id,title,path)VALUES(1,'Idle','/tv/idle');DELETE FROM series WHERE id=1;").await;
    assert_eq!(h.count("SELECT count(*) FROM notification_events").await, 0);

    h.sql("INSERT INTO tags(id,media_type,label)VALUES(1,'tv','kids'),(2,'tv','adult'),(3,'movies','kids');").await;
    let all = input(
        "All TV",
        "webhook",
        json!({"url":format!("{base}/all")}),
        json!({}),
        json!({"tv":{"triggers":{"on_series_add":true,"on_series_delete":true,"on_episode_file_delete":true}},"movies":{"triggers":{"on_movie_added":true}}}),
    );
    let (code, a) = h.call("POST", "/api/v1/notifications", all).await;
    assert_eq!(code, 201, "{a}");
    let tagged = input(
        "Kids only",
        "webhook",
        json!({"url":format!("{base}/kids")}),
        json!({}),
        json!({"tv":{"triggers":{"on_series_add":true,"on_series_delete":true},"tag_ids":[1]}}),
    );
    assert_eq!(h.call("POST", "/api/v1/notifications", tagged).await.0, 201);

    h.sql("INSERT INTO series(id,title,year,path)VALUES(10,'Cartoon',2020,'/tv/cartoon'),(11,'Drama',2019,'/tv/drama');INSERT INTO series_tags(series_id,tag_id)VALUES(10,1),(11,2);\
           INSERT INTO movie_metadata(id,tmdb_id,title,year)VALUES(1,500,'Film',2001);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies/film');").await;
    h.pump().await;
    let seen = h.seen();
    let all_hits: Vec<_> = seen
        .iter()
        .filter(|c| c.target == "/all")
        .map(|c| c.json())
        .collect();
    let kids_hits: Vec<_> = seen
        .iter()
        .filter(|c| c.target == "/kids")
        .map(|c| c.json())
        .collect();
    assert_eq!(all_hits.len(), 3, "{all_hits:?}");
    assert_eq!(
        kids_hits.len(),
        1,
        "tag scope: only the series carrying the kids tag"
    );
    assert_eq!(kids_hits[0]["series"]["title"], "Cartoon");
    assert_eq!(kids_hits[0]["series"]["tags"], json!(["kids"]));
    let tv_add = all_hits
        .iter()
        .find(|j| j["eventType"] == "SeriesAdd" && j["series"]["id"] == 10)
        .unwrap();
    assert_eq!(tv_add["mediaType"], "tv");
    assert!(tv_add.get("movie").is_none());
    let movie_add = all_hits
        .iter()
        .find(|j| j["eventType"] == "MovieAdded")
        .unwrap();
    assert_eq!(movie_add["mediaType"], "movies");
    assert_eq!(movie_add["movie"]["tmdbId"], 500);
    // The same numeric id in the other domain stays a distinct subject.
    assert_eq!(movie_add["movie"]["id"], 1);

    // Deletion snapshots title and tags, and the file-delete reason comes from the journals.
    h.clear();
    h.sql("INSERT INTO seasons(series_id,number)VALUES(11,1);INSERT INTO episode_files(id,series_id,path)VALUES(1,11,'/tv/drama/a.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(1,11,1,1,'Pilot',1);\
           UPDATE episodes SET episode_file_id=NULL WHERE id=1;DELETE FROM episode_files WHERE id=1;").await;
    h.pump().await;
    let seen = h.seen();
    let file_delete = seen
        .iter()
        .find(|c| c.json()["eventType"] == "EpisodeFileDelete")
        .expect("file delete");
    assert_eq!(file_delete.json()["deleteReason"], "NoLinkedEpisodes");
    assert_eq!(file_delete.target, "/all");
    assert!(
        seen.iter()
            .all(|c| c.target != "/kids" || c.json()["eventType"] != "EpisodeFileDelete")
    );
    h.clear();
    h.sql("DELETE FROM series WHERE id=10;").await;
    h.pump().await;
    let seen = h.seen();
    let kids: Vec<_> = seen.iter().filter(|c| c.target == "/kids").collect();
    assert_eq!(
        kids.len(),
        1,
        "tags were snapshotted before series_tags was cleared"
    );
    let del = kids[0].json();
    assert_eq!(del["eventType"], "SeriesDelete");
    assert_eq!(del["series"]["title"], "Cartoon");
    assert_eq!(del["series"]["path"], "/tv/cartoon");
    assert_eq!(
        h.count("SELECT count(*) FROM notification_deliveries WHERE state='delivered'")
            .await,
        7
    );
}

#[tokio::test]
async fn health_import_and_manual_events_flow_end_to_end() {
    let h = harness().await;
    let base = h.fake_base.clone();
    let hook = |name: &str, path: &str, tv: Value, movies: Value| {
        input(
            name,
            "webhook",
            json!({"url":format!("{base}/{path}")}),
            json!({}),
            json!({"tv":{"triggers":tv},"movies":{"triggers":movies}}),
        )
    };
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/notifications",
            hook(
                "Health",
                "health",
                json!({"on_health_issue":true,"on_health_restored":true,"include_health_warnings":true}),
                json!({})
            )
        )
        .await
        .0,
        201
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/notifications",
            hook(
                "Errors",
                "errors",
                json!({"on_health_issue":true,"include_health_warnings":false}),
                json!({})
            )
        )
        .await
        .0,
        201
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/notifications",
            hook("Imports", "imports", json!({}), json!({"on_import":true}))
        )
        .await
        .0,
        201
    );
    assert_eq!(
        h.call(
            "POST",
            "/api/v1/notifications",
            hook(
                "Upgrades",
                "upgrades",
                json!({}),
                json!({"on_import":true,"on_upgrade":true})
            )
        )
        .await
        .0,
        201
    );

    let health = |id: u8, kind: &str, grace: u8, severity: u8| {
        format!(
            "INSERT INTO health_transitions(event_id,epoch,command_id,command_attempt,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type) VALUES('00000000-0000-4000-8000-0000000000{id:02}','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-0000000001{id:02}',1,'tv','indexer_status','{kind}',{grace},100,{severity},'r','Indexers unavailable','https://wiki.invalid/h','IndexerStatusCheck');"
        )
    };
    h.sql(&format!(
        "{}{}{}{}",
        health(1, "issue", 1, 3),
        health(2, "issue", 0, 2),
        health(3, "issue", 0, 3),
        health(4, "restored", 0, 1)
    ))
    .await;
    h.pump().await;
    let seen = h.seen();
    let hooks = |p: &str| {
        seen.iter()
            .filter(|c| c.target == format!("/{p}"))
            .map(|c| c.json())
            .collect::<Vec<_>>()
    };
    let health_events = hooks("health");
    assert_eq!(
        health_events.len(),
        3,
        "grace-window transition is not an alert: {health_events:?}"
    );
    assert!(
        health_events
            .iter()
            .any(|j| j["eventType"] == "HealthIssue" && j["level"] == "Warning")
    );
    assert!(
        health_events
            .iter()
            .any(|j| j["eventType"] == "HealthRestored")
    );
    assert_eq!(
        hooks("errors").len(),
        1,
        "warnings are excluded unless include_health_warnings"
    );
    assert_eq!(hooks("errors")[0]["level"], "Error");
    assert_eq!(hooks("errors")[0]["message"], "Indexers unavailable");

    // A movie import receipt produces a Download event; a plain import does not reach OnUpgrade-only targets
    // beyond the ones that opted in.
    h.clear();
    h.sql("INSERT INTO movie_metadata(id,tmdb_id,title,year)VALUES(1,201,'Film',2000);INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies/film');\
           INSERT INTO operations VALUES('op1','movie',NULL,1,'/downloads/film.mkv','copy','/movies/film/film.mkv','preview','');\
           INSERT INTO import_journal(operation_id,plan_json,phase,stage_json)VALUES('op1','{}','staging','{}');\
           INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/film/film.mkv');\
           INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256)VALUES('op1','movie',1,1,'/downloads/film.mkv','/movies/film/film.mkv',1234,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');").await;
    h.pump().await;
    let imports = h
        .seen()
        .iter()
        .filter(|c| c.target == "/imports")
        .map(|c| c.json())
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0]["eventType"], "Download");
    assert_eq!(imports[0]["isUpgrade"], false);
    assert_eq!(imports[0]["movieFile"]["relativePath"], "film.mkv");
    assert_eq!(imports[0]["movie"]["title"], "Film");
    assert!(imports[0].get("series").is_none());
    assert_eq!(
        h.seen().iter().filter(|c| c.target == "/upgrades").count(),
        1,
        "plain imports reach on_import+on_upgrade targets"
    );

    // An upgrade import (emitted with the is_upgrade fact) needs on_upgrade; a delete-for-upgrade
    // is withheld without its own flag.
    h.clear();
    events::emit(&h.db.connect().await.unwrap(), events::EventKind::Import, events::Scope::Domain(hrrdarr::api::MediaDomain::Movies), Some(1),
        &json!({"operation_id":"op1","movie_file_id":1,"path":"/movies/film/film.mkv","source_path":"/d/f.mkv","size":1,"is_upgrade":1})).await.unwrap();
    h.pump().await;
    assert_eq!(
        h.seen().iter().filter(|c| c.target == "/imports").count(),
        0,
        "OnUpgrade not enabled there"
    );
    let upgrades = h
        .seen()
        .iter()
        .filter(|c| c.target == "/upgrades")
        .map(|c| c.json())
        .collect::<Vec<_>>();
    assert_eq!(upgrades.len(), 1);
    assert_eq!(upgrades[0]["isUpgrade"], true);
}

#[tokio::test]
async fn retries_back_off_then_park_and_permanent_failures_do_not_retry() {
    let h = harness().await;
    let base = h.fake_base.clone();
    let make = |name: &str, path: &str| {
        input(
            name,
            "webhook",
            json!({"url":format!("{base}/{path}")}),
            json!({}),
            tv_only(json!({"on_series_add":true})),
        )
    };
    assert_eq!(
        h.call("POST", "/api/v1/notifications", make("Flaky", "fail/a"))
            .await
            .0,
        201
    );
    h.sql("INSERT INTO series(id,title,path)VALUES(1,'S','/tv/s');")
        .await;
    *h.fake.fail_status.lock().unwrap() = 503;
    h.pump().await;
    let row = |col: &str| format!("SELECT {col} FROM notification_deliveries");
    assert_eq!(h.count(&row("attempts")).await, 1);
    assert_eq!(
        h.count("SELECT state='pending' FROM notification_deliveries")
            .await,
        1
    );
    assert!(
        h.count("SELECT next_attempt_at-strftime('%s','now') FROM notification_deliveries")
            .await
            >= 25,
        "backed off"
    );
    // Not due yet: no new request.
    let before = h.seen().len();
    h.pump().await;
    assert_eq!(h.seen().len(), before);
    // Due again; once the target recovers (the fake now answers 200) it is delivered.
    h.sql("UPDATE notification_deliveries SET next_attempt_at=0;")
        .await;
    *h.fake.fail_status.lock().unwrap() = 200;
    h.pump().await;
    assert_eq!(h.count("SELECT state='delivered' AND attempts=2 AND last_error IS NULL FROM notification_deliveries").await, 1);

    // Permanent failure (401) parks immediately; a flaky target gives up after the attempt cap.
    h.sql("DELETE FROM notification_deliveries;").await;
    *h.fake.fail_status.lock().unwrap() = 401;
    h.sql("INSERT INTO series(id,title,path)VALUES(2,'S2','/tv/s2');")
        .await;
    h.pump().await;
    assert_eq!(h.count("SELECT state='failed' AND attempts=1 AND last_error='authentication' FROM notification_deliveries").await, 1);
    h.sql("DELETE FROM notification_deliveries;").await;
    *h.fake.fail_status.lock().unwrap() = 500;
    h.sql("INSERT INTO series(id,title,path)VALUES(3,'S3','/tv/s3');")
        .await;
    for _ in 0..8 {
        h.sql("UPDATE notification_deliveries SET next_attempt_at=0 WHERE state='pending';")
            .await;
        h.pump().await;
    }
    assert_eq!(h.count("SELECT state='failed' AND attempts=5 AND last_error='server_error' FROM notification_deliveries").await, 1);
    // A leased (in-flight) delivery is not claimed twice.
    h.sql("INSERT INTO series(id,title,path)VALUES(4,'S4','/tv/s4');")
        .await;
    h.dispatcher.fan_out().await.unwrap();
    h.sql("UPDATE notification_deliveries SET lease_until=strftime('%s','now')+600 WHERE state='pending';").await;
    let before = h.seen().len();
    h.dispatcher.deliver_due().await.unwrap();
    assert_eq!(h.seen().len(), before);
    // Removing the notification discards its pending work.
    h.sql("DELETE FROM notifications;").await;
    assert_eq!(
        h.count("SELECT count(*) FROM notification_deliveries")
            .await,
        0
    );
}

#[tokio::test]
async fn custom_script_receives_domain_environment_without_a_shell() {
    use std::os::unix::fs::PermissionsExt;
    let h = harness().await;
    let out = h._dir.join("env.txt");
    let script = h._dir.join("hook.sh");
    std::fs::write(&script, format!("#!/bin/sh\n{{ env | sort; echo \"ARGS:$1|$2\"; echo \"PWD:$(pwd)\"; }} > {}\necho hello\n", out.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let draft = input(
        "Script",
        "custom_script",
        json!({"path":script.to_str().unwrap(),"arguments":"\"two words\" $HOME"}),
        json!({}),
        both(json!({}), json!({})),
    );
    let (code, result) = h
        .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
        .await;
    assert_eq!(code, 200, "{result}");
    assert_eq!(result["results"].as_array().unwrap().len(), 2);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("radarr_eventtype=Test") || text.contains("sonarr_eventtype=Test"));
    assert!(
        text.contains("ARGS:two words|$HOME"),
        "no shell expansion: {text}"
    );
    assert!(text.contains("PWD:/"));
    assert!(
        !text.contains("HRRDARR_PROVIDER_KEY") && !text.contains("HOME="),
        "environment is cleared"
    );
    // Per-domain contract: TV runs get sonarr_*, movie runs get radarr_* (the later run wrote the file).
    assert!(text.contains("radarr_instancename=hrrdarr"));
    // Not executable / relative / failing scripts are failures, never executed through a shell.
    let plain = h._dir.join("plain.sh");
    std::fs::write(&plain, "#!/bin/sh\nexit 3\n").unwrap();
    for (path, class) in [
        (plain.to_str().unwrap().to_owned(), "script_not_executable"),
        ("relative.sh".to_owned(), "invalid_notification"),
    ] {
        let draft = input(
            "S2",
            "custom_script",
            json!({"path":path}),
            json!({}),
            tv_only(json!({})),
        );
        let (code, result) = h
            .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
            .await;
        if class == "invalid_notification" {
            assert_eq!(code, 400);
        } else {
            assert_eq!(result["results"][0]["error"], class);
        }
    }
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
    let draft = input(
        "S3",
        "custom_script",
        json!({"path":plain.to_str().unwrap()}),
        json!({}),
        tv_only(json!({})),
    );
    let (_, result) = h
        .call("POST", "/api/v1/notifications/test", json!({"draft":draft}))
        .await;
    assert_eq!(result["results"][0]["error"], "script_failed");
}

#[tokio::test]
async fn plex_refreshes_only_sections_containing_the_changed_path() {
    let h = harness().await;
    let base = h.fake_base.clone();
    let cfg = input(
        "Plex",
        "plex",
        json!({"url":base}),
        json!({"auth_token":"plextok"}),
        both(json!({"on_import":true}), json!({"on_import":true})),
    );
    let (code, n) = h.call("POST", "/api/v1/notifications", cfg).await;
    assert_eq!(code, 201, "{n}");
    h.sql("INSERT INTO series(id,title,path)VALUES(1,'S','/tv/s');INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/s/e1.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(1,1,1,1,'E',1);\
           INSERT INTO operations VALUES('op','episode',1,NULL,'/d/e1.mkv','copy','/tv/s/e1.mkv','preview','');INSERT INTO import_journal(operation_id,plan_json,phase,stage_json)VALUES('op','{}','staging','{}');\
           INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)VALUES('op','episode',1,1,'/d/e1.mkv','/tv/s/e1.mkv',5,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb');").await;
    h.pump().await;
    let seen = h.seen();
    let refreshes: Vec<_> = seen
        .iter()
        .filter(|c| c.target.contains("/refresh"))
        .collect();
    assert_eq!(refreshes.len(), 1, "{seen:?}");
    assert!(
        refreshes[0]
            .target
            .starts_with("/library/sections/1/refresh?path=%2Ftv%2Fs")
    );
    assert_eq!(refreshes[0].headers["x-plex-token"], "plextok");
}

#[tokio::test]
async fn system_notification_scope_survives_restart_filter_edits_and_retry() {
    use std::os::unix::fs::PermissionsExt;
    let h = harness().await;
    let out = h._dir.join("system-env.txt");
    let script = h._dir.join("system-hook.sh");
    std::fs::write(&script, format!("#!/bin/sh\nenv > {}\n", out.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (name, implementation, settings, secrets, tv) in [
        (
            "Webhook",
            "webhook",
            json!({"url":format!("{}/fail", h.fake_base)}),
            json!({}),
            json!({}),
        ),
        (
            "Notifiarr",
            "notifiarr",
            json!({}),
            json!({"api_key":"nk"}),
            json!({"on_health_issue":true,"include_health_warnings":false}),
        ),
        (
            "Script",
            "custom_script",
            json!({"path":script.to_str().unwrap()}),
            json!({}),
            json!({}),
        ),
    ] {
        let (code, body) = h
            .call(
                "POST",
                "/api/v1/notifications",
                input(
                    name,
                    implementation,
                    settings,
                    secrets,
                    both(
                        tv,
                        json!({"on_health_issue":true,"include_health_warnings":true}),
                    ),
                ),
            )
            .await;
        assert_eq!(code, 201, "{body}");
    }
    h.sql("INSERT INTO notification_events(kind,scope,payload_json,created_at) VALUES('health_issue','system','{\"severity\":2,\"message\":\"Warning\"}',100);").await;
    assert_eq!(h.dispatcher.fan_out().await.unwrap(), 3);
    assert_eq!(
        h.count("SELECT count(*) FROM notification_deliveries WHERE media_type='movies'")
            .await,
        3
    );
    // Editing filters after fan-out must not change the durable choice on delivery or retry.
    h.sql("UPDATE notification_scopes SET on_health_issue=(media_type='tv'),include_health_warnings=(media_type='tv');").await;
    let restarted = Dispatcher::new(
        h.db.clone(),
        Some(Arc::new(CredentialKey::from_hex(KEY_HEX).unwrap())),
        Transport::new().unwrap().with_origin_map(
            FIXED_ORIGINS
                .iter()
                .map(|o| ((*o).to_owned(), h.fake_base.clone()))
                .collect(),
        ),
    );
    assert_eq!(restarted.deliver_due().await.unwrap(), 3);
    let seen = h.seen();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|c| c.json()["mediaType"] == "movies"));
    assert!(
        seen.iter()
            .any(|c| c.target == "/api/v1/notification/radarr")
    );
    let env = std::fs::read_to_string(out).unwrap();
    assert!(env.contains("radarr_eventtype=HealthIssue"), "{env}");
    assert!(!env.contains("sonarr_"), "{env}");
    *h.fake.fail_status.lock().unwrap() = 200;
    h.sql("UPDATE notification_deliveries SET next_attempt_at=0 WHERE state='pending';")
        .await;
    h.clear();
    assert_eq!(restarted.deliver_due().await.unwrap(), 1);
    assert_eq!(h.seen()[0].json()["mediaType"], "movies");
    assert_eq!(
        h.count("SELECT count(*) FROM notification_deliveries WHERE state='delivered'")
            .await,
        3
    );
}

#[tokio::test]
async fn legacy_system_delivery_selects_matching_scope_once() {
    let h = harness().await;
    let (code, _) = h
        .call(
            "POST",
            "/api/v1/notifications",
            input(
                "Legacy",
                "webhook",
                json!({"url":format!("{}/fail", h.fake_base)}),
                json!({}),
                both(
                    json!({}),
                    json!({"on_health_issue":true,"include_health_warnings":true}),
                ),
            ),
        )
        .await;
    assert_eq!(code, 201);
    h.sql("INSERT INTO notification_events(kind,scope,payload_json,created_at,fanned_out) VALUES('health_issue','system','{\"severity\":2}',100,1);
        INSERT INTO notification_deliveries(event_sequence,notification_id,next_attempt_at,created_at) SELECT 1,id,0,100 FROM notifications;").await;
    h.dispatcher.deliver_due().await.unwrap();
    assert_eq!(h.seen()[0].json()["mediaType"], "movies");
    assert_eq!(
        h.count("SELECT count(*) FROM notification_deliveries WHERE media_type='movies'")
            .await,
        1
    );
    h.sql("UPDATE notification_scopes SET on_health_issue=(media_type='tv'),include_health_warnings=(media_type='tv');
        UPDATE notification_deliveries SET next_attempt_at=0;").await;
    *h.fake.fail_status.lock().unwrap() = 200;
    h.clear();
    h.dispatcher.deliver_due().await.unwrap();
    assert_eq!(h.seen()[0].json()["mediaType"], "movies");
}
