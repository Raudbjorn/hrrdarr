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
    assert_eq!(version(&conn).await?, 28); // Latest open adds same-path exchange authority; fixed predecessors remain unchanged.
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

#[tokio::test]
async fn mixed_history_page_uses_covering_candidate_indices() -> Result<(), Error> {
    let files = Sandbox(
        std::env::temp_dir().join(format!("hrrdarr-history-plan-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let db = Database::open_local(files.0.join("db")).await?;
    let conn = db.connect().await?;
    for predicate in [
        "",
        " WHERE h.imported_at>='2026-09-25 01:00:00.5' AND h.imported_at<'2026-09-26 00:00:00'",
    ] {
        let mut rows = conn
            .query(
                &format!("EXPLAIN QUERY PLAN {}", crate::history::page_sql(predicate)),
                params![100, 100, 100, 0],
            )
            .await?;
        let mut details = String::new();
        while let Some(row) = rows.next().await? {
            details.push_str(&row.get::<String>(3)?);
            details.push('\n');
        }
        for expected in [
            "USING COVERING INDEX import_history_order",
            "USING COVERING INDEX snapshot_history_order",
            "SEARCH n USING INDEX",
            "SEARCH e USING INDEX",
        ] {
            assert!(details.contains(expected), "{details}");
        }
        // Temporary sorting is allowed only after both branch LIMITs; wide facts are keyed joins.
        assert!(
            details.contains("CO-ROUTINE native_keys")
                && details.contains("CO-ROUTINE source_keys"),
            "{details}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn source_history_predecessor_backfill_rollback_and_reopen() -> Result<(), Error> {
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let files = Sandbox(
        std::env::temp_dir().join(format!("hrrdarr-history-upgrade-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&files.0)?;
    let source_path = files.0.join("source.db");
    let source = libsql::Builder::new_local(&source_path).build().await?;
    let sc = source.connect()?;
    sc.execute_batch("CREATE TABLE VersionInfo(Version INTEGER); INSERT INTO VersionInfo VALUES(233);
CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(7,777,'TV',2020,'/tv/TV',1,'[{\"seasonNumber\":1,\"monitored\":true}]');
CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,7,1,1,'One',1,0);
CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);
CREATE TABLE History(Id INTEGER,EpisodeId INTEGER,SeriesId INTEGER,Date TEXT,EventType INTEGER,Languages TEXT);INSERT INTO History VALUES(1,1,7,'2026-09-25 01:00:00.000000000',6,'[null,1]');").await?;
    drop(sc);
    drop(source);
    let bytes = std::fs::read(source_path)?;
    let movie_path = files.0.join("movie.db");
    let movie = libsql::Builder::new_local(&movie_path).build().await?;
    let mc = movie.connect()?;
    mc.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);
CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,2001,NULL,'Film',2000,'/movies/Film',1,0);
CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);
CREATE TABLE History(Id INTEGER,MovieId INTEGER,Date TEXT,EventType INTEGER);INSERT INTO History VALUES(1,1,'2026-09-25 01:00:00',6);").await?;
    drop(mc);
    drop(movie);
    let movie_bytes = std::fs::read(movie_path)?;
    let path = files.0.join("dest.db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let conn = raw.connect()?;
    conn.execute("PRAGMA foreign_keys=ON", ()).await?;
    conn.execute(HISTORY_SQL, ()).await?;
    // Construct all seventeen real predecessor migrations with their recorded checksums.
    for (i, (name, sql)) in MIGRATIONS.iter().take(17).enumerate() {
        conn.execute_batch(sql).await?;
        conn.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let tx = conn.transaction().await?;
    crate::snapshots::write_core_snapshot_fixture(
        &tx,
        crate::snapshots::Application::Sonarr,
        bytes.clone(),
    )
    .await?;
    crate::snapshots::write_core_snapshot_fixture(
        &tx,
        crate::snapshots::Application::Radarr,
        movie_bytes.clone(),
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM snapshot_records WHERE source_table='History'"
        )
        .await?,
        2
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM episodes").await?, 1);
    let tx = conn.transaction().await?;
    tx.execute_batch(MIGRATIONS[17].1).await?;
    assert!(tx.execute_batch(MIGRATIONS[17].1).await.is_err());
    tx.rollback().await?;
    assert_eq!(version(&conn).await?, 17);
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE name='snapshot_history_events'"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM snapshot_records WHERE source_table='History'"
        )
        .await?,
        2
    );
    drop(conn);
    drop(raw);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_some());
    let conn = db.connect().await?;
    assert_eq!(version(&conn).await?, 28); // Latest open adds same-path exchange authority; fixed predecessors remain unchanged.
    assert_eq!(
        scalar(&conn, "SELECT history_version FROM snapshot_imports").await?,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM snapshot_history_events").await?,
        0,
        "migration alone cannot validate an uploaded source"
    );
    let report = crate::snapshots::import(
        &db,
        crate::snapshots::Application::Sonarr,
        bytes.clone(),
        false,
    )
    .await?;
    assert!(report.applied);
    assert_eq!(report.mapped, 1);
    let event = conn
        .query(
            "SELECT occurred_at,languages_json FROM snapshot_history_events",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    assert_eq!(event.get::<String>(0)?, "2026-09-25 01:00:00");
    assert_eq!(event.get::<String>(1)?, "[0,1]");
    drop(event);
    let repeat =
        crate::snapshots::import(&db, crate::snapshots::Application::Sonarr, bytes, false).await?;
    assert!(repeat.applied);
    assert_eq!(repeat.mapped, 0);
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM import_history").await?,
        0
    );
    let movie_report = crate::snapshots::import(
        &db,
        crate::snapshots::Application::Radarr,
        movie_bytes.clone(),
        false,
    )
    .await?;
    assert!(movie_report.applied);
    assert_eq!(movie_report.mapped, 1);
    let movie_repeat = crate::snapshots::import(
        &db,
        crate::snapshots::Application::Radarr,
        movie_bytes,
        false,
    )
    .await?;
    assert!(movie_repeat.applied);
    assert_eq!(movie_repeat.mapped, 0);
    assert_eq!(scalar(&conn,"SELECT count(*) FROM snapshot_history_events WHERE (media_type='episode' AND episode_id=1 AND movie_id IS NULL) OR (media_type='movie' AND movie_id=1 AND episode_id IS NULL)").await?,2);
    // Direct writes cannot create a wrong-domain target or reinterpret Sonarr event 6.
    for sql in [
        "INSERT INTO snapshot_history_events SELECT application,fingerprint,2,'movie',episode_id,movie_id,occurred_at,event_type,source_event_type,source_title,download_id,quality_id,quality_revision_json,languages_json FROM snapshot_history_events",
        "INSERT INTO snapshot_history_events SELECT application,fingerprint,2,media_type,episode_id,movie_id,occurred_at,'file_deleted',source_event_type,source_title,download_id,quality_id,quality_revision_json,languages_json FROM snapshot_history_events",
        "INSERT INTO snapshot_history_events SELECT application,fingerprint,2,media_type,99999,movie_id,occurred_at,event_type,source_event_type,source_title,download_id,quality_id,quality_revision_json,languages_json FROM snapshot_history_events",
    ] {
        assert!(conn.execute(sql, ()).await.is_err());
    }
    integrity(&conn).await?;
    drop(conn);
    drop(db);
    let db = Database::open_local(&path).await?;
    assert!(db.migration_backup().is_none());
    let conn = db.connect().await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM snapshot_history_events").await?,
        2
    );
    assert_eq!(
        scalar(&conn, "SELECT history_version FROM snapshot_imports").await?,
        1
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM episodes").await?, 1);
    integrity(&conn).await?;
    Ok(())
}
