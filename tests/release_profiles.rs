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
            std::env::temp_dir().join(format!("hrrdarr-release-profile-{}", uuid::Uuid::new_v4()));
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
impl Server {
    async fn close(mut self) {
        self.0.abort();
        let result = (&mut self.0).await;
        assert!(result.is_ok() || result.unwrap_err().is_cancelled());
    }
}
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
async fn server(db: Arc<Database>, key_byte: &str) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let key = Arc::new(hrrdarr::providers::CredentialKey::from_hex(&key_byte.repeat(32)).unwrap());
    let (providers, _) = hrrdarr::providers::router_with_refresh(db.clone(), Some(key));
    let app = providers
        .merge(hrrdarr::release_profiles::router(db.clone()))
        .merge(hrrdarr::tags::router(db.clone()))
        .merge(hrrdarr::library::router(db));
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
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
fn profile(d: &str) -> Value {
    let mut v = json!({"name":null,"enabled":true,"required":["WEB"],"ignored":[],"tag_ids":[],"indexers":[]});
    if d == "tv" {
        v["excluded_tag_ids"] = json!([]);
        v["air_date_restriction"] = json!(false);
        v["air_date_grace_period_days"] = json!(0);
        v["allow_season_pack_without_all_episodes_aired"] = json!(false);
    }
    v
}
fn provider() -> Value {
    json!({"name":"Indexer","enabled":false,"priority":1,"settings":{"implementation":"torznab","endpoint":"http://fixture.invalid/api","tv":{"categories":[5000],"anime_categories":[]},"movies":{"categories":[2000]}},"credentials":{"kind":"api_key","api_key":"SYNTHETIC_RELEASE_PROFILE_KEY"}})
}
#[tokio::test]
async fn release_profile_http_domains_cas_references_and_authenticated_provider_guards() {
    for d in ["tv", "movies"] {
        let s = Scratch::new();
        let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        let (base, server) = server(db.clone(), "11").await;
        let (status, indexer) = request(&base, "POST", "/api/v1/providers", provider()).await;
        assert_eq!(status, 201, "{indexer}");
        let indexer = indexer["id"].as_str().unwrap().to_owned();
        let (status, tag) = request(
            &base,
            "POST",
            &format!("/api/v1/{d}/tags"),
            json!({"label":"scope"}),
        )
        .await;
        assert_eq!(status, 201);
        let tag = tag["id"].as_i64().unwrap();
        let p = format!("/api/v1/{d}/release-profiles");
        let (status, draft) = request(&base, "GET", &format!("{p}/schema"), Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(draft["enabled"], true);
        let _pending = pending(&c, d).await;
        let mut value = profile(d);
        value["enabled"] = json!(false);
        value["tag_ids"] = json!([tag]);
        value["indexers"] = json!([{"kind":"provider","id":indexer}]);
        // A live provider FK alone is insufficient: kind and domain must validate
        // before any profile or catalog-revision write.
        let client_id = c
            .query(
                "SELECT id FROM providers WHERE implementation='qbittorrent'",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        let mut invalid = value.clone();
        invalid["indexers"] = json!([{"kind":"provider","id":client_id}]);
        let (status, error) =
            request(&base, "POST", &p, json!({"revision":1,"profile":invalid})).await;
        // The contract classifies unavailable/wrong-scope indexers as reference conflicts.
        assert_eq!(status, 409);
        assert_eq!(
            error["error"]["code"],
            "release_profile_indexer_unavailable"
        );
        let mut other_provider = provider();
        // Provider scopes are required nullable fields, not optional omitted keys.
        other_provider["settings"][d] = Value::Null;
        let (status, other) = request(&base, "POST", "/api/v1/providers", other_provider).await;
        assert_eq!(status, 201, "{other}");
        invalid["indexers"] = json!([{"kind":"provider","id":other["id"]}]);
        let (status, error) =
            request(&base, "POST", &p, json!({"revision":1,"profile":invalid})).await;
        // The contract classifies unavailable/wrong-scope indexers as reference conflicts.
        assert_eq!(status, 409);
        assert_eq!(
            error["error"]["code"],
            "release_profile_indexer_unavailable"
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
        let (status, all) = request(&base, "POST", &p, json!({"revision":1,"profile":value})).await;
        assert_eq!(status, 201, "{all}");
        assert_eq!(all["revision"], 2);
        let id = all["profiles"][0]["id"].as_i64().unwrap();
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT not_before FROM rss_candidates WHERE media_type='{d}'")
            )
            .await,
            0
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{p}/{id}"),
                json!({"revision":1,"profile":value})
            )
            .await
            .0,
            409
        );
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("/api/v1/{d}/tags/{tag}"),
                Value::Null
            )
            .await
            .0,
            409
        );
        let (status, detail) = request(
            &base,
            "GET",
            &format!("/api/v1/{d}/tags/detail/{tag}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(detail["release_profile_ids"], json!([id]));
        let other = if d == "tv" { "movies" } else { "tv" };
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/{other}/release-profiles/{id}"),
                Value::Null
            )
            .await
            .0,
            404
        );
        // Disabled references still protect the provider; credential authentication must run first.
        let (wrong, wrong_server) = server_with_wrong_key(db.clone()).await;
        let status = request(
            &wrong,
            "DELETE",
            &format!("/api/v1/providers/{indexer}?revision=1"),
            Value::Null,
        )
        .await
        .0;
        assert_eq!(status, 503); // Credential authentication precedes reference-conflict disclosure.
        wrong_server.close().await;
        let mut change = provider();
        change.as_object_mut().unwrap().remove("credentials");
        change["revision"] = json!(1);
        change["name"] = json!("Same scopes");
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("/api/v1/providers/{indexer}"),
                change.clone()
            )
            .await
            .0,
            200
        );
        change["revision"] = json!(2);
        change["settings"][d] = Value::Null;
        let (status, error) = request(
            &base,
            "PUT",
            &format!("/api/v1/providers/{indexer}"),
            change,
        )
        .await;
        assert_eq!(status, 409, "{error}");
        assert_eq!(error["error"]["code"], "provider_in_use");
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("/api/v1/providers/{indexer}?revision=2"),
                Value::Null
            )
            .await
            .0,
            409
        );
        let (_, unchanged) = request(
            &base,
            "GET",
            &format!("/api/v1/providers/{indexer}"),
            Value::Null,
        )
        .await;
        assert_eq!(unchanged["revision"], 2);
        assert!(unchanged["settings"][d].is_object());
        let mut bad = value.clone();
        bad["indexers"] = json!([{"kind":"unresolved_source","application":if d=="tv"{"sonarr"}else{"radarr"},"fingerprint":"a".repeat(64),"source_id":7}]);
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{p}/{id}"),
                json!({"revision":2,"profile":bad})
            )
            .await
            .0,
            400
        );
        if d == "tv" {
            let mut bad = value.clone();
            bad["excluded_tag_ids"] = json!([tag]);
            assert_eq!(
                request(
                    &base,
                    "PUT",
                    &format!("{p}/{id}"),
                    json!({"revision":2,"profile":bad})
                )
                .await
                .0,
                400
            );
        } else {
            let mut bad = value.clone();
            bad["air_date_restriction"] = json!(false);
            assert_eq!(
                request(
                    &base,
                    "PUT",
                    &format!("{p}/{id}"),
                    json!({"revision":2,"profile":bad})
                )
                .await
                .0,
                400
            );
        }
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("{p}/{id}?revision=2"),
                Value::Null
            )
            .await
            .0,
            200
        );
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("/api/v1/providers/{indexer}?revision=2"),
                Value::Null
            )
            .await
            .0,
            204
        );
        server.close().await;
    }
}
async fn server_with_wrong_key(db: Arc<Database>) -> (String, Server) {
    server(db, "22").await
}

