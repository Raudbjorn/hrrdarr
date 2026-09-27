use hrrdarr::{
    db::Database,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::sync::Arc;
static SNAPSHOTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-revision-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn request(base: &str, method: &str, path: &str, v: Value) -> (u16, Value) {
    let r = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(v.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (
        status,
        if status == 204 {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        },
    )
}
async fn server(db: Arc<Database>) -> (String, Server) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = hrrdarr::revision_policy::router(db);
    (
        base,
        Server(tokio::spawn(
            async move { axum::serve(l, app).await.unwrap() },
        )),
    )
}
async fn fixture(s: &Scratch, version: i64, suffix: &str, extra: &str) -> Vec<u8> {
    let p = s.0.join(format!("source-{version}-{suffix}"));
    let db = libsql::Builder::new_local(&p).build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});CREATE TABLE Tags(Id INTEGER,Label TEXT);INSERT INTO Tags VALUES(7,'Imported'),(8,'unused');")).await.unwrap();
    let sql = match version {
        233 => {
            "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,Tags TEXT);INSERT INTO Series VALUES(1,233,'TV',2020,'/tv/imported',1,'[]','[7]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
        }
        206 => {
            "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,206,NULL,'Old',2020,'/movies/old',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
        _ => {
            "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(10,242,NULL,'New',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,10,'/movies/new',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
        }
    };
    c.execute_batch(sql).await.unwrap();
    c.execute_batch(extra).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(p).unwrap()
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
async fn pending(c: &libsql::Connection, d: &str) -> String {
    use libsql::params;
    let client = uuid::Uuid::new_v4().to_string();
    let indexer = uuid::Uuid::new_v4().to_string();
    let command = uuid::Uuid::new_v4().to_string();
    let candidate = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[client.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags)VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![client.clone(),d,d]).await.unwrap();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Indexer',1,1,1,1,'http://fixture.invalid')",[indexer.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),d,(d=="tv").then_some(0),(d=="movies").then_some(0)]).await.unwrap();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,1,?,1,100000,100)",params![command.clone(),d,indexer.clone(),client.clone()]).await.unwrap();
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,status,decision_reasons_json,created_at,updated_at,not_before)VALUES(?,?,?,?,1,?,1,?,'Release',?,'pending','[]',100,100,100000)",params![candidate.clone(),command,d,indexer,client,"f".repeat(64),vec![1u8;29]]).await.unwrap();
    candidate
}
#[tokio::test]
async fn policy_http_cas_domains_and_atomic_wake() {
    let s = Scratch::new();
    let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    let (_tv, _movie) = (pending(&c, "tv").await, pending(&c, "movies").await);
    // The new exception must not turn a deadline wake into arbitrary command mutation.
    for change in [
        "fetched=1",
        "indexer_revision=2",
        "attempts=1",
        "status='succeeded'",
        "next_attempt_at=100001",
    ] {
        let sql = if change.starts_with("next_attempt_at") {
            format!("UPDATE rss_commands SET {change} WHERE media_type='tv'")
        } else {
            format!("UPDATE rss_commands SET next_attempt_at=0,{change} WHERE media_type='tv'")
        };
        assert!(c.execute(&sql, ()).await.is_err(), "{change}");
    }
    let (base, _server) = server(db).await;
    let path = "/api/v1/tv/revision-policy";
    assert_eq!(
        request(&base, "GET", path, Value::Null).await.1["mode"],
        "prefer_and_upgrade"
    );
    for body in [
        json!({"mode":null,"revision":1}),
        json!({"mode":"unknown","revision":1}),
        json!({"mode":"do_not_prefer","revision":1,"extra":true}),
    ] {
        assert_eq!(request(&base, "PUT", path, body).await.0, 400)
    }
    assert_eq!(
        request(&base, "GET", "/api/v1/tv/revision-policy?x=1", Value::Null)
            .await
            .0,
        400
    );
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"mode":"prefer_and_upgrade","revision":1})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT locally_edited FROM revision_policies WHERE media_type='tv'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT not_before FROM rss_candidates WHERE media_type='tv'"
        )
        .await,
        100000
    ); // Same-mode intent does not change scheduling.
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"mode":"do_not_prefer","revision":1})
        )
        .await
        .0,
        409
    );
    c.execute_batch("CREATE TRIGGER fail_wake BEFORE UPDATE OF not_before ON rss_candidates BEGIN SELECT RAISE(ABORT,'injected'); END;").await.unwrap();
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"mode":"do_not_prefer","revision":2})
        )
        .await
        .0,
        503
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM revision_policies WHERE media_type='tv'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT next_attempt_at FROM rss_commands WHERE media_type='tv'"
        )
        .await,
        100000
    );
    c.execute_batch("DROP TRIGGER fail_wake;").await.unwrap();
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"mode":"do_not_prefer","revision":2})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT not_before FROM rss_candidates WHERE media_type='tv'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT next_attempt_at FROM rss_commands WHERE media_type='tv'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT not_before FROM rss_candidates WHERE media_type='movies'"
        )
        .await,
        100000
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT next_attempt_at FROM rss_commands WHERE media_type='movies'"
        )
        .await,
        100000
    );
    c.execute_batch("INSERT INTO movie_metadata(id,title,year)VALUES(77,'Protected',2020);INSERT INTO movies(id,metadata_id,path)VALUES(77,77,'/synthetic/protected');").await.unwrap();
    let protected = pending(&c, "movies").await;
    c.execute(
        "UPDATE rss_candidates SET movie_id=77 WHERE id=?",
        [protected.clone()],
    )
    .await
    .unwrap();
    let identity =
        json!({"version":1,"target":{"media_type":"movie","id":77},"hashes":["a".repeat(40)],"settings_fingerprint":"b".repeat(64),"payload_sha256":"c".repeat(64)})
            .to_string();
    c.execute(
        "UPDATE rss_candidates SET status='prepared',submission_identity_json=? WHERE id=?",
        libsql::params![identity, protected.clone()],
    )
    .await
    .unwrap();
    assert_eq!(
        request(
            &base,
            "PUT",
            "/api/v1/movies/revision-policy",
            json!({"mode":"do_not_upgrade","revision":1})
        )
        .await
        .0,
        200
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM rss_candidates WHERE media_type='movies' AND status='pending' AND not_before=0").await,1);
    assert_eq!(
        c.query(
            "SELECT not_before FROM rss_candidates WHERE id=?",
            [protected]
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<i64>(0)
        .unwrap(),
        100000
    );
}
#[tokio::test]
async fn snapshot_policy_activation_replay_local_intent_and_rollback() {
    let _lock = SNAPSHOTS.lock().await;
    let s = Scratch::new();
    for v in [233, 206, 242] {
        let app = if v == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let media = if v == 233 { "tv" } else { "movies" };
        let bytes=fixture(&s,v,"policy","CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'DownloadPropersAndRepacks','DoNotPrefer'),(2,'private_other','PRIVATE_SENTINEL');").await;
        let db = Arc::new(
            Database::open_local(s.0.join(format!("dest{v}")))
                .await
                .unwrap(),
        );
        let c = db.connect().await.unwrap();
        pending(&c, media).await;
        assert!(
            !snapshots::import(&db, app, bytes.clone(), true)
                .await
                .unwrap()
                .applied
        );
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT revision FROM revision_policies WHERE media_type='{media}'")
            )
            .await,
            1
        );
        c.execute_batch("CREATE TRIGGER fail_policy_end BEFORE UPDATE OF revision_policy_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'injected'); END;").await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(
            scalar(&c, "SELECT not_before FROM rss_candidates").await,
            100000
        );
        c.execute_batch("DROP TRIGGER fail_policy_end;")
            .await
            .unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        assert_eq!(scalar(&c, "SELECT not_before FROM rss_candidates").await, 0);
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT revision FROM revision_policies WHERE media_type='{media}'")
            )
            .await,
            2
        );
        let different=fixture(&s,v,"different","CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'downloadpropersandrepacks','1');").await;
        assert!(
            !snapshots::import(&db, app, different, false)
                .await
                .unwrap()
                .applied
        ); // Different fingerprint cannot replace earlier imported intent.
        let (base, _server) = server(db.clone()).await;
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("/api/v1/{media}/revision-policy"),
                json!({"mode":"do_not_upgrade","revision":2})
            )
            .await
            .0,
            200
        );
        assert!(
            !snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        let fresh = Arc::new(
            Database::open_local(s.0.join(format!("local{v}")))
                .await
                .unwrap(),
        );
        let (b, _guard) = server(fresh.clone()).await;
        assert_eq!(
            request(
                &b,
                "PUT",
                &format!("/api/v1/{media}/revision-policy"),
                json!({"mode":"prefer_and_upgrade","revision":1})
            )
            .await
            .0,
            200
        );
        assert!(
            !snapshots::import(&fresh, app, bytes, false)
                .await
                .unwrap()
                .applied
        ); // Explicit same-value local intent fences backfill.
    }
}
#[tokio::test]
async fn snapshot_policy_source_shapes_and_archive_backfill() {
    let _lock = SNAPSHOTS.lock().await;
    // Snapshot imports are serialized within this binary below via a shared test lock.
    let s = Scratch::new();
    for (suffix, extra, supported, error) in [
        ("absent", "", false, false),
        (
            "default",
            "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);",
            true,
            false,
        ),
        (
            "numeric",
            "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'downloadpropersandrepacks','2');",
            true,
            false,
        ),
        (
            "unknown",
            "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'downloadpropersandrepacks','SECRET_UNKNOWN');",
            false,
            false,
        ),
        (
            "null",
            "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'downloadpropersandrepacks',NULL);",
            false,
            true,
        ),
        (
            "duplicate",
            "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'DownloadPropersAndRepacks','0'),(2,'downloadpropersandrepacks','1');",
            false,
            true,
        ),
    ] {
        let bytes = fixture(&s, 233, suffix, extra).await;
        let db = Database::open_local(s.0.join(suffix)).await.unwrap();
        let result = snapshots::import(&db, Application::Sonarr, bytes.clone(), false).await;
        if error {
            assert!(result.is_err());
            continue;
        }
        let report = result.unwrap();
        assert!(report.applied);
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("SECRET_UNKNOWN"));
        let c = db.connect().await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT revision_policy_version FROM snapshot_imports").await,
            i64::from(supported)
        );
        if !supported {
            assert!(text.contains("revision policy"));
        }
        if suffix == "numeric" {
            // Model an old archive that retained Config without activating it. No receipt is rewritten.
            c.execute_batch("UPDATE snapshot_imports SET revision_policy_version=0;UPDATE revision_policies SET mode='prefer_and_upgrade',revision=1,locally_edited=0;").await.unwrap();
            assert!(
                snapshots::import(&db, Application::Sonarr, bytes, false)
                    .await
                    .unwrap()
                    .applied
            );
            assert_eq!(
                scalar(
                    &c,
                    "SELECT revision FROM revision_policies WHERE media_type='tv'"
                )
                .await,
                2
            );
        }
    }
}
