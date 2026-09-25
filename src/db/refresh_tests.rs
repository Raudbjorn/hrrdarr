use super::*;

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-refresh-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("library.db")
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

pub(super) async fn provider(conn: &Connection) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://fixture.invalid')",[id.clone()]).await?;
    for media in ["tv", "movies"] {
        conn.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![id.clone(),media,media]).await?;
    }
    Ok(id)
}
pub(super) async fn enqueue(
    conn: &Connection,
    provider: &str,
    media: &str,
) -> Result<String, libsql::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute("INSERT INTO commands(id,provider_id,media_type,provider_revision,next_attempt_at,created_at) VALUES(?,?,?,1,100,100)",params![id.clone(),provider,media]).await?;
    Ok(id)
}
pub(super) async fn succeed(conn: &Connection, id: &str, count: i64) -> Result<(), libsql::Error> {
    conn.execute(
        "UPDATE commands SET status='running',attempts=1,started_at=100 WHERE id=?",
        [id],
    )
    .await?;
    conn.execute(
        "UPDATE commands SET status='succeeded',completed_at=101,items_observed=? WHERE id=?",
        params![count, id],
    )
    .await?;
    Ok(())
}
pub(super) async fn snapshot(
    conn: &Connection,
    provider: &str,
    media: &str,
    command: &str,
    items: &str,
) -> Result<(), libsql::Error> {
    conn.execute("INSERT INTO download_refresh_snapshots(provider_id,media_type,provider_revision,observed_at,command_id,items_json) VALUES(?,?,1,101,?,?) ON CONFLICT(provider_id,media_type) DO UPDATE SET command_id=excluded.command_id,items_json=excluded.items_json,observed_at=excluded.observed_at",params![provider,media,command,items]).await?;
    Ok(())
}
pub(super) async fn schedule(
    conn: &Connection,
    provider: &str,
    media: &str,
) -> Result<(), libsql::Error> {
    conn.execute("INSERT INTO download_refresh_schedules(provider_id,media_type,provider_revision,enabled,interval_seconds,next_run_at) VALUES(?,?,1,1,60,100) ON CONFLICT(provider_id,media_type) DO UPDATE SET interval_seconds=120,revision=revision+1",params![provider,media]).await?;
    Ok(())
}

