use hrrdarr::{
    db::Database,
    quality_profiles,
    snapshots::{self, Application},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("hrrdarr-profile-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn items(tv: bool) -> Value {
    let mut leaf = json!({"quality":1,"items":[],"allowed":true});
    if tv {
        leaf["minSize"] = json!(2);
        leaf["maxSize"] = json!(20);
        leaf["preferredSize"] = json!(10);
    } else {
        // Older embedded profile documents retain the default leaf ID explicitly.
        leaf["id"] = json!(0);
    }
    json!([leaf,{"id":1001,"name":"HD tier","quality":null,"allowed":true,"items":[{"quality":3,"items":[],"allowed":true},{"quality":7,"items":[],"allowed":false}]}])
}
async fn fixture(s: &Scratch, version: i64, changes: &str, graph: Option<Value>) -> Vec<u8> {
    let path = s.0.join(format!("source-{}.db", uuid::Uuid::new_v4()));
    let db = libsql::Builder::new_local(&path).build().await.unwrap();
    let c = db.connect().unwrap();
    let tv = version == 233;
    c.execute_batch(&format!(
        "CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});"
    ))
    .await
    .unwrap();
    c.execute_batch("CREATE TABLE CustomFormats(Id INTEGER,Name TEXT);")
        .await
        .unwrap();
    if tv {
        c.execute_batch("CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,QualityProfileId INTEGER);INSERT INTO Series VALUES(1,123,'TV',2020,'/profile-fixture/tv',1,'[{\"seasonNumber\":1,\"monitored\":true}]',7);CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,1,1,1,'Episode',1,0);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);").await.unwrap();
    } else if version == 206 {
        c.execute_batch("CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,ProfileId INTEGER);INSERT INTO Movies VALUES(1,206,NULL,'Old movie',2000,'/profile-fixture/movie206',1,0,7);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);").await.unwrap();
    } else {
        c.execute_batch("CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(1,242,NULL,'New movie',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,QualityProfileId INTEGER);INSERT INTO Movies VALUES(1,1,'/profile-fixture/movie242',1,0,7);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);").await.unwrap();
    }
    let table = if version == 206 {
        "Profiles"
    } else {
        "QualityProfiles"
    };
    let increment = if version == 206 {
        ""
    } else {
        ",MinUpgradeFormatScore INTEGER"
    };
    c.execute_batch(&format!("CREATE TABLE {table}(Id INTEGER,Name TEXT,Items TEXT,UpgradeAllowed INTEGER,Cutoff INTEGER,MinFormatScore INTEGER,CutoffFormatScore INTEGER,FormatItems TEXT,Language INTEGER{increment});")).await.unwrap();
    // Sonarr has no Language column; a synthetic opposite-domain field must not imply support.
    if tv {
        c.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN Language;"))
            .await
            .unwrap();
    }
    let mut columns =
        "Id,Name,Items,UpgradeAllowed,Cutoff,MinFormatScore,CutoffFormatScore,FormatItems"
            .to_owned();
    let mut values = vec![
        libsql::Value::Integer(7),
        format!("Profile{version}").into(),
        graph.unwrap_or_else(|| items(tv)).to_string().into(),
        0.into(),
        1001.into(),
        (-10).into(),
        (-20).into(),
        "[]".into(),
    ];
    if !tv {
        columns.push_str(",Language");
        values.push((-2).into());
    }
    if version != 206 {
        columns.push_str(",MinUpgradeFormatScore");
        values.push(2.into());
    }
    c.execute(
        &format!(
            "INSERT INTO {table}({columns}) VALUES({})",
            vec!["?"; values.len()].join(",")
        ),
        values,
    )
    .await
    .unwrap();
    c.execute_batch(changes).await.unwrap();
    drop(c);
    drop(db);
    std::fs::read(path).unwrap()
}
async fn count(c: &libsql::Connection, table: &str) -> i64 {
    c.query(&format!("SELECT count(*) FROM {table}"), ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn profile_id(c: &libsql::Connection, fingerprint: &str) -> i64 {
    c.query("SELECT destination_id FROM snapshot_mappings WHERE fingerprint=? AND destination_table='quality_profiles' AND source_id=7",[fingerprint]).await.unwrap().next().await.unwrap().unwrap().get(0).unwrap()
}
async fn read(client: &reqwest::Client, base: &str, media: &str, id: i64) -> Value {
    let response = client
        .get(format!("{base}/api/v1/{media}/quality-profiles/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    serde_json::from_str(&response.text().await.unwrap()).unwrap()
}
#[tokio::test]
async fn complete_profiles_activate_with_scoped_assignments_and_exact_replay() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let path = s.0.join("dest");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let c = db.connect().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = quality_profiles::router(db.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut expected = Vec::new();
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let media = if version == 233 { "tv" } else { "movies" };
        let sql = if version == 206 {
            ""
        } else {
            "UPDATE QualityProfiles SET Name='Shared profile'"
        };
        let bytes = fixture(&s, version, sql, None).await;
        let before = count(&c, "quality_profiles").await;
        let settings_before = count(&c, "library_settings").await;
        let mappings_before = count(&c, "snapshot_mappings").await;
        let dry = snapshots::import(&db, app, bytes.clone(), true)
            .await
            .unwrap();
        assert!(!dry.applied);
        assert_eq!(count(&c, "quality_profiles").await, before);
        // Replay must not silently repair deleted local rows or provenance mappings.
        assert_eq!(count(&c, "library_settings").await, settings_before);
        assert_eq!(count(&c, "snapshot_mappings").await, mappings_before);
        let report = snapshots::import(&db, app, bytes.clone(), false)
            .await
            .unwrap();
        assert!(
            report.applied,
            "{}",
            serde_json::to_string(&report).unwrap()
        );
        assert_eq!(report.conflicts, 0);
        let id = profile_id(&c, &report.fingerprint).await;
        let p = read(&client, &base, media, id).await;
        assert_eq!(
            p["name"],
            if version == 206 {
                "Profile206"
            } else {
                "Shared profile"
            }
        );
        assert_eq!(p["media_type"], media);
        assert_eq!(p["items"].as_array().unwrap().len(), 2);
        assert_eq!(p["items"][0]["quality_id"], 1);
        assert_eq!(p["items"][1]["name"], "HD tier");
        assert_eq!(p["items"][1]["items"][0]["quality_id"], 3);
        assert_eq!(p["items"][1]["items"][1]["quality_id"], 7);
        assert_eq!(p["items"][1]["items"][1]["allowed"], false);
        assert_eq!(
            p["policy"],
            json!({"upgrade_allowed":false,"cutoff":{"kind":"group","position":1},"min_format_score":-10,"cutoff_format_score":-20,"min_upgrade_format_score":if version==206 {1}else{2},"language_id":if version==233 {Value::Null}else{json!(-2)},"format_items":[]})
        );
        if version == 233 {
            assert_eq!(p["items"][0]["min_size"], 2.0);
            assert_eq!(p["items"][0]["preferred_size"], 10.0);
            assert_eq!(p["items"][0]["max_size"], 20.0);
        }
        let row=c.query("SELECT media_type,quality_profile_id FROM library_settings WHERE quality_profile_id=?",[id]).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), media);
        assert_eq!(row.get::<i64>(1).unwrap(), id);
        drop(row);
        let repeat = snapshots::import(&db, app, bytes, false).await.unwrap();
        assert!(repeat.applied);
        assert_eq!(repeat.mapped, 0);
        assert_eq!(read(&client, &base, media, id).await, p);
        expected.push((id, p));
    }
    assert_eq!(count(&c, "quality_profiles").await, 3);
    assert_eq!(count(&c, "quality_profile_policies").await, 3);
    assert_eq!(count(&c, "episodes").await, 1);
    assert_eq!(count(&c, "movies").await, 2);
    // A new backup identity may reuse a profile only on complete graph/policy equality.
    let duplicate = fixture(
        &s,
        233,
        "UPDATE QualityProfiles SET Name='Shared profile';PRAGMA user_version=1",
        None,
    )
    .await;
    let report = snapshots::import(&db, Application::Sonarr, duplicate, false)
        .await
        .unwrap();
    assert!(report.applied);
    assert_eq!(profile_id(&c, &report.fingerprint).await, expected[0].0);
    assert_eq!(count(&c, "quality_profiles").await, 3);
    for changed_graph in [false, true] {
        let mut graph = items(true);
        if changed_graph {
            graph[1]["items"][1]["allowed"] = json!(true);
        }
        let sql = if changed_graph {
            "UPDATE QualityProfiles SET Name='Shared profile'"
        } else {
            "UPDATE QualityProfiles SET Name='Shared profile',MinUpgradeFormatScore=3"
        };
        let bytes = fixture(&s, 233, sql, Some(graph)).await;
        let archives = count(&c, "snapshot_imports").await;
        let report = snapshots::import(&db, Application::Sonarr, bytes, false)
            .await
            .unwrap();
        assert!(!report.applied && report.conflicts > 0);
        assert_eq!(count(&c, "snapshot_imports").await, archives);
        assert_eq!(
            read(&client, &base, "tv", expected[0].0).await,
            expected[0].1
        );
    }
    server.abort();
    let _ = server.await;
    drop(c);
    drop(db);
    let db = Database::open_local(path).await.unwrap();
    let c = db.connect().await.unwrap();
    assert_eq!(count(&c, "quality_profiles").await, 3);
    for (id, p) in expected {
        let name = c
            .query("SELECT name FROM quality_profiles WHERE id=?", [id])
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(name, p["name"]);
    }
}

#[tokio::test]
async fn unsupported_profiles_are_never_truncated_and_conflicts_roll_back() {
    let _guard = IMPORT_LOCK.lock().await;
    let s = Scratch::new();
    let cases = [
        "UPDATE QualityProfiles SET FormatItems='[{\"format\":9,\"score\":10}]'",
        "INSERT INTO CustomFormats VALUES(9,'PRIVATE_CF_SENTINEL')",
        "DROP TABLE CustomFormats",
        "UPDATE QualityProfiles SET Cutoff=9999,Items='[{\"quality\":9999,\"allowed\":true,\"items\":[]}]'",
        "UPDATE QualityProfiles SET Items='[{\"quality\":1,\"allowed\":true,\"items\":[],\"PRIVATE_KEY_SENTINEL\":true}]'",
        "UPDATE QualityProfiles SET Items='[{\"id\":1001,\"quality\":null,\"name\":\"Nested\",\"allowed\":true,\"items\":[{\"id\":1002,\"quality\":null,\"name\":\"Child\",\"allowed\":true,\"items\":[{\"quality\":1,\"allowed\":true,\"items\":[]}]}]}]'",
    ];
    for (index, sql) in cases.iter().enumerate() {
        let db = Database::open_local(s.0.join(format!("unsupported{index}")))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        let bytes = fixture(&s, 233, sql, None).await;
        let report = snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .unwrap();
        assert!(
            report.applied,
            "{}",
            serde_json::to_string(&report).unwrap()
        );
        assert_eq!(count(&c, "quality_profiles").await, 0);
        assert_eq!(count(&c, "quality_profile_policies").await, 0);
        assert!(
            report
                .unsupported
                .iter()
                .any(|u| u.table == "QualityProfiles")
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("PRIVATE_KEY_SENTINEL")
        );
        let row = c
            .query(
                "SELECT quality_profile_id FROM library_settings WHERE media_type='tv'",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert!(row.get::<Option<i64>>(0).unwrap().is_none());
        drop(row);
        assert!(count(&c, "snapshot_records").await > 0);
        assert!(
            snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
                .await
                .unwrap()
                .applied
        );
        if index == 0 {
            c.execute(
                "INSERT INTO quality_profiles(media_type,name) VALUES('tv','Local policy')",
                (),
            )
            .await
            .unwrap();
            let local = c.last_insert_rowid();
            c.execute("UPDATE library_settings SET quality_profile_id=?", [local])
                .await
                .unwrap();
            let report = snapshots::import(&db, Application::Sonarr, bytes, false)
                .await
                .unwrap();
            assert!(!report.applied && report.conflicts > 0);
            assert_eq!(
                c.query("SELECT quality_profile_id FROM library_settings", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                local
            );
        }
    }
    for (index, sql) in [
        "UPDATE QualityProfiles SET Items='not-json'",
        "INSERT INTO QualityProfiles SELECT * FROM QualityProfiles",
        "WITH RECURSIVE ids(n) AS (SELECT 8 UNION ALL SELECT n+1 FROM ids WHERE n<263) INSERT INTO QualityProfiles SELECT n,'Bound'||n,Items,UpgradeAllowed,Cutoff,MinFormatScore,CutoffFormatScore,FormatItems,MinUpgradeFormatScore FROM ids CROSS JOIN QualityProfiles WHERE Id=7",
    ]
    .iter()
    .enumerate()
    {
        let db = Database::open_local(s.0.join(format!("malformed{index}")))
            .await
            .unwrap();
        let c = db.connect().await.unwrap();
        let source = fixture(&s, 233, sql, None).await;
        assert!(
            snapshots::import(&db, Application::Sonarr, source, false)
                .await
                .is_err()
        );
        for table in [
            "series",
            "quality_profiles",
            "snapshot_imports",
            "snapshot_records",
        ] {
            assert_eq!(count(&c, table).await, 0);
        }
    }
    for mutation in [
        "name",
        "assignment",
        "deleted",
        "settings_mapping",
        "settings_row",
    ] {
        let db = Database::open_local(s.0.join(mutation)).await.unwrap();
        let c = db.connect().await.unwrap();
        let bytes = fixture(&s, 233, "", None).await;
        let first = snapshots::import(&db, Application::Sonarr, bytes.clone(), false)
            .await
            .unwrap();
        assert!(first.applied);
        let id = profile_id(&c, &first.fingerprint).await;
        match mutation {
            "name" => {
                c.execute(
                    "UPDATE quality_profiles SET name='Local override' WHERE id=?",
                    [id],
                )
                .await
                .unwrap();
            }
            "assignment" => {
                c.execute("UPDATE library_settings SET quality_profile_id=NULL", ())
                    .await
                    .unwrap();
            }
            "settings_mapping" => {
                c.execute(
                    "DELETE FROM snapshot_mappings WHERE destination_table='library_settings'",
                    (),
                )
                .await
                .unwrap();
            }
            "settings_row" => {
                c.execute("DELETE FROM library_settings", ()).await.unwrap();
            }
            _ => {
                c.execute_batch("UPDATE library_settings SET quality_profile_id=NULL;DELETE FROM quality_profile_policies;DELETE FROM quality_profile_items;DELETE FROM quality_profile_groups;DELETE FROM quality_profiles;").await.unwrap();
            }
        }
        let before = count(&c, "quality_profiles").await;
        let settings_before = count(&c, "library_settings").await;
        let mappings_before = count(&c, "snapshot_mappings").await;
        let report = snapshots::import(&db, Application::Sonarr, bytes, false)
            .await
            .unwrap();
        assert!(!report.applied);
        assert!(report.conflicts > 0);
        assert_eq!(count(&c, "quality_profiles").await, before);
        // Replay must not silently repair deleted local rows or provenance mappings.
        assert_eq!(count(&c, "library_settings").await, settings_before);
        assert_eq!(count(&c, "snapshot_mappings").await, mappings_before);
        if mutation == "name" {
            assert_eq!(
                c.query("SELECT name FROM quality_profiles WHERE id=?", [id])
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<String>(0)
                    .unwrap(),
                "Local override"
            );
        } else if mutation == "assignment" || mutation == "deleted" {
            assert!(
                c.query("SELECT quality_profile_id FROM library_settings", ())
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<Option<i64>>(0)
                    .unwrap()
                    .is_none()
            );
        }
    }
    let db = Database::open_local(s.0.join("oversized-graph"))
        .await
        .unwrap();
    let c = db.connect().await.unwrap();
    let graph = Value::Array(
        (0..65)
            .map(|_| json!({"quality":1,"allowed":true,"items":[]}))
            .collect(),
    );
    let bytes = fixture(&s, 233, "UPDATE QualityProfiles SET Cutoff=1", Some(graph)).await;
    let report = snapshots::import(&db, Application::Sonarr, bytes, false)
        .await
        .unwrap();
    assert!(report.applied);
    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.table == "QualityProfiles")
    );
    assert_eq!(count(&c, "quality_profiles").await, 0);
    let db = Database::open_local(s.0.join("legacy-candidate"))
        .await
        .unwrap();
    let c = db.connect().await.unwrap();
    let bytes = fixture(&s, 233, "", None).await;
    assert!(
        snapshots::import(&db, Application::Sonarr, bytes, false)
            .await
            .unwrap()
            .applied
    );
    c.execute("DELETE FROM quality_profile_policies", ())
        .await
        .unwrap();
    let archives = count(&c, "snapshot_imports").await;
    let bytes = fixture(&s, 233, "PRAGMA user_version=3", None).await;
    let report = snapshots::import(&db, Application::Sonarr, bytes, false)
        .await
        .unwrap();
    assert!(!report.applied && report.conflicts > 0);
    assert_eq!(count(&c, "quality_profile_policies").await, 0);
    assert_eq!(count(&c, "snapshot_imports").await, archives);
    let db = Database::open_local(s.0.join("late")).await.unwrap();
    let c = db.connect().await.unwrap();
    c.execute_batch("CREATE TRIGGER reject_snapshot_policy BEFORE INSERT ON quality_profile_policies BEGIN SELECT RAISE(ABORT,'late profile fixture');END;").await.unwrap();
    let source = fixture(&s, 233, "", None).await;
    assert!(
        snapshots::import(&db, Application::Sonarr, source.clone(), false)
            .await
            .is_err()
    );
    for table in [
        "series",
        "episodes",
        "quality_profiles",
        "quality_profile_groups",
        "quality_profile_items",
        "quality_profile_policies",
        "library_settings",
        "snapshot_imports",
        "snapshot_mappings",
        "snapshot_records",
    ] {
        assert_eq!(count(&c, table).await, 0, "{table}");
    }
    c.execute_batch("DROP TRIGGER reject_snapshot_policy;")
        .await
        .unwrap();
    assert!(
        snapshots::import(&db, Application::Sonarr, source, false)
            .await
            .unwrap()
            .applied
    );
}
