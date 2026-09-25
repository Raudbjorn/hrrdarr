use super::*;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[tokio::test]
async fn same_path_schema27_upgrade_rollback_exchange_and_commit_guards() -> Result<(), Error> {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-same-path-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0)?;
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(27).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    c.execute_batch("INSERT INTO series(id,tvdb_id,title,path)VALUES(1,101,'TV','/tv'),(2,102,'Other','/other');INSERT INTO seasons VALUES(1,1,1);INSERT INTO episode_files(id,series_id,path)VALUES(1,1,'/tv/old.mkv');INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(1,1,1,1,'One',1),(2,1,1,2,'Two',1);INSERT INTO movie_metadata(id,tmdb_id,title)VALUES(1,101,'Movie');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');INSERT INTO movie_files(id,movie_id,path)VALUES(1,1,'/movies/old.mkv');").await?;
    let client = super::refresh_tests::provider(&c).await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint)VALUES(?,'torznab','Indexer',1,1,1,1,'http://127.0.0.1:1/')",[indexer.clone()]).await?;
    for domain in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year)VALUES(?,'torznab',?,'[5000]','[]',?,?)",params![indexer.clone(),domain,(domain=="tv").then_some(0),(domain=="movies").then_some(0)]).await?;
    }
    let mut operations = Vec::new();
    for domain in ["tv", "movies"] {
        let cmd = super::rss_tests::command(&c, &indexer, &client, domain).await?;
        let receipt = super::processing_tests::observed(
            &c,
            &cmd,
            &client,
            domain,
            &if domain == "tv" { "a" } else { "b" }.repeat(40),
        )
        .await?;
        let op = uuid::Uuid::new_v4().to_string();
        let destination = format!("/{domain}/old.mkv");
        let plan = serde_json::json!({"destination":destination});
        c.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message)VALUES(?,?,?,?,?,'copy',?,'preview','fixture')",params![op.clone(),if domain=="tv"{"episode"}else{"movie"},(domain=="tv").then_some(1),(domain=="movies").then_some(1),format!("/download/{domain}"),destination]).await?;
        c.execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,?,'preview')",
            params![op.clone(), plan.to_string()],
        )
        .await?;
        super::processing_tests::link(&c, &receipt, &op, domain).await?;
        c.execute(
            "UPDATE import_journal SET phase='staged',stage_json=? WHERE operation_id=?",
            params![
                serde_json::json!({"sha256":"c".repeat(64),"directory":{},"file":{}}).to_string(),
                op.clone()
            ],
        )
        .await?;
        operations.push(op);
    }
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATIONS[27].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[27].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&c).await?, 27);
    drop(c);
    drop(raw);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(version(&c).await?, 28);
    for op in &operations {
        let r = c
            .query(
                "SELECT version,phase FROM import_journal WHERE operation_id=?",
                [op.clone()],
            )
            .await?
            .next()
            .await?
            .unwrap();
        assert_eq!(r.get::<i64>(0)?, 1);
        assert_eq!(r.get::<String>(1)?, "staged");
    }
    assert!(
        c.execute(
            "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
            [operations[0].clone()]
        )
        .await
        .is_err(),
        "shared TV file must never be exchanged"
    );
    c.execute("UPDATE episodes SET episode_file_id=NULL WHERE id=2", ())
        .await?;
    for op in &operations {
        c.execute(
            "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
            [op.clone()],
        )
        .await?;
        assert!(
            c.execute(
                "UPDATE same_path_replacements SET state='restored' WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err()
        );
        c.execute(
            "UPDATE same_path_replacements SET state='installed' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE same_path_replacements SET state='restored' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE same_path_replacements SET state='exchange_intent' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE same_path_replacements SET state='installed' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        c.execute(
            "UPDATE import_journal SET phase='published' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        assert!(
            c.execute(
                "DELETE FROM same_path_replacements WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "UPDATE same_path_replacements SET protocol_version=2 WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err()
        );
    }
    // Even valid typed history cannot authorize commit after a restored exchange.
    let tx = c.transaction().await?;
    let movie = &operations[1];
    tx.execute(
        "UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=?",
        [movie.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE same_path_replacements SET state='restored' WHERE operation_id=?",
        [movie.clone()],
    )
    .await?;
    tx.execute("INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256)VALUES(?,'movie',1,1,'/download/movies','/movies/old.mkv',1,?)", params![movie.clone(), "c".repeat(64)]).await?;
    assert!(
        tx.execute(
            "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
            [movie.clone()]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    // rtrim uses a character set without '/', so repeated/Unicode filename
    // characters stop precisely at the last separator rather than eating the parent.
    for (old, expected) in [
        ("/tv/aaaa/aaaa.mkv", "/tv/aaaa/.recovery/original"),
        ("/tv/Ár/ÁrÁr.mkv", "/tv/Ár/.recovery/original"),
        ("/file.mkv", "/.recovery/original"),
    ] {
        let row = c
            .query(
                "SELECT rtrim(?1,replace(?1,'/',''))||'.recovery/original'",
                [old],
            )
            .await?
            .next()
            .await?
            .unwrap();
        assert_eq!(row.get::<String>(0)?, expected);
    }
    // The installed protocol allows only the exact TV archive path and owning series.
    let tv = &operations[0];
    let recovery = format!("/tv/.hrrdarr-replaced-{tv}/original");
    let tx = c.transaction().await?;
    tx.execute("UPDATE episodes SET episode_file_id=NULL WHERE id=1", ())
        .await?;
    assert!(
        tx.execute("UPDATE episode_files SET path='/wrong' WHERE id=1", ())
            .await
            .is_err()
    );
    tx.execute(
        "UPDATE episode_files SET path=? WHERE id=1",
        [recovery.clone()],
    )
    .await?;
    assert!(
        tx.execute(
            "INSERT INTO episode_files(series_id,path)VALUES(2,'/tv/old.mkv')",
            ()
        )
        .await
        .is_err()
    );
    assert!(
        tx.execute("UPDATE movie_files SET path='/tv/old.mkv' WHERE id=1", ())
            .await
            .is_err()
    );
    tx.execute(
        "INSERT INTO episode_files(id,series_id,path)VALUES(2,1,'/tv/old.mkv')",
        (),
    )
    .await?;
    tx.execute("UPDATE episodes SET episode_file_id=2 WHERE id=1", ())
        .await?;
    // A late failure rolls relocation and association back; intent remains installed for FS restoration.
    assert!(
        tx.execute(
            "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(
        c.query("SELECT path FROM episode_files WHERE id=1", ())
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "/tv/old.mkv"
    );
    for (n, domain) in ["tv", "movies"].iter().enumerate() {
        let op = &operations[n];
        let tx = c.transaction().await?;
        if n == 0 {
            tx.execute("UPDATE episodes SET episode_file_id=NULL WHERE id=1", ())
                .await?;
            tx.execute(
                "UPDATE episode_files SET path=? WHERE id=1",
                [recovery.clone()],
            )
            .await?;
            tx.execute(
                "INSERT INTO episode_files(id,series_id,path)VALUES(2,1,'/tv/old.mkv')",
                (),
            )
            .await?;
            tx.execute("UPDATE episodes SET episode_file_id=2 WHERE id=1", ())
                .await?;
        }
        tx.execute("INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256)VALUES(?,?,?,?,?,?,?,?,1,?)",params![op.clone(),if n==0{"episode"}else{"movie"},(n==0).then_some(1),(n==1).then_some(1),(n==0).then_some(2),(n==1).then_some(1),format!("/download/{domain}"),format!("/{domain}/old.mkv"),"c".repeat(64)]).await?;
        assert!(
            tx.execute(
                "UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err(),
            "history prevents ambiguous postcommit restoration"
        );
        tx.execute(
            "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
            [op.clone()],
        )
        .await?;
        tx.commit().await?;
        assert!(
            c.execute(
                "UPDATE same_path_replacements SET state='restore_intent' WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err()
        );
        assert!(
            c.execute(
                "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
                [op.clone()]
            )
            .await
            .is_err(),
            "retirement remains mandatory"
        );
    }
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        c.query(
            "SELECT count(*) FROM same_path_replacements WHERE state='installed'",
            ()
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        2
    );
    integrity(&c).await?;
    Ok(())
}