#[tokio::test]
async fn download_refresh_upgrade_rollback_and_reopen_preserve_prior_data() -> Result<(), Error> {
    let files = Sandbox::new();
    let raw = libsql::Builder::new_local(files.path()).build().await?;
    let conn = raw.connect()?;
    conn.execute("PRAGMA foreign_keys=ON", ()).await?;
    conn.execute(HISTORY_SQL, ()).await?;
    // Establish the actual predecessor with the same checksums used by the production runner.
    for (index, (name, sql)) in MIGRATIONS.iter().take(15).enumerate() {
        conn.execute_batch(sql).await?;
        conn.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![index as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    let provider_id = provider(&conn).await?;
    conn.execute(
        "INSERT INTO series(id,title,path) VALUES(1,'Preserved','/tv/Preserved')",
        (),
    )
    .await?;
    let before = version(&conn).await?;
    assert_eq!(before, 15);
    let tx = conn.transaction().await?;
    tx.execute_batch(MIGRATIONS[15].1).await?;
    let command = enqueue(&tx, &provider_id, "tv").await?;
    assert!(
        tx.execute(
            "UPDATE commands SET status='succeeded' WHERE id=?",
            [command]
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(scalar(&conn,"SELECT count(*) FROM sqlite_schema WHERE name IN ('commands','download_refresh_schedules','download_refresh_snapshots','refresh_provider_changed')").await?,0);
    assert_eq!(version(&conn).await?, 15);
    drop(conn);
    drop(raw);
    let db = Database::open_local(files.path()).await?;
    assert!(db.migration_backup().is_some());
    let conn = db.connect().await?;
    // Opening the predecessor now also applies the History ordering index.
    assert_eq!(version(&conn).await?, 27); // Latest open adds targeted search authority; fixed predecessors remain unchanged.
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM series WHERE title='Preserved'").await?,
        1
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM providers").await?, 1);
    let tv = enqueue(&conn, &provider_id, "tv").await?;
    let movie = enqueue(&conn, &provider_id, "movies").await?;
    succeed(&conn, &tv, 0).await?;
    snapshot(&conn, &provider_id, "tv", &tv, "[]").await?;
    schedule(&conn, &provider_id, "movies").await?;
    drop(conn);
    drop(db);
    let db = Database::open_local(files.path()).await?;
    assert!(db.migration_backup().is_none());
    let conn = db.connect().await?;
    assert_eq!(scalar(&conn, "SELECT count(*) FROM commands").await?, 2);
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM download_refresh_snapshots").await?,
        1
    );
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM download_refresh_schedules").await?,
        1
    );
    assert_eq!(
        conn.query("SELECT media_type,status FROM commands WHERE id=?", [movie])
            .await?
            .next()
            .await?
            .unwrap()
            .get::<String>(0)?,
        "movies"
    );
    integrity(&conn).await?;
    Ok(())
}

#[tokio::test]
async fn download_refresh_state_and_observation_constraints() -> Result<(), Error> {
    let files = Sandbox::new();
    let db = Database::open_local(files.path()).await?;
    let conn = db.connect().await?;
    let provider_id = provider(&conn).await?;
    assert!(enqueue(&conn, &provider_id, "episode").await.is_err());
    assert!(
        enqueue(&conn, &uuid::Uuid::new_v4().to_string(), "tv")
            .await
            .is_err()
    );
    let tv = enqueue(&conn, &provider_id, "tv").await?;
    assert!(enqueue(&conn, &provider_id, "tv").await.is_err());
    let movie = enqueue(&conn, &provider_id, "movies").await?;
    for set in [
        "provider_revision=2",
        "media_type='movies'",
        "attempts=1",
        "status='running',started_at=100",
        "status='failed',completed_at=101",
        "status='cancelled'",
        "items_observed=1",
    ] {
        assert!(
            conn.execute(
                &format!("UPDATE commands SET {set} WHERE id=?"),
                [tv.clone()]
            )
            .await
            .is_err(),
            "accepted {set}"
        );
    }
    assert!(
        conn.execute("DELETE FROM commands WHERE id=?", [tv.clone()])
            .await
            .is_err()
    );
    for attempt in 1..=3 {
        conn.execute("UPDATE commands SET status='running',attempts=?,started_at=100,error_code=NULL WHERE id=?",params![attempt,tv.clone()]).await?;
        if attempt < 3 {
            conn.execute("UPDATE commands SET status='retry_wait',error_code='interrupted',next_attempt_at=101 WHERE id=?",[tv.clone()]).await?;
        }
    }
    assert!(
        conn.execute(
            "UPDATE commands SET status='retry_wait',error_code='interrupted' WHERE id=?",
            [tv.clone()]
        )
        .await
        .is_err()
    );
    conn.execute(
        "UPDATE commands SET status='failed',error_code='interrupted',completed_at=101 WHERE id=?",
        [tv.clone()],
    )
    .await?;
    assert!(conn.execute("UPDATE commands SET status='queued',attempts=0,started_at=NULL,completed_at=NULL,error_code=NULL WHERE id=?",[tv.clone()]).await.is_err());
    let tv2 = enqueue(&conn, &provider_id, "tv").await?;
    succeed(&conn, &tv2, 1).await?;
    succeed(&conn, &movie, 1).await?;
    let tv_json = format!(r#"[{{"domain":"tv","hash":"{}"}}]"#, "a".repeat(40));
    let movie_json = tv_json.replace("tv", "movies");
    // SQL validates identity/provenance; closed full DownloadItem validation belongs to the writer.
    assert!(
        snapshot(&conn, &provider_id, "movies", &movie, &tv_json)
            .await
            .is_err()
    );
    assert!(
        snapshot(&conn, &provider_id, "tv", &movie, &tv_json)
            .await
            .is_err()
    );
    assert!(
        snapshot(&conn, &provider_id, "tv", &tv2, "[]")
            .await
            .is_err()
    );
    assert!(
        snapshot(
            &conn,
            &provider_id,
            "tv",
            &tv2,
            &tv_json.replace(&"a".repeat(40), "bad")
        )
        .await
        .is_err()
    );
    snapshot(&conn, &provider_id, "tv", &tv2, &tv_json).await?;
    snapshot(&conn, &provider_id, "movies", &movie, &movie_json).await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM download_refresh_snapshots").await?,
        2
    );
    conn.execute("DELETE FROM commands WHERE id=?", [tv2])
        .await?;
    assert_eq!(
        scalar(
            &conn,
            "SELECT count(*) FROM download_refresh_snapshots WHERE command_id IS NULL"
        )
        .await?,
        1
    );
    schedule(&conn, &provider_id, "tv").await?;
    schedule(&conn, &provider_id, "movies").await?;
    let stale = enqueue(&conn, &provider_id, "tv").await?;
    conn.execute(
        "UPDATE providers SET name='Edited',revision=2 WHERE id=?",
        [provider_id.clone()],
    )
    .await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM download_refresh_snapshots").await?,
        0
    );
    assert_eq!(scalar(&conn,"SELECT count(*) FROM download_refresh_schedules WHERE enabled=0 AND error_code='provider_changed' AND provider_revision=1").await?,2);
    assert!(
        conn.execute(
            "UPDATE commands SET status='running',attempts=1,started_at=100 WHERE id=?",
            [stale.clone()]
        )
        .await
        .is_err()
    );
    conn.execute("UPDATE commands SET status='failed',error_code='provider_changed',completed_at=101 WHERE id=?",[stale]).await?;
    conn.execute("DELETE FROM providers WHERE id=?", [provider_id])
        .await?;
    assert_eq!(
        scalar(&conn, "SELECT count(*) FROM download_refresh_schedules").await?,
        0
    );
    assert_eq!(scalar(&conn, "SELECT count(*) FROM commands").await?, 3);
    integrity(&conn).await?;
    Ok(())
}

#[tokio::test]
async fn download_refresh_storage_caps_allow_explicit_retention_and_replacement()
-> Result<(), Error> {
    let files = Sandbox::new();
    let db = Database::open_local(files.path()).await?;
    let conn = db.connect().await?;
    let tx = conn.transaction().await?;
    let mut first = None;
    for _ in 0..32 {
        let provider_id = provider(&tx).await?;
        for media in ["tv", "movies"] {
            let id = enqueue(&tx, &provider_id, media).await?;
            succeed(&tx, &id, 0).await?;
            snapshot(&tx, &provider_id, media, &id, "[]").await?;
            schedule(&tx, &provider_id, media).await?;
            first.get_or_insert((provider_id.clone(), id));
        }
    }
    let (first_provider, first_command) = first.unwrap();
    snapshot(&tx, &first_provider, "tv", &first_command, "[]").await?;
    schedule(&tx, &first_provider, "tv").await?;
    tx.execute("UPDATE download_refresh_schedules SET error_code='command_history_full',revision=revision+1 WHERE provider_id=? AND media_type='tv'", [first_provider.clone()]).await?;
    assert_eq!(scalar(&tx,"SELECT count(*) FROM download_refresh_schedules WHERE enabled=1 AND error_code='command_history_full'").await?,1);
    assert!(tx.execute("UPDATE download_refresh_schedules SET error_code='provider_changed' WHERE provider_id=? AND media_type='tv'", [first_provider.clone()]).await.is_err());
    let extra = provider(&tx).await?;
    let extra_command = enqueue(&tx, &extra, "tv").await?;
    succeed(&tx, &extra_command, 0).await?;
    assert!(
        snapshot(&tx, &extra, "tv", &extra_command, "[]")
            .await
            .is_err()
    );
    assert!(schedule(&tx, &extra, "tv").await.is_err());
    assert_eq!(
        scalar(&tx, "SELECT count(*) FROM download_refresh_snapshots").await?,
        64
    );
    assert_eq!(
        scalar(&tx, "SELECT count(*) FROM download_refresh_schedules").await?,
        64
    );
    for _ in 65..1024 {
        let id = enqueue(&tx, &extra, "tv").await?;
        tx.execute(
            "UPDATE commands SET status='cancelled',completed_at=101 WHERE id=?",
            [id],
        )
        .await?;
    }
    assert_eq!(scalar(&tx, "SELECT count(*) FROM commands").await?, 1024);
    assert!(enqueue(&tx, &extra, "tv").await.is_err());
    tx.execute("DELETE FROM commands WHERE id=?", [extra_command])
        .await?;
    enqueue(&tx, &extra, "tv").await?;
    tx.commit().await?;
    integrity(&conn).await?;
    Ok(())
}
