use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hrrdarr-indexer-client-schema-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn rows(c: &Connection, table: &str) -> Result<Vec<Vec<libsql::Value>>, Error> {
    let mut cursor = c
        .query(&format!("SELECT * FROM {table} ORDER BY rowid"), ())
        .await?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().await? {
        result.push(
            (0..row.column_count())
                .map(|i| row.get_value(i))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}
async fn count(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
async fn indexer(c: &Connection, implementation: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,'owned fixture',1,1,1,1,'http://127.0.0.1:9')",params![id.clone(),implementation]).await?;
    for media in ["tv", "movies"] {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,?,?,'[5000]','[]',?,?)",params![id.clone(),implementation,media,(media=="tv").then_some(0),(media=="movies").then_some(0)]).await?;
    }
    Ok(id)
}
async fn binding(c: &Connection, id: &str, media: &str) -> Result<Option<String>, Error> {
    Ok(c.query(
        "SELECT download_client_id FROM provider_scopes WHERE provider_id=? AND media_type=?",
        params![id, media],
    )
    .await?
    .next()
    .await?
    .unwrap()
    .get(0)?)
}

#[tokio::test]
async fn indexer_client_upgrade48_rollback_defaults_and_reopen_preserve_history()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let raw = libsql::Builder::new_local(&path).build().await?;
    let c = raw.connect()?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    c.execute(HISTORY_SQL, ()).await?;
    for (i, (name, sql)) in MIGRATIONS.iter().take(48).enumerate() {
        c.execute_batch(sql).await?;
        c.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![i as i64 + 1, *name, checksum(sql), *sql],
        )
        .await?;
    }
    indexer(&c, "torznab").await?;
    indexer(&c, "newznab").await?;
    super::refresh_tests::provider(&c).await?;
    let tables = [
        "providers",
        "provider_scopes",
        "health_checks",
        "health_lifecycle",
        "schema_migrations",
    ];
    let mut before = Vec::new();
    for table in tables {
        before.push(rows(&c, table).await?);
    }
    let schema = rows(&c, "sqlite_schema").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATIONS[48].1).await?;
    // Reasoning: the schema48 predecessor registry holds 12 rows; 0049 adds the two indexer_download_client checks.
    assert_eq!(count(&tx, "SELECT count(*) FROM health_checks").await?, 14);
    // Force failure after ALTER and both seeds, then prove DDL/data/history rollback together.
    assert!(
        tx.execute(
            "UPDATE provider_scopes SET download_client_id='not-a-uuid'",
            ()
        )
        .await
        .is_err()
    );
    tx.rollback().await?;
    assert_eq!(rows(&c, "sqlite_schema").await?, schema);
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(&rows(&c, table).await?, expected);
    }
    drop(c);
    drop(raw);
    for _ in 0..2 {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        assert_eq!(version(&c).await?, 49);
        assert_eq!(rows(&c, "providers").await?, before[0]);
        let mut expected = before[1].clone();
        // Only the appended preference column differs: all old values remain exact and NULL.
        for row in &mut expected {
            row.push(libsql::Value::Null);
        }
        assert_eq!(rows(&c, "provider_scopes").await?, expected);
        let registry = rows(&c, "health_checks").await?;
        assert_eq!(&registry[..12], before[2].as_slice());
        assert_eq!(registry.len(), 14);
        assert_eq!(rows(&c, "health_lifecycle").await?, before[3]);
        let history = rows(&c, "schema_migrations").await?;
        assert_eq!(&history[..48], before[4].as_slice());
        assert_eq!(history.len(), 49);
        assert_eq!(count(&c,"SELECT count(*) FROM health_checks WHERE check_key='indexer_download_client' AND scope IN ('tv','movies') AND startup=1 AND scheduled=1 AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND observed_generation IS NULL AND observed_epoch IS NULL AND checked_at IS NULL AND last_error IS NULL AND severity IS NULL AND reason IS NULL AND message IS NULL AND wiki_url IS NULL AND compatibility_type='IndexerDownloadClientCheck'").await?,2);
        integrity(&c).await?;
    }
    Ok(())
}