#[tokio::test]
async fn snapshot_empty_catalog_preserves_native_intent_and_activated_replay() {
    let _serial = SNAPSHOTS.lock().await;
    for (version, application, domain, table) in [
        (233, Application::Sonarr, "tv", "ReleaseProfiles"),
        (206, Application::Radarr, "movies", "Restrictions"),
        (242, Application::Radarr, "movies", "ReleaseProfiles"),
    ] {
        let scratch = Scratch::new();
        let bytes = fixture(
            &scratch,
            version,
            "empty",
            &format!("CREATE TABLE {table}(Id INTEGER);"),
        )
        .await;
        let db = Database::open_local(scratch.0.join("destination"))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        // Equal empty catalogs must not override explicit native empty intent.
        c.execute(
            "UPDATE release_profile_domains SET locally_edited=1 WHERE media_type=?",
            [domain],
        )
        .await
        .unwrap();
        let blocked = snapshots::import(&db, application, bytes.clone(), false)
            .await
            .unwrap();
        assert!(blocked.conflicts > 0);
        // Any reconciliation conflict rolls back the whole transaction, so there
        // is no provenance row at all (SUM over this empty set would be NULL).
        assert!(!blocked.applied);
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM snapshot_mappings WHERE destination_table='release_profiles'"
            )
            .await,
            0
        );
        // Already activated exact replay remains safe after a same-value edit.
        let pristine = Database::open_local(scratch.0.join("pristine"))
            .await
            .unwrap();
        snapshots::import(&pristine, application, bytes.clone(), false)
            .await
            .unwrap();
        let p = pristine.connect().await.unwrap();
        assert_eq!(
            scalar(
                &p,
                "SELECT sum(release_profile_version) FROM snapshot_imports"
            )
            .await,
            1
        );
        p.execute(
            "UPDATE release_profile_domains SET locally_edited=1 WHERE media_type=?",
            [domain],
        )
        .await
        .unwrap();
        let replay = snapshots::import(&pristine, application, bytes, false)
            .await
            .unwrap();
        assert_eq!(replay.conflicts, 0);
        assert_eq!(
            scalar(
                &p,
                "SELECT sum(release_profile_version) FROM snapshot_imports"
            )
            .await,
            1
        );
    }
}

