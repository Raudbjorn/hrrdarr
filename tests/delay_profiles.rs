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
        let p = std::env::temp_dir().join(format!("hrrdarr-delay-policy-{}", uuid::Uuid::new_v4()));
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
async fn server(db: Arc<Database>) -> (String, Server) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (_, transport) = hrrdarr::providers::router_with_refresh(db.clone(), None);
    let app = hrrdarr::delay_profiles::router(db.clone())
        .merge(hrrdarr::tags::router(db.clone()))
        .merge(hrrdarr::library::router(db.clone()))
        .merge(hrrdarr::search::router(db, transport));
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
fn input(tags: &[i64], minutes: u32) -> Value {
    json!({"settings":{"torrent_delay_minutes":minutes,"usenet_delay_minutes":0,"enable_torrent":true,"enable_usenet":true,"preferred_protocol":"usenet","bypass_if_highest_quality":true,"bypass_if_above_custom_format_score":false,"minimum_custom_format_score":0},"tag_ids":tags})
}
#[tokio::test]
async fn delay_http_order_selection_cas_and_wake() {
    for (d, other, media, global, join) in [
        (
            "tv",
            "movies",
            hrrdarr::api::MediaDomain::Tv,
            1,
            "series_tags(series_id,tag_id)",
        ),
        (
            "movies",
            "tv",
            hrrdarr::api::MediaDomain::Movies,
            2,
            "movie_tags(movie_id,tag_id)",
        ),
    ] {
        let s = Scratch::new();
        let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        c.execute_batch(&format!("INSERT INTO series(id,title,path)VALUES(1,'TV','/synthetic/tv');INSERT INTO movie_metadata(id,title)VALUES(1,'Movie');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/synthetic/movie');INSERT INTO tags(id,media_type,label)VALUES(10,'{d}','first'),(11,'{d}','second'),(12,'{other}','other');INSERT INTO {join}VALUES(1,10);")).await.unwrap();
        let _tv = pending(&c, "tv").await;
        let _movies = pending(&c, "movies").await;
        let (base, _server) = server(db.clone()).await;
        let path_value = format!("/api/v1/{d}/delay-profiles");
        let path = path_value.as_str();
        let (status, all) = request(&base, "GET", path, Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(all["configured"], false);
        assert_eq!(
            all["profiles"][0]["settings"]["semantics"],
            "legacy_age_only"
        );
        assert!(matches!(
            hrrdarr::delay_profiles::select(&c, media, 1).await.unwrap(),
            hrrdarr::delay_profiles::EffectiveDelay::Unconfigured { revision: 1 }
        ));
        let (status, all) = request(
            &base,
            "PUT",
            &format!("{path}/{global}"),
            json!({"revision":1,"profile":input(&[],30)}),
        )
        .await;
        assert_eq!(status, 200, "{all}");
        assert_eq!(all["revision"], 2);
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT not_before FROM rss_candidates WHERE media_type='{d}'")
            )
            .await,
            0
        );
        assert_eq!(
            scalar(
                &c,
                &format!("SELECT not_before FROM rss_candidates WHERE media_type='{other}'")
            )
            .await,
            100000
        );
        assert_eq!(
            request(
                &base,
                "PUT",
                &format!("{path}/{global}"),
                json!({"revision":1,"profile":input(&[],1)})
            )
            .await
            .0,
            409
        );
        let (status, all) = request(
            &base,
            "POST",
            path,
            json!({"revision":2,"profile":input(&[10],60)}),
        )
        .await;
        assert_eq!(status, 201, "{all}");
        let a = all["profiles"][0]["id"].as_i64().unwrap();
        let (status, all) = request(
            &base,
            "POST",
            path,
            json!({"revision":3,"profile":input(&[11],90)}),
        )
        .await;
        assert_eq!(status, 201, "{all}");
        let b = all["profiles"][1]["id"].as_i64().unwrap();
        assert_eq!(
            request(
                &base,
                "POST",
                path,
                json!({"revision":4,"profile":input(&[12],1)})
            )
            .await
            .0,
            400
        );
        assert_eq!(
            request(
                &base,
                "POST",
                path,
                json!({"revision":4,"profile":input(&[10],1)})
            )
            .await
            .0,
            409
        );
        let effective = hrrdarr::delay_profiles::select(&c, media, 1).await.unwrap();
        assert!(
            matches!(effective,hrrdarr::delay_profiles::EffectiveDelay::Profile{id,settings,..}if id==a&&settings.torrent_delay_minutes==60)
        );
        let (status, all) = request(
            &base,
            "PUT",
            &format!("{path}/reorder"),
            json!({"revision":4,"ids":[b,a]}),
        )
        .await;
        assert_eq!(status, 200, "{all}");
        assert_eq!(all["profiles"][0]["id"], b);
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("{path}/{global}?revision=5"),
                Value::Null
            )
            .await
            .0,
            409
        );
        assert_eq!(
            request(
                &base,
                "DELETE",
                &format!("/api/v1/{d}/tags/11"),
                Value::Null
            )
            .await
            .0,
            409
        );
        let (status, detail) = request(
            &base,
            "GET",
            &format!("/api/v1/{d}/tags/detail/11"),
            Value::Null,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(detail["delay_profile_ids"], json!([b]));
        assert!(
            c.execute(
                &format!("UPDATE delay_profile_tags SET profile_id={global} WHERE tag_id=11"),
                ()
            )
            .await
            .is_err()
        );
        let (status, all) = request(
            &base,
            "DELETE",
            &format!("{path}/{b}?revision=5"),
            Value::Null,
        )
        .await;
        assert_eq!(status, 200, "{all}");
        assert_eq!(all["profiles"][0]["position"], 1);
        assert_eq!(
            request(
                &base,
                "GET",
                &format!("/api/v1/{other}/delay-profiles/{global}"),
                Value::Null
            )
            .await
            .0,
            404
        );
        _server.close().await;
    }
}