#[tokio::test]
async fn indexer_client_constraints_scoped_bindings_and_dangling_targets_survive_reopen()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let path = scratch.0.join("db");
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    let torznab = indexer(&c, "torznab").await?;
    let newznab = indexer(&c, "newznab").await?;
    let client = super::refresh_tests::provider(&c).await?;
    for id in [&torznab, &newznab, &client] {
        for media in ["tv", "movies"] {
            assert_eq!(binding(&c, id, media).await?, None);
        }
    }
    let missing = "abcdefab-1234-5678-9abc-def012345678";
    for bad in [
        "",
        "ABCDEFAB-1234-5678-9abc-def012345678",
        "abcdefab1234-5678-9abc-def012345678",
        "abcdefab-1234-5678-9abc-def01234567g",
        "abcdefab-1234-5678-9abc-def012345678 ",
        "abcdefab-1234-5678-9abc-def012345678\0",
        "abcdefab-1234-5678-9abc-def01234567é",
    ] {
        for id in [&torznab, &newznab] {
            for media in ["tv", "movies"] {
                assert!(c.execute("UPDATE provider_scopes SET download_client_id=? WHERE provider_id=? AND media_type=?",params![bad,id.clone(),media]).await.is_err(),"accepted {bad:?}");
                assert_eq!(binding(&c, id, media).await?, None);
            }
        }
    }
    assert!(
        c.execute(
            "UPDATE provider_scopes SET download_client_id=123 WHERE provider_id=?",
            [torznab.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![missing.as_bytes().to_vec(), torznab.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![missing, client.clone()]
        )
        .await
        .is_err(),
        "qBittorrent cannot carry an indexer preference"
    );
    for id in [&torznab, &newznab] {
        c.execute("UPDATE provider_scopes SET download_client_id=? WHERE provider_id=? AND media_type='tv'",params![client.clone(),id.clone()]).await?;
        c.execute("UPDATE provider_scopes SET download_client_id=? WHERE provider_id=? AND media_type='movies'",params![missing,id.clone()]).await?;
        assert_eq!(
            binding(&c, id, "tv").await?.as_deref(),
            Some(client.as_str())
        );
        assert_eq!(binding(&c, id, "movies").await?.as_deref(), Some(missing));
    }
    let before = rows(&c, "provider_scopes").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(
        "UPDATE provider_scopes SET download_client_id=NULL WHERE provider_id=?",
        [torznab.clone()],
    )
    .await?;
    tx.rollback().await?;
    assert_eq!(rows(&c, "provider_scopes").await?, before);
    // Existing category, owner, identity and revision guards still reject invalid changes.
    assert!(
        c.execute(
            "UPDATE provider_scopes SET categories='[1,1]' WHERE provider_id=?",
            [torznab.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE providers SET name='bad revision' WHERE id=?",
            [client.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE provider_scopes SET provider_id=? WHERE provider_id=?",
            params![missing, newznab.clone()]
        )
        .await
        .is_err()
    );
    c.execute(
        "UPDATE providers SET enabled=0,revision=revision+1 WHERE id=?",
        [client.clone()],
    )
    .await?;
    assert_eq!(
        binding(&c, &torznab, "tv").await?.as_deref(),
        Some(client.as_str())
    );
    c.execute("DELETE FROM providers WHERE id=?", [client.clone()])
        .await?;
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM provider_scopes WHERE implementation='qbittorrent'"
        )
        .await?,
        0
    );
    // Target deletion retains the broken reference; owner deletion still cascades its scopes.
    c.execute("DELETE FROM providers WHERE id=?", [newznab])
        .await?;
    assert_eq!(count(&c, "SELECT count(*) FROM provider_scopes").await?, 2);
    integrity(&c).await?;
    drop(c);
    drop(db);
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    assert_eq!(
        binding(&c, &torznab, "tv").await?.as_deref(),
        Some(client.as_str())
    );
    assert_eq!(
        binding(&c, &torznab, "movies").await?.as_deref(),
        Some(missing)
    );
    integrity(&c).await?;
    Ok(())
}

#[tokio::test]
async fn indexer_client_insert_guards_do_not_clear_existing_scope_on_rejected_replace()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    let client = super::refresh_tests::provider(&c).await?;
    let wanted = "01234567-89ab-cdef-0123-456789abcdef";
    for implementation in ["torznab", "newznab"] {
        let id = indexer(&c, implementation).await?;
        for media in ["tv", "movies"] {
            let statement = "INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year,download_client_id) VALUES(?,?,?,'[5000]','[]',?,?,?)";
            for invalid in [
                "bad",
                "01234567-89AB-cdef-0123-456789abcdef",
                "01234567-89ab-cdef-0123-456789abcdef\0",
            ] {
                let before = rows(&c, "provider_scopes").await?;
                assert!(
                    c.execute(
                        statement,
                        params![
                            id.clone(),
                            implementation,
                            media,
                            (media == "tv").then_some(0),
                            (media == "movies").then_some(0),
                            invalid
                        ]
                    )
                    .await
                    .is_err()
                );
                assert_eq!(
                    rows(&c, "provider_scopes").await?,
                    before,
                    "failed replacement must preserve original scope"
                );
            }
            c.execute(
                statement,
                params![
                    id.clone(),
                    implementation,
                    media,
                    (media == "tv").then_some(0),
                    (media == "movies").then_some(0),
                    wanted
                ],
            )
            .await?;
            assert_eq!(binding(&c, &id, media).await?.as_deref(), Some(wanted));
        }
    }
    for media in ["tv", "movies"] {
        assert!(c.execute("INSERT OR REPLACE INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags,download_client_id) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0,?)",params![client.clone(),media,media,wanted]).await.is_err());
        assert_eq!(binding(&c, &client, media).await?, None);
    }
    integrity(&c).await?;
    Ok(())
}
