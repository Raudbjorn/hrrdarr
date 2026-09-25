use super::*;

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
const SELECT: &str = "SELECT h.operation_id,h.media_type,h.episode_id,h.movie_id,h.episode_file_id,h.movie_file_id,h.source,h.destination,h.size,h.sha256,h.imported_at FROM import_history h";
const ORDER: &str = "ORDER BY h.imported_at DESC,h.operation_id DESC LIMIT 100 OFFSET 0";

async fn rows(conn: &Connection) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut rows = conn.query(&format!("{SELECT} {ORDER}"), ()).await?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn plan(conn: &Connection, predicate: &str) -> Result<String, Error> {
    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {SELECT} {predicate} {ORDER}"),
            (),
        )
        .await?;
    let mut result = String::new();
    while let Some(row) = rows.next().await? {
        result.push_str(&row.get::<String>(3)?);
        result.push('\n');
    }
    Ok(result)
}

#[tokio::test]
async fn history_order_upgrade_rollback_preserves_facts_and_uses_index() -> Result<(), Error> {
    let files = Sandbox(
        std::env::temp_dir().join(format!("hrrdarr-history-schema-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let path = files.0.join("library.db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let conn = raw.connect()?;
    conn.execute("PRAGMA foreign_keys=ON", ()).await?;
    conn.execute(HISTORY_SQL, ()).await?;
    // Construct the real schema-16 predecessor, not a simplified substitute schema.
    for (index, (name, sql)) in MIGRATIONS.iter().take(16).enumerate() {
        conn.execute_batch(sql).await?;
        conn.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    conn.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'TV','/tv');
        INSERT INTO seasons(series_id,number) VALUES(1,1);
        INSERT INTO episode_files(id,series_id,path) VALUES(1,1,'/tv/one.mkv'),(2,1,'/tv/two.mkv');
        INSERT INTO episodes(id,series_id,season,number,title,episode_file_id) VALUES(1,1,1,1,'One',1),(2,1,1,2,'Two',2);
        INSERT INTO movie_metadata(id,title) VALUES(1,'Movie');
        INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies');
        INSERT INTO movie_files(id,movie_id,path) VALUES(1,1,'/movies/one.mkv');").await?;
    for (n, media, episode, movie, file_episode, file_movie, destination, time) in [
        (
            1,
            "episode",
            Some(1),
            None,
            Some(1),
            None,
            "/tv/one.mkv",
            "2026-09-25 01:00:01",
        ),
        (
            2,
            "episode",
            Some(2),
            None,
            Some(2),
            None,
            "/tv/two.mkv",
            "2026-09-25 01:00:02",
        ),
        (
            3,
            "movie",
            None,
            Some(1),
            None,
            Some(1),
            "/movies/one.mkv",
            "2026-09-25 01:00:02",
        ),
    ] {
        let id = uuid::Uuid::from_u128(n).to_string();
        conn.execute("INSERT INTO operations(id,media_type,episode_id,movie_id,source,mode,destination,status,message) VALUES(?,?,?,?,?,'copy',?,'committed','')",params![id.clone(),media,episode,movie,"/download/source",destination]).await?;
        conn.execute(
            "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
            [id.clone()],
        )
        .await?;
        conn.execute("INSERT INTO import_history(operation_id,media_type,episode_id,movie_id,episode_file_id,movie_file_id,source,destination,size,sha256,imported_at) VALUES(?,?,?,?,?,?,?,?,42,?,?)",params![id.clone(),media,episode,movie,file_episode,file_movie,"/download/source",destination,"a".repeat(64),time]).await?;
        conn.execute(
            "UPDATE import_journal SET phase='committed',stage_json='{}' WHERE operation_id=?",
            [id],
        )
        .await?;
    }
    let before = rows(&conn).await?;
    assert_eq!(before.len(), 3);
    let without_index = plan(&conn, "").await?;
    assert!(
        without_index.contains("TEMP B-TREE"),
        "expected default mixed-domain sorting without index: {without_index}"
    );
    let tx = conn.transaction().await?;
    tx.execute_batch(MIGRATIONS[16].1).await?;
    // A failed step rolls back the index too; the original events and receipts remain intact.
    assert!(tx.execute_batch(MIGRATIONS[16].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&conn).await?, 16);
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='import_history_order'"
        )
        .await?,
        0
    );
    assert_eq!(rows(&conn).await?, before);
    drop(conn);
    drop(raw);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let conn = db.connect().await?;
    assert_eq!(version(&conn).await?, 17);
    assert_eq!(rows(&conn).await?, before);
    for predicate in [
        "",
        "WHERE h.imported_at>='2026-09-25 01:00:01.5' AND h.imported_at<'2026-09-25 01:00:03'",
    ] {
        let indexed = plan(&conn, predicate).await?;
        assert!(
            indexed.contains("USING INDEX import_history_order"),
            "expected native index plan: {indexed}"
        );
        assert!(
            !indexed.contains("TEMP B-TREE"),
            "unexpected sort: {indexed}"
        );
        if !predicate.is_empty() {
            assert!(
                indexed.contains("SEARCH h"),
                "date range should search the index: {indexed}"
            );
        }
    }
    assert_eq!(scalar(&conn,"SELECT count(*) FROM import_history WHERE imported_at>='2026-09-25 01:00:01.5' AND imported_at<'2026-09-25 01:00:03'").await?,2);
    assert_eq!(scalar(&conn,"SELECT count(*) FROM import_history WHERE imported_at>='2026-09-25 01:00:02' AND imported_at<'2026-09-25 01:00:02.5'").await?,2);
    assert_eq!(scalar(&conn,"SELECT count(*) FROM sqlite_schema WHERE name IN ('import_history_episode','import_history_movie')").await?,2);
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM import_journal WHERE phase='committed'"
        )
        .await?,
        3
    );
    integrity(&conn).await?;
    drop(conn);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let conn = db.connect().await?;
    assert_eq!(rows(&conn).await?, before);
    assert!(
        plan(&conn, "")
            .await?
            .contains("USING INDEX import_history_order")
    );
    Ok(())
}