fn source_delay(version: i64) -> String {
    let modern = version != 206;
    let extra = if modern {
        ",BypassIfAboveCustomFormatScore INTEGER,MinimumCustomFormatScore INTEGER"
    } else {
        ""
    };
    let values = if modern { ",0,NULL" } else { "" };
    format!(
        "CREATE TABLE Config(Id INTEGER,Key TEXT,Value TEXT);INSERT INTO Config VALUES(1,'AvailabilityDelay','-2');CREATE TABLE DelayProfiles(Id INTEGER,EnableUsenet INTEGER,EnableTorrent INTEGER,PreferredProtocol INTEGER,UsenetDelay INTEGER,TorrentDelay INTEGER,\"Order\" INTEGER,BypassIfHighestQuality INTEGER,Tags TEXT{extra});INSERT INTO DelayProfiles VALUES(1,1,1,1,0,30,2147483647,1,'[]'{values}),(7,1,1,2,15,60,10,0,'[7]'{values});"
    )
}
#[tokio::test]
async fn delay_snapshot_graph_activation_replay_and_rollback() {
    let _guard = SNAPSHOTS.lock().await;
    for version in [233, 206, 242] {
        let s = Scratch::new();
        let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
        let c = db.connect().await.unwrap();
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let domain = if version == 233 { "tv" } else { "movies" };
        let bytes = fixture(&s, version, "good", &source_delay(version)).await;
        let dry = snapshots::import(&db, app, bytes.clone(), true)
            .await
            .unwrap();
        assert!(!dry.applied);
        assert_eq!(dry.conflicts, 0);
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        c.execute_batch("CREATE TRIGGER fail_delay_finish BEFORE UPDATE OF delay_profile_version ON snapshot_imports BEGIN SELECT RAISE(ABORT,'injected'); END;").await.unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .is_err()
        );
        assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM release_delay_policies").await,
            0
        );
        c.execute_batch("DROP TRIGGER fail_delay_finish")
            .await
            .unwrap();
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM delay_profiles WHERE is_global=0 AND position=1"
            )
            .await,
            1
        );
        assert_eq!(scalar(&c,"SELECT count(*) FROM delay_profiles WHERE semantics='profile' AND minimum_custom_format_score=0").await,2);
        assert!(
            snapshots::import(&db, app, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        let media = if version == 233 {
            hrrdarr::api::MediaDomain::Tv
        } else {
            hrrdarr::api::MediaDomain::Movies
        };
        let owner = scalar(
            &c,
            if version == 233 {
                "SELECT id FROM series LIMIT 1"
            } else {
                "SELECT id FROM movies LIMIT 1"
            },
        )
        .await;
        assert!(
            matches!(hrrdarr::delay_profiles::select(&c,media,owner).await.unwrap(),hrrdarr::delay_profiles::EffectiveDelay::Profile{settings,..}if settings.torrent_delay_minutes==60&&settings.preferred_protocol==hrrdarr::delay_profiles::Protocol::Torrent)
        );
        let (base, _server) = server(db.clone()).await;
        let path = format!("/api/v1/{domain}/delay-profiles");
        let (_, catalog) = request(&base, "GET", &path, Value::Null).await;
        let id = catalog["profiles"][0]["id"].as_i64().unwrap();
        let revision = catalog["revision"].as_i64().unwrap();
        let tag = catalog["profiles"][0]["tag_ids"][0].as_i64().unwrap();
        // Recreate equal content must not resurrect the deleted source identity.
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
        let mut recreated = input(&[tag], 60);
        recreated["settings"]["usenet_delay_minutes"] = json!(15);
        recreated["settings"]["preferred_protocol"] = json!("torrent");
        recreated["settings"]["bypass_if_highest_quality"] = json!(false);
        let (status, new) = request(
            &base,
            "POST",
            &path,
            json!({"revision":revision+1,"profile":recreated}),
        )
        .await;
        assert_eq!(status, 201, "{new}");
        assert_ne!(new["profiles"][0]["id"], id);
        let replay = snapshots::import(&db, app, bytes, false).await.unwrap();
        assert!(!replay.applied);
        assert!(replay.conflicts > 0);
        _server.close().await;
    }
}
#[tokio::test]
async fn delay_snapshot_unknown_null_local_intent_and_legacy_conflicts() {
    let _guard = SNAPSHOTS.lock().await;
    for (suffix, extra, malformed) in [
        ("missing", String::new(), false),
        (
            "unknown",
            source_delay(233) + "ALTER TABLE DelayProfiles ADD COLUMN UnknownBehavior INTEGER;",
            false,
        ),
        (
            "protocol",
            source_delay(233) + "UPDATE DelayProfiles SET PreferredProtocol=3;",
            false,
        ),
        (
            "nullscore",
            source_delay(233) + "UPDATE DelayProfiles SET BypassIfAboveCustomFormatScore=1;",
            false,
        ),
        (
            "nulltags",
            source_delay(233) + "UPDATE DelayProfiles SET Tags=NULL WHERE Id=7;",
            true,
        ),
        (
            "duplicatetags",
            source_delay(233) + "UPDATE DelayProfiles SET Tags='[7,7]' WHERE Id=7;",
            true,
        ),
        (
            "malformed",
            source_delay(233) + "UPDATE DelayProfiles SET Tags='broken' WHERE Id=7;",
            true,
        ),
    ] {
        let s = Scratch::new();
        let db = Database::open_local(s.0.join("db")).await.unwrap();
        let bytes = fixture(&s, 233, suffix, &extra).await;
        let result = snapshots::import(&db, Application::Sonarr, bytes, false).await;
        if malformed {
            assert!(result.is_err(), "{suffix}")
        } else {
            let report = result.unwrap();
            assert!(report.applied, "{suffix}");
            assert!(
                report
                    .unsupported
                    .iter()
                    .any(|u| u.table == "DelayProfiles"),
                "{suffix}"
            );
        }
        let c = db.connect().await.unwrap();
        assert_eq!(
            scalar(&c, "SELECT count(*) FROM release_delay_policies").await,
            0
        );
        assert_eq!(
            scalar(
                &c,
                "SELECT count(*) FROM delay_profiles WHERE semantics='profile'"
            )
            .await,
            0
        );
    }
    // A configured age-only graph cannot be silently given a preferred protocol on import.
    let s = Scratch::new();
    let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute("INSERT INTO release_delay_policies VALUES('tv',30,0,0)", ())
        .await
        .unwrap();
    let bytes = fixture(&s, 233, "legacy", &source_delay(233)).await;
    let report = snapshots::import(&db, Application::Sonarr, bytes, false)
        .await
        .unwrap();
    assert!(!report.applied);
    assert!(report.conflicts > 0);
    assert_eq!(scalar(&c, "SELECT count(*) FROM snapshot_imports").await, 0);
    // Explicit native settings (including equal values) fence later source replacement.
    let (base, _server) = server(db.clone()).await;
    let path = "/api/v1/tv/delay-profiles/1";
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"revision":1,"profile":input(&[],30)})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(
            &base,
            "PUT",
            path,
            json!({"revision":2,"profile":input(&[],30)})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT locally_edited FROM delay_profile_domains WHERE media_type='tv'"
        )
        .await,
        1
    );
    let bytes = fixture(&s, 233, "local", &source_delay(233)).await;
    let report = snapshots::import(&db, Application::Sonarr, bytes, false)
        .await
        .unwrap();
    assert!(!report.applied);
    assert!(report.conflicts > 0);
    _server.close().await;
}
#[tokio::test]
async fn compatibility_http_preserves_full_fields_and_assignment_wakes() {
    let s = Scratch::new();
    let db = Arc::new(Database::open_local(s.0.join("db")).await.unwrap());
    let c = db.connect().await.unwrap();
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'TV','/synthetic');INSERT INTO tags(id,media_type,label)VALUES(10,'tv','tag');").await.unwrap();
    let (base, _server) = server(db.clone()).await;
    for (d, id, days) in [("tv", 1, 0), ("movies", 2, -3)] {
        let path = format!("/api/v1/{d}/delay-profiles/{id}");
        assert_eq!(
            request(
                &base,
                "PUT",
                &path,
                json!({"revision":1,"profile":input(&[],42)})
            )
            .await
            .0,
            200
        );
        assert_eq!(request(&base,"PUT",&format!("/api/v1/release-policies/{d}"),json!({"torrent_delay_minutes":17,"usenet_delay_minutes":23,"availability_delay_days":days})).await.0,200);
        let (_, all) = request(
            &base,
            "GET",
            &format!("/api/v1/{d}/delay-profiles"),
            Value::Null,
        )
        .await;
        assert_eq!(all["revision"], 3);
        assert_eq!(all["availability_delay_days"], days);
        assert_eq!(all["profiles"][0]["settings"]["semantics"], "profile");
        assert_eq!(all["profiles"][0]["settings"]["torrent_delay_minutes"], 17);
        assert_eq!(
            all["profiles"][0]["settings"]["bypass_if_highest_quality"],
            true
        );
    }
    let _tv = pending(&c, "tv").await;
    let _movie = pending(&c, "movies").await;
    assert_eq!(
        request(
            &base,
            "PUT",
            "/api/v1/tv/series/1",
            json!({"tags":{"mode":"replace","ids":[10]}})
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
    _server.close().await;
}

#[tokio::test]
async fn archived_delay_graph_backfills_once_and_activation_wakes() {
    let _guard = SNAPSHOTS.lock().await;
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await.unwrap();
    let c = db.connect().await.unwrap();
    let bytes = fixture(&s, 233, "backfill", &source_delay(233)).await;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .unwrap()
            .applied
    );
    // Restore the exact pre-adapter shape: archived core/tags remain; no active delay graph,
    // provenance or local intent existed. This is not a native deletion/recreation path.
    c.execute_batch("DELETE FROM delay_profiles WHERE is_global=0;DELETE FROM release_delay_policies;UPDATE delay_profiles SET semantics='legacy_age_only',enable_torrent=NULL,enable_usenet=NULL,preferred_protocol=NULL,bypass_if_highest_quality=NULL,bypass_if_above_custom_format_score=NULL,minimum_custom_format_score=NULL;UPDATE delay_profile_domains SET revision=1,locally_edited=0;UPDATE snapshot_imports SET delay_profile_version=0;DELETE FROM snapshot_mappings WHERE destination_table='delay_profiles';").await.unwrap();
    let _tv = pending(&c, "tv").await;
    let _movie = pending(&c, "movies").await;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        scalar(&c, "SELECT delay_profile_version FROM snapshot_imports").await,
        1
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
            "SELECT not_before FROM rss_candidates WHERE media_type='movies'"
        )
        .await,
        100000
    );
    let revision = scalar(
        &c,
        "SELECT revision FROM delay_profile_domains WHERE media_type='tv'",
    )
    .await;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes, false)
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT revision FROM delay_profile_domains WHERE media_type='tv'"
        )
        .await,
        revision
    );
}
#[tokio::test]
async fn legacy_source_with_newer_score_fields_stays_archived() {
    let _guard = SNAPSHOTS.lock().await;
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await.unwrap();
    let extra = source_delay(206)
        + "ALTER TABLE DelayProfiles ADD COLUMN BypassIfAboveCustomFormatScore INTEGER;ALTER TABLE DelayProfiles ADD COLUMN MinimumCustomFormatScore INTEGER;UPDATE DelayProfiles SET BypassIfAboveCustomFormatScore=1,MinimumCustomFormatScore=123;";
    let bytes = fixture(&s, 206, "unexpected-fields", &extra).await;
    let report = snapshots::import(&db, Application::Radarr, bytes, false)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(report.unsupported.iter().any(|u| {
        u.table == "DelayProfiles"
            && u.columns
                .iter()
                .any(|c| c.contains("unexpected delay score fields"))
    }));
    let c = db.connect().await.unwrap();
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM release_delay_policies").await,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT delay_profile_version FROM snapshot_imports").await,
        0
    );
}
