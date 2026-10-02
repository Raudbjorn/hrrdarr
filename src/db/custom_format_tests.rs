use super::*;
use crate::{api::MediaDomain, db::custom_formats::*};
use serde_json::json;

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn custom_format_upgrade_rollback_domains_bounds_and_reopen() -> Result<(), Error> {
    let files =
        Sandbox(std::env::temp_dir().join(format!("hrrdarr-formats-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(33).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO quality_profiles(id,media_type,name) VALUES(1,'tv','Shared'),(2,'movies','Shared');
        INSERT INTO quality_profile_items(profile_id,media_type,quality_id,position,allowed) VALUES(1,'tv',1,0,1),(2,'movies',1,0,1);
        INSERT INTO quality_profile_policies VALUES(1,'tv',1,1,NULL,-3,10,2,NULL),(2,'movies',0,1,NULL,-5,20,3,-2);
        INSERT INTO series(id,title,path) VALUES(1,'TV','/tv');
        INSERT INTO seasons(series_id,number) VALUES(1,1);
        INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/renamed.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'Episode',1);
        INSERT INTO movie_metadata(id,title,year) VALUES(1,'Movie',2000);
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies');
        INSERT INTO movie_files(id,movie_id,path,edition) VALUES(1,1,'/movies/renamed.mkv','Extended');
        INSERT INTO file_metadata(media_type,episode_file_id,quality_id,languages_json) VALUES('tv',1,1,NULL);
        INSERT INTO file_metadata(media_type,movie_file_id,quality_id,languages_json) VALUES('movies',1,1,'[]');
        INSERT INTO library_settings(media_type,series_id,quality_profile_id) VALUES('tv',1,1);
        INSERT INTO library_settings(media_type,movie_id,quality_profile_id) VALUES('movies',1,2);").await?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[33].1).await?;
    tx.execute(
        "UPDATE quality_profile_policies SET min_format_score=100",
        (),
    )
    .await?;
    assert!(tx.execute_batch(MIGRATIONS[33].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 33);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM sqlite_schema WHERE name='custom_formats'"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT min_format_score FROM quality_profile_policies WHERE profile_id=1"
        )
        .await?,
        -3
    );
    assert!(
        c.execute(
            "UPDATE quality_profile_items SET allowed=0 WHERE profile_id=1",
            ()
        )
        .await
        .is_err()
    );
    drop(c);
    drop(raw);

    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 47); // Reasoning: latest migration is now 0047 movie credits (was 46: removed metadata health46); historical migration prefixes stay fixed.
    assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 0);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_metadata WHERE original_release_title IS NULL"
        )
        .await?,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM series WHERE original_language IS NULL"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM library_settings WHERE quality_profile_id IN (1,2)"
        )
        .await?,
        2
    );
    assert_eq!(scalar(&c,"SELECT count(*) FROM file_metadata WHERE (media_type='tv' AND languages_json IS NULL) OR (media_type='movies' AND languages_json='[]')").await?,2);
    assert_eq!(
        scalar(
            &c,
            "SELECT min_format_score FROM quality_profile_policies WHERE profile_id=1"
        )
        .await?,
        -3
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT language_id FROM quality_profile_policies WHERE profile_id=2"
        )
        .await?,
        -2
    );
    let specs = json!([{"name":"English","negate":false,"required":true,"condition":{"kind":"language","value":1,"except_language":false}}]).to_string();
    for (id, media) in [(1, "tv"), (2, "movies")] {
        c.execute("INSERT INTO custom_formats(id,media_type,name,include_when_renaming,specifications_json) VALUES(?,?,'Shared',1,?)",params![id,media,specs.clone()]).await?;
        c.execute(
            "INSERT INTO quality_profile_format_scores VALUES(?,?,?,?)",
            params![id, id, media, if id == 1 { i32::MIN } else { i32::MAX }],
        )
        .await?;
        // Same source numeric ID maps separately by application/fingerprint, without a new mapping system.
        let app = if id == 1 { "sonarr" } else { "radarr" };
        c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES(?,'fixture',?)",params![app,if id==1{233}else{242}]).await?;
        c.execute(
            "INSERT INTO snapshot_mappings VALUES(?,'fixture','custom_formats',7,?)",
            params![app, id],
        )
        .await?;
    }
    for sql in [
        "INSERT INTO quality_profile_format_scores VALUES(1,2,'tv',0)",
        "INSERT INTO quality_profile_format_scores VALUES(1,2,'movies',0)",
        "INSERT INTO quality_profile_format_scores VALUES(99,1,'tv',0)",
        "UPDATE quality_profile_format_scores SET score=2147483648",
        "UPDATE quality_profile_format_scores SET score=-2147483649",
        "UPDATE quality_profile_format_scores SET score=1.5",
        "UPDATE custom_formats SET include_when_renaming=2",
        "UPDATE custom_formats SET specification_version=2",
        "UPDATE custom_formats SET specifications_json='[]'",
        "UPDATE custom_formats SET specifications_json='{}'",
        "UPDATE custom_formats SET specifications_json='invalid'",
        "UPDATE custom_formats SET name='  '",
        "UPDATE custom_formats SET media_type='movies' WHERE id=1",
        "UPDATE series SET original_language=53",
        "UPDATE series SET original_language=-2",
        "UPDATE file_metadata SET original_release_title=''",
        "UPDATE file_metadata SET original_release_title=char(0)",
        "UPDATE quality_profile_items SET allowed=0 WHERE profile_id=1",
        "UPDATE quality_profile_policies SET cutoff_quality_id=7 WHERE profile_id=1",
        "UPDATE quality_profile_policies SET min_format_score=2147483648",
    ] {
        assert!(c.execute(sql, ()).await.is_err(), "{sql}");
    }
    for bad in [
        json!([]).to_string(),
        serde_json::to_string(&vec![json!({}); 65])?,
        format!("[\"{}\"]", "x".repeat(65_536)),
    ] {
        assert!(
            c.execute("UPDATE custom_formats SET specifications_json=?", [bad])
                .await
                .is_err()
        );
    }
    c.execute(
        "UPDATE quality_profile_policies SET min_format_score=2147483647 WHERE profile_id=1",
        (),
    )
    .await?;
    c.execute("UPDATE series SET original_language=52", ())
        .await?;
    c.execute(
        "UPDATE file_metadata SET original_release_title='Original.Scene.Title'",
        (),
    )
    .await?;
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM custom_formats").await?, 2);
    assert_eq!(scalar(&c,"SELECT count(*) FROM snapshot_mappings WHERE source_id=7 AND destination_table='custom_formats'").await?,2);
    assert_eq!(
        scalar(
            &c,
            "SELECT score FROM quality_profile_format_scores WHERE profile_id=1"
        )
        .await?,
        i64::from(i32::MIN)
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM file_metadata WHERE original_release_title='Original.Scene.Title'"
        )
        .await?,
        2
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE id=1 AND episode_file_id=1"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM movie_files WHERE id=1 AND movie_id=1 AND edition='Extended'"
        )
        .await?,
        1
    );
    c.execute("DELETE FROM custom_formats WHERE id=1", ())
        .await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profile_format_scores WHERE profile_id=1"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM quality_profile_format_scores WHERE profile_id=2"
        )
        .await?,
        1
    );
    c.execute("DELETE FROM library_settings WHERE media_type='movies'", ())
        .await?;
    c.execute("DELETE FROM quality_profiles WHERE id=2", ())
        .await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM quality_profile_format_scores").await?,
        0
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM custom_formats WHERE id=2").await?,
        1
    );
    integrity(&c).await?;
    Ok(())
}