fn source_profiles(version: i64, enabled: bool, indexer: i64) -> String {
    match version {
        233 => format!("CREATE TABLE ReleaseProfiles(Id INTEGER,Name TEXT,Enabled INTEGER,Required TEXT,Ignored TEXT,IndexerIds TEXT,Tags TEXT,ExcludedTags TEXT,AirDateRestriction INTEGER,AirDateGracePeriod INTEGER,AllowSeasonPackWithoutAllEpisodesAired INTEGER);INSERT INTO ReleaseProfiles VALUES(7,'Imported',{},'[\"WEB\",\"HD\"]','[\"CAM\"]','{}','[7]','[8]',1,3,1);",i32::from(enabled),if indexer==0{"[]".to_owned()}else{format!("[{indexer}]")}),
        206 => "CREATE TABLE Restrictions(Id INTEGER,Required TEXT,Ignored TEXT,Preferred TEXT,Tags TEXT);INSERT INTO Restrictions VALUES(7,'WEB,, HD','CAM','historic preferred','[7]');".into(),
        _ => format!("CREATE TABLE ReleaseProfiles(Id INTEGER,Name TEXT,Enabled INTEGER,Required TEXT,Ignored TEXT,IndexerId INTEGER,Tags TEXT);INSERT INTO ReleaseProfiles VALUES(7,'Imported',{},'[\"WEB\",\"HD\"]','[\"CAM\"]',{},'[7]');",i32::from(enabled),indexer),
    }
}
#[tokio::test]
async fn snapshot_release_profiles_three_versions_rollback_replay_and_deleted_identity() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        let scratch = Scratch::new();
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let domain = if version == 233 { "tv" } else { "movies" };
        let bytes = fixture(
            &scratch,
            version,
            "normal",
            &source_profiles(version, true, 0),
        )
        .await;
        let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        snapshots::import(&db, app, bytes.clone(), true)
            .await
            .unwrap();
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
        c.execute_batch("CREATE TRIGGER fail_release_activation BEFORE UPDATE OF release_profile_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM tags").await, 0);
        c.execute("DROP TRIGGER fail_release_activation", ())
            .await
            .unwrap();
        let imported = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert_eq!(imported.conflicts, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 1);
        if version == 206 {
            assert!(
                imported
                    .unsupported
                    .iter()
                    .any(|u| u.columns.iter().any(|s| s.contains("Preferred")))
            );
        }
        let (base, server) = server(db.clone(), "11").await;
        let path = format!("/api/v1/{domain}/release-profiles");
        let (_, catalog) = request(&base, "GET", &path, Value::Null).await;
        let mut saved = catalog["profiles"][0].clone();
        let id = saved["id"].as_i64().unwrap();
        let revision = catalog["revision"].as_i64().unwrap();
        saved.as_object_mut().unwrap().remove("id");
        saved.as_object_mut().unwrap().remove("media_type");
        assert_eq!(
            saved["required"],
            if version == 206 {
                json!(["WEB", " HD"])
            } else {
                json!(["WEB", "HD"])
            }
        );
        let replay = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert_eq!(replay.conflicts, 0);
        // Native same-value PUT fences later backfill without breaking exact replay.
        let (status, edited) = request(
            &base,
            "PUT",
            &format!("{path}/{id}"),
            json!({"revision":revision,"profile":saved}),
        )
        .await;
        assert_eq!(status, 200, "{edited}");
        assert_eq!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .conflicts,
            0
        );
        let revision = edited["revision"].as_i64().unwrap();
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("{path}/{id}?revision={revision}"),
                Value::Null
            )
            .await
            .0,
            200
        );
        let (status, recreated) = request(
            &base,
            "POST",
            &path,
            json!({"revision":revision+1,"profile":saved}),
        )
        .await;
        assert_eq!(status, 201, "{recreated}");
        assert_ne!(recreated["profiles"][0]["id"], id);
        assert!(
            snapshots::import(&db, app, bytes, false)
                .await
                .unwrap()
                .conflicts
                > 0
        );
        server.close().await;
    }
}
#[tokio::test]
async fn snapshot_unresolved_disabled_indexers_never_broaden_and_unsupported_is_archived() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 242] {
        for enabled in [false, true] {
            let scratch = Scratch::new();
            let app = if version == 233 {
                Application::Sonarr
            } else {
                Application::Radarr
            };
            let domain = if version == 233 { "tv" } else { "movies" };
            let bytes = fixture(
                &scratch,
                version,
                "missing-provider",
                &source_profiles(version, enabled, 77),
            )
            .await;
            let db = Arc::new(Database::open_local(scratch.0.join("db")).await.unwrap());
            let report = snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap();
            let c = db.connect().await.unwrap();
            if enabled {
                assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
                assert_eq!(
                    scalar(
                        &c,
                        "SELECT sum(release_profile_version) FROM snapshot_imports"
                    )
                    .await,
                    0
                );
                assert!(
                    report
                        .unsupported
                        .iter()
                        .any(|u| u.columns.iter().any(|s| s.contains("indexer unavailable")))
                );
            } else {
                assert_eq!(scalar(&c,"SELECT count(*) FROM release_profile_indexers WHERE provider_id IS NULL AND source_id=77").await,1);
                let (base, server) = server(db.clone(), "11").await;
                let path = format!("/api/v1/{domain}/release-profiles");
                let (_, catalog) = request(&base, "GET", &path, Value::Null).await;
                let mut value = catalog["profiles"][0].clone();
                let id = value["id"].as_i64().unwrap();
                value.as_object_mut().unwrap().remove("id");
                value.as_object_mut().unwrap().remove("media_type");
                value["enabled"] = json!(true);
                assert_eq!(
                    request(
                        &base,
                        "PUT",
                        &format!("{path}/{id}"),
                        json!({"revision":catalog["revision"],"profile":value})
                    )
                    .await
                    .0,
                    400
                );
                let (status, provider) =
                    request(&base, "POST", "/api/v1/providers", provider()).await;
                assert_eq!(status, 201);
                let provider_id = provider["id"].as_str().unwrap();
                // Late provenance availability must not rewrite an activated unresolved identity.
                c.execute("INSERT INTO snapshot_provider_mappings(application,fingerprint,source_table,source_id,provider_id,provider_revision)VALUES(?,?,'Indexers',77,?,1)",libsql::params![if version==233{"sonarr"}else{"radarr"},report.fingerprint.clone(),provider_id]).await.unwrap();
                assert_eq!(
                    snapshots::import(&db, app, bytes, false)
                        .await
                        .unwrap()
                        .conflicts,
                    0
                );
                assert_eq!(scalar(&c,"SELECT count(*) FROM release_profile_indexers WHERE provider_id IS NULL AND source_id=77").await,1);
                value["indexers"] = json!([{"kind":"provider","id":provider_id}]);
                let (status, resolved) = request(
                    &base,
                    "PUT",
                    &format!("{path}/{id}"),
                    json!({"revision":catalog["revision"],"profile":value}),
                )
                .await;
                assert_eq!(status, 200, "{resolved}");
                assert_eq!(scalar(&c,"SELECT count(*) FROM release_profile_indexers WHERE provider_id IS NOT NULL").await,1);
                server.close().await;
            }
        }
        for (suffix, mutation, invalid) in [
            (
                "unknown",
                "ALTER TABLE ReleaseProfiles ADD COLUMN FutureMeaning INTEGER",
                false,
            ),
            (
                "regex",
                "UPDATE ReleaseProfiles SET Required='[\"/[a-z-[aeiou]]/\"]'",
                false,
            ),
            (
                "invalid-regex",
                "UPDATE ReleaseProfiles SET Required='[\"/(/\"]'",
                true,
            ),
            (
                "malformed",
                "UPDATE ReleaseProfiles SET Required='null'",
                true,
            ),
        ] {
            let scratch = Scratch::new();
            let app = if version == 233 {
                Application::Sonarr
            } else {
                Application::Radarr
            };
            let bytes = fixture(
                &scratch,
                version,
                suffix,
                &format!("{}{};", source_profiles(version, true, 0), mutation),
            )
            .await;
            let db = Database::open_local(scratch.0.join("db")).await.unwrap();
            let result = snapshots::import(&db, app, bytes, false).await;
            assert_eq!(result.is_err(), invalid);
            let c = db.connect().await.unwrap();
            assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM snapshot_imports").await,
                i64::from(!invalid)
            );
        }
    }
}
#[tokio::test]
async fn snapshot_provider_activation_supported_n_and_whole_catalog_unsupported() {
    let _serial = SNAPSHOTS.lock().await;
    for version in [233, 242] {
        let scratch = Scratch::new();
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let provider_source = r#"CREATE TABLE Indexers(Id INTEGER,Name TEXT,Implementation TEXT,ConfigContract TEXT,Settings TEXT,Priority INTEGER,EnableRss INTEGER);INSERT INTO Indexers VALUES(77,'Imported indexer','Torznab','TorznabSettings','{"baseUrl":"https://indexer.example","apiPath":"/api","apiKey":"SYNTHETIC_PROFILE_SECRET","categories":[5000],"animeCategories":[],"animeStandardFormatSearch":false}',1,1);"#;
        let bytes = fixture(
            &scratch,
            version,
            "provider",
            &format!(
                "{}{}UPDATE ReleaseProfiles SET Required='[\"/a/n\"]';",
                source_profiles(version, true, 77),
                provider_source
            ),
        )
        .await;
        let db = Database::open_local(scratch.0.join("db")).await.unwrap();
        let key = hrrdarr::providers::CredentialKey::from_hex(&"11".repeat(32)).unwrap();
        // First archive without provider reconstruction: later opt-in must backfill
        // the still-inactive catalog without mistaking archive presence for activation.
        let archived = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(archived.applied);
        let before = db.connect().await.unwrap();
        assert_eq!(
            scalar(&before, "SELECT count(*) FROM release_profiles").await,
            0
        );
        assert_eq!(
            scalar(
                &before,
                "SELECT sum(release_profile_version) FROM snapshot_imports"
            )
            .await,
            0
        );
        let report = snapshots::import_with_providers(&db, app, bytes, false, Some(&key))
            .await
            .unwrap();
        assert_eq!(report.conflicts, 0);
        let c = db.connect().await.unwrap();
        // n is supported by the independent matcher; source activation must honor that.
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM release_profiles WHERE enabled=1").await,
            1
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM release_profile_indexers WHERE provider_id IS NOT NULL"
            )
            .await,
            1
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM providers").await, 1);
        let mixed=fixture(&scratch,version,"mixed",&format!("{}INSERT INTO ReleaseProfiles SELECT * FROM ReleaseProfiles;UPDATE ReleaseProfiles SET Id=8,Required='[\"/[a-z-[aeiou]]/\"]' WHERE rowid=2;",source_profiles(version,true,0))).await;
        let other = Database::open_local(scratch.0.join("mixed-db"))
            .await
            .unwrap();
        let report = snapshots::import(&other, app, mixed, false).await.unwrap();
        assert!(!report.unsupported.is_empty());
        let c = other.connect().await.unwrap();
        assert_eq!(scalar(&c, "SELECT count(*) FROM release_profiles").await, 0);
        assert_eq!(
            scalar(
                &c,
                "SELECT sum(release_profile_version) FROM snapshot_imports"
            )
            .await,
            0
        );
    }
}