#[test]
fn custom_format_specification_storage_contract() {
    fn spec(condition: impl serde::Serialize) -> String {
        json!([{"name":"Condition","required":true,"negate":true,"condition":condition}])
            .to_string()
    }
    let shared = [
        json!({"kind":"release_title","pattern":"English"}),
        json!({"kind":"release_group","pattern":"GROUP"}),
        json!({"kind":"language","value":-2,"except_language":true}),
        json!({"kind":"size","min_gib":0.5,"max_gib":2.0}),
        json!({"kind":"source","value":7}),
        json!({"kind":"resolution","value":2160}),
        json!({"kind":"indexer_flag","value":8}),
    ];
    for media in [MediaDomain::Tv, MediaDomain::Movies] {
        for condition in &shared {
            let json = spec(condition);
            let decoded = decode_specifications(media, 1, &json).unwrap();
            assert!(decoded[0].required && decoded[0].negate);
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::from_str::<serde_json::Value>(&json).unwrap()
            );
        }
        for bad in [
            "[]".to_owned(),
            spec(json!({"kind":"unknown"})),
            spec(json!({"kind":"size","min_gib":2.0,"max_gib":1.0})),
            spec(json!({"kind":"language","value":58,"except_language":false})),
            spec(json!({"kind":"source","value":10})),
            spec(json!({"kind":"indexer_flag","value":3})),
            spec(json!({"kind":"release_title","pattern":"x","extra":true})),
            serde_json::to_string(&vec![json!({}); 65]).unwrap(),
        ] {
            assert!(decode_specifications(media, 1, &bad).is_err(), "{bad}");
        }
        assert!(decode_specifications(media, 2, &spec(&shared[0])).is_err());
        assert!(decode_specifications(media, 1, &" ".repeat(MAX_SPECIFICATION_BYTES + 1)).is_err());
    }
    for condition in [
        json!({"kind":"edition","pattern":"Extended"}),
        json!({"kind":"year","min":1999,"max":2000}),
        json!({"kind":"quality_modifier","value":5}),
        json!({"kind":"language","value":-1,"except_language":false}),
        json!({"kind":"source","value":9}),
        json!({"kind":"indexer_flag","value":2048}),
    ] {
        assert!(decode_specifications(MediaDomain::Movies, 1, &spec(&condition)).is_ok());
        assert!(decode_specifications(MediaDomain::Tv, 1, &spec(&condition)).is_err());
    }
    let release_type = spec(json!({"kind":"release_type","value":3}));
    assert!(decode_specifications(MediaDomain::Tv, 1, &release_type).is_ok());
    assert!(decode_specifications(MediaDomain::Movies, 1, &release_type).is_err());
}
