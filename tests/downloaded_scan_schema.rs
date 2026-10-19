//! Actual53 migration witnesses and direct-SQL adversarial checks for the scan producer.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, TransactionBehavior, params};
use std::path::{Path, PathBuf};
const MIGRATION: &str = include_str!("../migrations/0054_downloaded_scan.sql");
const ADMISSION: [&str; 9] = [
    "commands_admit",
    "metadata_refresh_admit",
    "blocklist_clear_admit",
    "rss_commands_admit",
    "search_commands_admit",
    "manual_import_commands_admit",
    "quality_reset_admit",
    "rescan_admit",
    "health_admit",
];
const CAPACITY: &str = "+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))>=1024";
// Reasoning: reopening applies every later migration; 0060 appends exactly the recycle bin cleanup term after 0054's scan term.
const LATEST_CAPACITY: &str = "+(SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM recycle_bin_cleanup_commands WHERE status IN ('queued','running','retry_wait'))+(SELECT count(*) FROM rename_commands WHERE status IN ('queued','running','retry_wait'))>=1024";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-scan-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ())
        .await?
        .next()
        .await?
        .ok_or("missing scalar")?
        .get(0)?)
}
async fn dump(c: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    let mut rows = c.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        let mut v = Vec::new();
        for n in 0..r.column_count() {
            v.push(format!("{:?}", r.get_value(n)?));
        }
        out.push(v.join("|"));
    }
    out.sort();
    Ok(out)
}
async fn schema(c: &Connection) -> Result<Vec<String>, Error> {
    dump(
        c,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name",
    )
    .await
}
async fn predecessor(source: &Connection, path: &Path) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    c.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut rows=source.query("SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=53 ORDER BY version",()).await?;
    let mut n = 0;
    while let Some(r) = rows.next().await? {
        let version: i64 = r.get(0)?;
        let name: String = r.get(1)?;
        let checksum: String = r.get(2)?;
        let sql: String = r.get(3)?;
        tx.execute_batch(&sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![version, name, checksum, sql],
        )
        .await?;
        n += 1;
    }
    assert_eq!(n, 53);
    assert!(
        tx.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(db)
}
// Snapshot the actual predecessor's column list, not a manually selected subset of library rows.
async fn witness_queries(c: &Connection) -> Result<Vec<String>, Error> {
    let mut tables=c.query("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",()).await?;
    let mut out = Vec::new();
    while let Some(r) = tables.next().await? {
        let name: String = r.get(0)?;
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let mut columns = c.query(&format!("PRAGMA table_info({quoted})"), ()).await?;
        let mut names = Vec::new();
        while let Some(col) = columns.next().await? {
            let name: String = col.get(1)?;
            names.push(format!("\"{}\"", name.replace('"', "\"\"")));
        }
        let filter = if name == "schema_migrations" {
            " WHERE version<=53"
        } else if name == "health_checks" {
            // Reasoning: 0060 registers recycle_bin identities after this predecessor; preserved rows stay exact.
            " WHERE check_key!='recycle_bin'"
        } else {
            ""
        };
        out.push(format!("SELECT {} FROM {quoted}{filter}", names.join(",")));
    }
    Ok(out)
}
async fn witness(c: &Connection, queries: &[String]) -> Result<Vec<Vec<String>>, Error> {
    let mut out = Vec::new();
    for q in queries {
        out.push(dump(c, q).await?);
    }
    Ok(out)
}

async fn seed(c: &Connection) -> Result<(), Error> {
    c.execute_batch("PRAGMA foreign_keys=ON;
      INSERT INTO series(id,tvdb_id,title,path) VALUES(1,101,'Series','/tv/one');
      INSERT INTO seasons(series_id,number) VALUES(1,1);
      INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'Episode');
      INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(1,201,'Film');
      INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/one');
      INSERT INTO operations VALUES('tv','episode',1,NULL,'/source/tv','copy','/tv/one/a.mkv','preview','kept');
      INSERT INTO operations VALUES('movie','movie',NULL,1,'/source/movie','copy','/movies/one/a.mkv','preview','kept');
      INSERT INTO import_journal(operation_id,plan_json,phase,error_code) VALUES('tv','{\"private\":\"tv\"}','preview','import_error'),('movie','{\"private\":\"movie\"}','staging',NULL);
      INSERT INTO movie_metadata(id,tmdb_id,title) VALUES(2,202,'Completed Film');
      INSERT INTO movies(id,metadata_id,path) VALUES(2,2,'/movies/completed');
      INSERT INTO movie_files(id,movie_id,path) VALUES(2,2,'/movies/completed/a.mkv');
      INSERT INTO operations VALUES('done','movie',NULL,2,'/source/done','copy','/movies/completed/a.mkv','complete','kept history');
      INSERT INTO import_journal(operation_id,plan_json,stage_json,phase) VALUES('done','{}','{}','staging');
      INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) VALUES('done','movie',2,2,'/source/done','/movies/completed/a.mkv',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
      UPDATE import_journal SET phase='complete' WHERE operation_id='done';
      INSERT INTO remote_path_mappings(media_type,host,remote_path,remote_kind,remote_key,local_path) VALUES('tv','fixture.invalid','/remote','posix','/remote','/source/tv'),('movies','fixture.invalid','/remote','posix','/remote','/source/movie');
      UPDATE health_checks SET generation=1,observed_generation=1,observed_epoch='00000000-0000-0000-0000-000000000001',checked_at=100,severity=0 WHERE check_key='completed_download_handling';").await?;
    let indexer = uuid::Uuid::new_v4().to_string();
    let client = uuid::Uuid::new_v4().to_string();
    for (id, implementation) in [(&indexer, "torznab"), (&client, "qbittorrent")] {
        c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,?,?,1,1,1,1,'http://fixture.invalid')", params![id.clone(),implementation,implementation]).await?;
    }
    // Add both scopes before policy rows: scope changes intentionally invalidate existing policy.
    for media in ["tv", "movies"] {
        let (anime, year, categories) = if media == "tv" {
            (Some(0), None, "[5000]")
        } else {
            (None, Some(0), "[2000]")
        };
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year) VALUES(?,'torznab',?,?,'[]',?,?)", params![indexer.clone(),media,categories,anime,year]).await?;
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent',?,?,0,0,'started','default',0,0,0)", params![client.clone(),media,media]).await?;
    }
    for (media, digit) in [("tv", "a"), ("movies", "b")] {
        c.execute("INSERT INTO download_processing_policies(provider_id,media_type,provider_revision,revision,enabled,mode) VALUES(?,?,1,1,1,'copy')", params![client.clone(),media]).await?;
        let command = uuid::Uuid::new_v4().to_string();
        let candidate = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,next_attempt_at,created_at) VALUES(?,'rss_sync',?,?,1,?,1,100,100)", params![command.clone(),media,indexer.clone(),client.clone()]).await?;
        c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,status,decision_reasons_json,created_at,updated_at) SELECT ?,id,media_type,indexer_id,indexer_revision,client_id,client_revision,?,'Release',?,?,'pending','[]',100,100 FROM rss_commands WHERE id=?", params![candidate.clone(),digit.repeat(64),vec![1u8;29],if media=="tv" {Some(1)} else {None},command]).await?;
        if media == "tv" {
            c.execute(
                "INSERT INTO rss_candidate_episodes VALUES(?,1,1)",
                [candidate.clone()],
            )
            .await?;
        }
        let hash = digit.repeat(40);
        let identity = serde_json::json!({"version":1,"target":{"media_type":if media=="tv" {"episode"} else {"movie"},"id":1},"hashes":[hash],"settings_fingerprint":"c".repeat(64),"payload_sha256":"d".repeat(64)}).to_string();
        c.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,movie_id=? WHERE id=?", params![identity,if media=="movies" {Some(1)} else {None},candidate.clone()]).await?;
        c.execute(
            "INSERT INTO rss_hash_claims VALUES(?,?,?)",
            params![client.clone(), hash.clone(), candidate.clone()],
        )
        .await?;
        c.execute(
            "UPDATE rss_candidates SET status='submitting',private_payload=NULL WHERE id=?",
            [candidate.clone()],
        )
        .await?;
        c.execute(
            "UPDATE rss_candidates SET status='observed',observed_hash=? WHERE id=?",
            params![hash, candidate.clone()],
        )
        .await?;
        c.execute("INSERT INTO download_processing(candidate_id,policy_revision,status,next_attempt_at,created_at,updated_at) VALUES(?,1,'queued',100,100,100)", [candidate.clone()]).await?;
        c.execute("UPDATE download_processing SET status='checking',preflight_attempts=1,total_preflight_attempts=1 WHERE candidate_id=?", [candidate.clone()]).await?;
        c.execute("UPDATE download_processing SET status='blocked',error_code='source_unavailable',reasons_json='[\"hc021_source_missing_prepare\",\"source_unavailable\"]' WHERE candidate_id=?", [candidate]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn schema54_actual53_upgrade_backup_reopen_and_exact_admission_sql() -> Result<(), Error> {
    let scratch = Scratch::new();
    let fresh = Database::open_local(scratch.0.join("fresh")).await?;
    let c = fresh.connect().await?;
    // Fresh databases include later series-runtime and import-policy migrations; the upgrade witness remains53->54.
    assert_eq!(
        scalar(&c, "SELECT max(version) FROM schema_migrations").await?,
        76 // Reasoning: latest is migration file 0090 indexer_breadth at array position 76, appended after 0075 downloadstation; historical prefixes stay fixed.
    );
    let path = scratch.0.join("upgrade");
    let old = predecessor(&c, &path).await?;
    let before = old.connect()?;
    seed(&before).await?;
    // Retain a real predecessor snapshot receipt so 0055/56's inactive marker defaults
    // is checked on a row, rather than inferred from an empty table.
    before.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version) VALUES('sonarr','scan-schema-runtime',233)", ()).await?;
    before.execute("UPDATE import_journal SET path_failure_code='source_missing_precommit' WHERE operation_id='tv'",()).await?;
    let queries = witness_queries(&before).await?;
    let data = witness(&before, &queries).await?;
    let original = schema(&before).await?;
    let mut admission = Vec::new();
    for name in ADMISSION {
        let sql: String = before
            .query("SELECT sql FROM sqlite_schema WHERE name=?", [name])
            .await?
            .next()
            .await?
            .unwrap()
            .get(0)?;
        assert_eq!(sql.matches(">=1024").count(), 1);
        admission.push((name.to_owned(), sql.replace(">=1024", CAPACITY)));
    }
    // Only these pre-existing SQL objects change. All other trigger bodies/indexes stay byte-exact.
    let allowed = [
        "download_processing",
        "processing_transition",
        "same_path_admit",
        "rss_movie_target_owned",
        "rss_episode_target_owned",
    ];
    let mut unchanged = Vec::new();
    let mut rows = before
        .query(
            "SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let sql: String = row.get(1)?;
        if !ADMISSION.contains(&name.as_str()) && !allowed.contains(&name.as_str()) {
            unchanged.push((name, sql));
        }
    }
    drop(rows);
    // Keep the exact actual53->54 SQL proof before the latest runner appends0055.
    // Mirror the runner's bounded FK suspension for the processing-table rebuild.
    before.execute("PRAGMA foreign_keys=OFF", ()).await?;
    let tx = before
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATION).await?;
    assert_eq!(witness(&tx, &queries).await?, data);
    for (name, expected) in admission.iter().chain(unchanged.iter()) {
        let actual: String = tx
            .query("SELECT sql FROM sqlite_schema WHERE name=?", [name.clone()])
            .await?
            .next()
            .await?
            .ok_or("missing preserved object")?
            .get(0)?;
        assert_eq!(
            actual, *expected,
            "0054 changed SQL outside allocated predicate: {name}"
        );
    }
    assert!(
        tx.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    tx.rollback().await?;
    before.execute("PRAGMA foreign_keys=ON", ()).await?;
    assert_eq!(schema(&before).await?, original);
    assert_eq!(witness(&before, &queries).await?, data);
    drop(before);
    drop(old);
    let db = Database::open_local(&path).await?;
    let backup = db
        .migration_backup()
        .ok_or("missing migration backup")?
        .to_path_buf();
    let after = db.connect().await?;
    assert_eq!(witness(&after, &queries).await?, data);
    // 0055/56 append columns to these two predecessor tables only. Their exact old DDL was
    // checked above; every other preserved guard/index still compares byte-for-byte.
    for (name, expected) in admission
        .into_iter()
        .map(|(name, sql)| (name, sql.replace(CAPACITY, LATEST_CAPACITY)))
        .chain(
            unchanged
                .into_iter()
                .filter(|(name, _)| !matches!(name.as_str(), "series" | "snapshot_imports")),
        )
    {
        // Reasoning: 0059 rebuilds the provider tables and their shape/options/test-owner
        // guards (covered by db::download_client_registry_tests); skip only those objects.
        if matches!(
            name.as_str(),
            "providers"
                | "provider_scopes"
                | "cdh_scope_authority"
                | "provider_client_options_insert"
                | "provider_client_options_update"
                | "provider_scope_options_insert"
                | "provider_scope_options_update"
                | "provider_test_insert_owner"
                | "provider_test_update_owner"
                // Reasoning: 0065 appends collection policy columns to movie_collections; its rows
                // and predecessor columns are covered by tests/autotagging_metadata_schema.rs.
                | "movie_collections"
        ) {
            continue;
        }
        let actual: String = after
            .query("SELECT sql FROM sqlite_schema WHERE name=?", [name.clone()])
            .await?
            .next()
            .await?
            .ok_or("missing preserved object")?
            .get(0)?;
        // Reasoning: 0058 otherwise changes only the client predicate, now resolved through the
        // provider_implementations registry (protocol-matched for RSS/search); map it back.
        // Reasoning: 0068 widens only the RSS admission/schedule predicates' indexer list to torrentrss;
        // reverse that one exact edit so every other byte is still compared.
        let actual = actual.replace(
            "p.implementation IN ('torznab','newznab','torrentrss')",
            "p.implementation IN ('torznab','newznab')",
        );
        // Reasoning: 0069 appends the snapshot-origin provenance column to the membership table.
        let actual = actual
            .replace(", origin TEXT NOT NULL DEFAULT 'legacy' CHECK(origin IN('legacy','snapshot','discovery','graph'))", "")
            .replace(
                "p.implementation IN (SELECT c.implementation FROM provider_implementations c JOIN provider_implementations i ON i.protocol=c.protocol JOIN providers x ON x.implementation=i.implementation WHERE c.kind='download_client' AND x.id=NEW.indexer_id)",
                "p.implementation='qbittorrent'",
            )
            .replace(
                " IN (SELECT implementation FROM provider_implementations WHERE kind='download_client')",
                "='qbittorrent'",
            );
        assert_eq!(
            actual, expected,
            "SQL changed outside allocated predicate: {name}"
        );
    }
    assert_eq!(
        scalar(&after, "SELECT count(*) FROM series WHERE runtime IS NULL").await?,
        1
    );
    assert_eq!(scalar(&after, "SELECT series_runtime_version FROM snapshot_imports WHERE fingerprint='scan-schema-runtime'").await?, 0);
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM download_processing WHERE scan_command_id IS NULL"
        )
        .await?,
        2
    );
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM health_checks WHERE check_key!='recycle_bin'"
        )
        .await?,
        22, // Reasoning: 0060 adds recycle_bin identities; 0054's scan itself adds none.
        "scan reuses the existing scoped Health identities"
    );
    assert!(
        after
            .query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    let archived = libsql::Builder::new_local(backup.join("database.db"))
        .build()
        .await?;
    let archived = archived.connect()?;
    assert_eq!(schema(&archived).await?, original);
    assert_eq!(witness(&archived, &queries).await?, data);
    assert_eq!(
        scalar(&archived, "SELECT max(version) FROM schema_migrations").await?,
        53
    );
    let committed = schema(&after).await?;
    drop(after);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    let c = reopened.connect().await?;
    assert_eq!(schema(&c).await?, committed);
    assert_eq!(witness(&c, &queries).await?, data);
    Ok(())
}

#[tokio::test]
async fn schema54_late_failure_rolls_back_rebuild_and_retains_exact_backup() -> Result<(), Error> {
    let scratch = Scratch::new();
    let fresh = Database::open_local(scratch.0.join("fresh")).await?;
    let source = fresh.connect().await?;
    let path = scratch.0.join("failed");
    let old = predecessor(&source, &path).await?;
    let c = old.connect()?;
    seed(&c).await?;
    c.execute_batch("CREATE TRIGGER reject_schema54 BEFORE INSERT ON schema_migrations WHEN NEW.version=54 BEGIN SELECT RAISE(ABORT,'injected late scan migration failure'); END;").await?;
    let queries = witness_queries(&c).await?;
    let data = witness(&c, &queries).await?;
    let before = schema(&c).await?;
    c.execute("PRAGMA foreign_keys=OFF", ()).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch(MIGRATION).await?;
    assert!(tx.execute("INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(54,'downloaded_scan','fixture',?)",[MIGRATION]).await.is_err());
    tx.rollback().await?;
    assert_eq!(schema(&c).await?, before);
    assert_eq!(witness(&c, &queries).await?, data);
    drop(c);
    drop(old);
    assert!(Database::open_local(&path).await.is_err());
    let unchanged = libsql::Builder::new_local(&path).build().await?;
    let c = unchanged.connect()?;
    assert_eq!(schema(&c).await?, before);
    assert_eq!(witness(&c, &queries).await?, data);
    let mut backups = 0;
    for entry in std::fs::read_dir(&scratch.0)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".hrrdarr-migration-")
        {
            let b = libsql::Builder::new_local(entry.path().join("database.db"))
                .build()
                .await?;
            let b = b.connect()?;
            assert_eq!(schema(&b).await?, before);
            assert_eq!(witness(&b, &queries).await?, data);
            backups += 1;
        }
    }
    assert_eq!(backups, 1);
    Ok(())
}

async fn enqueue(c: &Connection, media: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at)VALUES(?,?,'/download','copy',100,100,100)",params![id.clone(),media]).await?;
    Ok(id)
}
async fn claim(c: &Connection, id: &str) -> Result<(), Error> {
    assert_eq!(c.execute("UPDATE downloaded_scan_commands SET status='running',attempts=attempts+1,run_generation=run_generation+1,started_at=100 WHERE id=?",[id]).await?,1);
    Ok(())
}
// 0057 adds the new envelope bytes to the current retained-byte view at publication.
async fn publish(c: &Connection, id: &str) -> Result<(), Error> {
    assert_eq!(c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json='{\"version\":1}',inventory_count=(SELECT child_count FROM downloaded_scan_inventory_size WHERE id=?),inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?)+13-coalesce(length(CAST(inventory_json AS BLOB)),0) WHERE id=?",params![id,id,id]).await?,1);
    Ok(())
}
async fn rejected_child(c: &Connection, id: &str, ordinal: i64) -> Result<String, Error> {
    let child = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition) VALUES(?,?,?,? ,?,'{\"version\":1}','rejected')",params![child.clone(),id,ordinal,format!("{ordinal}.txt"),format!("/download/{ordinal}.txt")]).await?;
    Ok(child)
}
async fn library(c: &Connection) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,title,path)VALUES(1,'One','/tv/one'),(2,'Two','/tv/two'); INSERT INTO seasons(series_id,number)VALUES(1,1),(2,1); INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'One'),(2,1,1,2,'Two'),(3,2,1,1,'Other'); INSERT INTO movie_metadata(id,tmdb_id,title)VALUES(1,1,'Movie'); INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies/one');").await?;
    Ok(())
}
// Latest-only schema authority fixtures explicitly retain unavailable probe evidence; no physical import is claimed.
async fn selected_tv(c: &Connection, parent: &str, old: Option<i64>) -> Result<String, Error> {
    selected_tv_revision(
        c,
        parent,
        old,
        r#"{"version":1,"real":0,"is_repack":false}"#,
    )
    .await
}
async fn selected_tv_revision(
    c: &Connection,
    parent: &str,
    old: Option<i64>,
    revision: &str,
) -> Result<String, Error> {
    let child = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,selected,series_id,destination,resolved_mode,quality_id,revision_json)VALUES(?,?,0,'pack.mkv','/download/pack.mkv','{\"version\":1,\"dev\":1,\"ino\":2}', '1','2','{\"version\":1,\"observation\":{\"version\":1,\"state\":\"unavailable\",\"reason\":\"probe_executable_missing\"}}',1,1,'/tv/one/pack.mkv','copy',1,?)",params![child.clone(),parent,revision]).await?;
    for ep in [1, 2] {
        c.execute("INSERT INTO downloaded_scan_child_episodes(child_id,command_id,episode_id,series_id,expected_old_episode_file_id)VALUES(?,?,?,1,?)",params![child.clone(),parent,ep,old]).await?;
    }
    Ok(child)
}
async fn prepare(c: &Connection, parent: &str, child: &str) -> Result<String, Error> {
    let revision: String = c
        .query(
            "SELECT revision_json FROM downloaded_scan_children WHERE id=?",
            [child],
        )
        .await?
        .next()
        .await?
        .ok_or("missing child revision")?
        .get(0)?;
    let op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message)VALUES(?,'episode',1,'/download/pack.mkv','copy','/tv/one/pack.mkv','preview','')",[op.clone()]).await?;
    c.execute("INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,'{\"destination\":\"/tv/one/pack.mkv\"}','preview')",[op.clone()]).await?;
    c.execute("INSERT INTO scan_import_facts(operation_id,child_id,command_id,inventory_revision,run_generation,quality_id,revision_json,expected_size,comparison_facts_json)VALUES(?,?,?,1,1,1,?,1,'{\"version\":1}')",params![op.clone(),child,parent,revision]).await?;
    Ok(op)
}

#[tokio::test]
async fn schema54_generations_inventory_bounds_capacity_and_raw_reads() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    let parent = enqueue(&c, "tv").await?;
    assert!(c.execute("UPDATE downloaded_scan_commands SET status='running',attempts=1,started_at=100 WHERE id=?",[parent.clone()]).await.is_err());
    claim(&c, &parent).await?;
    for n in 0..5 {
        rejected_child(&c, &parent, n).await?;
        c.execute("UPDATE downloaded_scan_commands SET run_generation=run_generation+1,ready_order_at=ready_order_at+1 WHERE id=?",[parent.clone()]).await?;
    }
    assert_eq!(
        scalar(&c, "SELECT attempts FROM downloaded_scan_commands").await?,
        1,
        "continuations do not exhaust three admission attempts"
    );
    assert!(c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json='{\"version\":1}',inventory_count=4,inventory_bytes=0 WHERE id=?",[parent.clone()]).await.is_err());
    for json in [
        "{}",
        "{\"version\":null}",
        "{\"version\":2}",
        "{\"version\":true}",
        "{\"version\":1.0}",
    ] {
        assert!(c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json=?,inventory_count=5,inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?)+13-coalesce(length(CAST(inventory_json AS BLOB)),0) WHERE id=?",params![json,parent.clone(),parent.clone()]).await.is_err());
    }
    publish(&c, &parent).await?;
    assert!(rejected_child(&c, &parent, 5).await.is_err());
    c.execute("UPDATE downloaded_scan_commands SET status='succeeded',outcome='unsuccessful',completed_at=200 WHERE id=?",[parent.clone()]).await?;
    assert!(c.execute("UPDATE downloaded_scan_commands SET status='running',completed_at=NULL,outcome=NULL WHERE id=?",[parent.clone()]).await.is_err());
    let p = enqueue(&c, "movies").await?;
    claim(&c, &p).await?;
    for (path, decision) in [
        ("../escape", "{\"version\":1}"),
        ("x", "{}"),
        ("x", "{\"version\":null}"),
    ] {
        assert!(c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition)VALUES(?,?,0,?,'/download/x',?,'rejected')",params![uuid::Uuid::new_v4().to_string(),p.clone(),path,decision]).await.is_err());
    }
    assert!(rejected_child(&c, &p, 20000).await.is_err());
    assert!(
        c.execute(
            "UPDATE downloaded_scan_commands SET run_generation=9007199254740992 WHERE id=?",
            [p.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("UPDATE downloaded_scan_commands SET cancel_requested=1,cleanup_json='{\"version\":1}',cleanup_state='pending' WHERE id=?",[p.clone()]).await.is_err());
    // Corruption probe is rolled back; SQL returns bounded bytes before UTF-8/JSON interpretation.
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("PRAGMA ignore_check_constraints=ON", ()).await?;
    tx.execute_batch("DROP TRIGGER downloaded_scan_transition;")
        .await?;
    tx.execute(
        "UPDATE downloaded_scan_commands SET source_path=CAST(x'ff0061' AS TEXT) WHERE id=?",
        [p.clone()],
    )
    .await?;
    let row=tx.query("SELECT length(CAST(source_path AS BLOB)),substr(CAST(source_path AS BLOB),1,4097) FROM downloaded_scan_commands WHERE id=?",[p.clone()]).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 3);
    assert_eq!(row.get::<Vec<u8>>(1)?, [255, 0, 97]);
    tx.execute("UPDATE downloaded_scan_commands SET source_path=CAST(zeroblob(1048576) AS TEXT) WHERE id=?",[p.clone()]).await?;
    let row=tx.query("SELECT length(CAST(source_path AS BLOB)),substr(CAST(source_path AS BLOB),1,4097) FROM downloaded_scan_commands WHERE id=?",[p.clone()]).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 1048576);
    assert_eq!(row.get::<Vec<u8>>(1)?.len(), 4097);
    tx.rollback().await?;
    c.execute("PRAGMA ignore_check_constraints=OFF", ()).await?;
    // All active parents count, including a cancelling running parent. Safe terminal rows do not.
    c.execute(
        "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=?",
        [p],
    )
    .await?;
    c.execute_batch("WITH RECURSIVE n(i)AS(SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1023) INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at)SELECT printf('%08x-0000-0000-0000-000000000000',i),'tv','/download','copy',100,100,100 FROM n;").await?;
    assert!(enqueue(&c, "tv").await.is_err());
    assert!(c.execute("INSERT INTO blocklist_clear_commands(id,name,media_type,next_attempt_at,created_at)VALUES(?,'clear_blocklist','tv',100,100)",[uuid::Uuid::new_v4().to_string()]).await.is_err());
    assert_eq!(scalar(&c,"SELECT count(*) FROM downloaded_scan_commands WHERE status IN ('queued','running','retry_wait')").await?,1024);
    Ok(())
}

async fn archive(c: &Connection, op: &str, file: i64, path: &str) -> Result<(), Error> {
    let json=serde_json::json!({"version":1,"file_id":file,"path":path,"quarantine_name":format!(".hrrdarr-replaced-{op}-{file}")}).to_string();
    c.execute("INSERT INTO scan_import_replacements(operation_id,media_type,old_file_id,old_episode_file_id,old_file_json)VALUES(?,'tv',?,?,?)",params![op,file,file,json]).await?;
    Ok(())
}
async fn link_and_stage(c: &Connection, child: &str, op: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE downloaded_scan_children SET operation_id=?,disposition='importing' WHERE id=?",
        params![op, child],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE import_journal SET phase='staging' WHERE operation_id=?",
            [op]
        )
        .await
        .is_err(),
        "effects cannot arm without retained path evidence"
    );
    c.execute("UPDATE import_journal SET phase='staging',path_failure_code='unresolved_attempt' WHERE operation_id=?",[op]).await?;
    let stage = serde_json::json!({"sha256":"a".repeat(64)}).to_string();
    c.execute(
        "UPDATE import_journal SET phase='staged',stage_json=? WHERE operation_id=?",
        params![stage, op],
    )
    .await?;
    Ok(())
}
async fn history(c: &Connection, op: &str, file: i64) -> Result<(), Error> {
    c.execute("INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)VALUES(?,'episode',1,?,'/download/pack.mkv','/tv/one/pack.mkv',1,?)",params![op,file,"a".repeat(64)]).await?;
    Ok(())
}
async fn history_members(c: &Connection, op: &str, file: i64) -> Result<(), Error> {
    for ep in [1, 2] {
        c.execute("INSERT INTO import_history_episodes(operation_id,episode_id,episode_file_id)VALUES(?,?,?)",params![op,ep,file]).await?;
    }
    Ok(())
}
async fn settle_child(c: &Connection, parent: &str, child: &str, op: &str) -> Result<(), Error> {
    c.execute(
        "UPDATE import_journal SET phase='complete',path_failure_code=NULL WHERE operation_id=?",
        [op],
    )
    .await?;
    c.execute(
        "UPDATE downloaded_scan_children SET disposition='imported' WHERE id=?",
        [child],
    )
    .await?;
    c.execute("UPDATE downloaded_scan_commands SET status='succeeded',outcome='success',completed_at=200 WHERE id=?",[parent]).await?;
    Ok(())
}

#[tokio::test]
async fn schema54_multiepisode_history_shared_retirement_and_same_path_exchange()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(10,1,'/tv/one/old.mkv'); UPDATE episodes SET episode_file_id=10 WHERE id IN(1,2); INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(4,1,1,4,'Outside member',10);").await?;
    let parent = enqueue(&c, "tv").await?;
    claim(&c, &parent).await?;
    let child = selected_tv(&c, &parent, Some(10)).await?;
    assert!(
        c.execute("UPDATE series SET path='/changed' WHERE id=1", ())
            .await
            .is_err()
    );
    assert!(c.execute("INSERT INTO rescan_commands(id,media_type,series_id,next_attempt_at,created_at)VALUES(?,'tv',1,100,100)",[uuid::Uuid::new_v4().to_string()]).await.is_err());
    publish(&c, &parent).await?;
    let wrong_anchor = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message)VALUES(?,'episode',2,'/download/pack.mkv','copy','/tv/one/pack.mkv','preview','')",[wrong_anchor.clone()]).await?;
    c.execute("INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,'{\"destination\":\"/tv/one/pack.mkv\"}','preview')",[wrong_anchor.clone()]).await?;
    assert!(c.execute("INSERT INTO scan_import_facts(operation_id,child_id,command_id,inventory_revision,run_generation,quality_id,revision_json,expected_size,comparison_facts_json)VALUES(?,?,?,1,1,1,'{\"version\":1,\"real\":0,\"is_repack\":false}',1,'{\"version\":1}')",params![wrong_anchor,child.clone(),parent.clone()]).await.is_err(),"a valid nonminimum member is not the stable anchor");
    let op = prepare(&c, &parent, &child).await?;
    assert!(
        c.execute(
            "UPDATE downloaded_scan_children SET operation_id=?,disposition='importing' WHERE id=?",
            params![op.clone(), child.clone()]
        )
        .await
        .is_err(),
        "every distinct original must be archived before linkage"
    );
    archive(&c, &op, 10, "/tv/one/old.mkv").await?;
    assert!(c.execute("INSERT INTO manual_import_commands(id,batch_id,operation_id,next_attempt_at,created_at)VALUES(?,?,?,100,100)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),op.clone()]).await.is_err());
    link_and_stage(&c, &child, &op).await?;
    assert!(c.execute("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(5,1,1,5,'Unsafe attach',10)",()).await.is_err());
    assert!(
        c.execute("UPDATE episode_files SET path='/other' WHERE id=10", ())
            .await
            .is_err()
    );
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(20,1,'/tv/one/pack.mkv'); UPDATE episodes SET episode_file_id=20 WHERE id IN(1,2);").await?;
    history(&c, &op, 20).await?;
    c.execute(
        "INSERT INTO import_history_episodes VALUES(?,1,20)",
        [op.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
            [op.clone()]
        )
        .await
        .is_err(),
        "anchor history alone is insufficient"
    );
    assert!(
        c.execute(
            "INSERT INTO import_history_episodes VALUES(?,3,20)",
            [op.clone()]
        )
        .await
        .is_err(),
        "other-series target cannot become a member"
    );
    c.execute(
        "INSERT INTO import_history_episodes VALUES(?,2,20)",
        [op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
            [op.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("UPDATE downloaded_scan_commands SET status='cancelled',completed_at=200,cancel_requested=1 WHERE id=?",[parent.clone()]).await.is_err(),"uncertain effects cannot become terminal cancellation");
    c.execute("UPDATE scan_import_replacements SET retirement_state='shared_retained' WHERE operation_id=?",[op.clone()]).await?;
    settle_child(&c, &parent, &child, &op).await?;
    // Completed shared retention is historical proof, not a permanent lock on live library bytes.
    c.execute_batch("INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(5,1,1,5,'Later legitimate association',10); UPDATE episode_files SET path='/tv/one/renamed-old.mkv' WHERE id=10;").await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM episodes WHERE episode_file_id=10").await?,
        2
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM import_history_episodes").await?,
        2
    );
    // An outside user of the next original must prevent a same-path exchange.
    c.execute("UPDATE episodes SET episode_file_id=20 WHERE id=5", ())
        .await?;
    let second = enqueue(&c, "tv").await?;
    claim(&c, &second).await?;
    let next = selected_tv(&c, &second, Some(20)).await?;
    publish(&c, &second).await?;
    let next_op = prepare(&c, &second, &next).await?;
    archive(&c, &next_op, 20, "/tv/one/pack.mkv").await?;
    link_and_stage(&c, &next, &next_op).await?;
    assert!(
        c.execute(
            "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
            [next_op.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE episodes SET episode_file_id=NULL WHERE id=5", ())
        .await?;
    c.execute(
        "INSERT INTO same_path_replacements(operation_id)VALUES(?)",
        [next_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE same_path_replacements SET state='installed' WHERE operation_id=?",
        [next_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [next_op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE episodes SET episode_file_id=NULL WHERE id IN(1,2)",
        (),
    )
    .await?;
    let recovery = format!("/tv/one/.hrrdarr-replaced-{next_op}-20/original");
    c.execute("UPDATE episode_files SET path=? WHERE id=20", [recovery])
        .await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(21,1,'/tv/one/pack.mkv'); UPDATE episodes SET episode_file_id=21 WHERE id IN(1,2);").await?;
    history(&c, &next_op, 21).await?;
    history_members(&c, &next_op, 21).await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [next_op.clone()],
    )
    .await?;
    c.execute("UPDATE scan_import_replacements SET retirement_state='quarantined',retirement_json='{\"directory\":{\"dev\":1,\"ino\":2}}' WHERE operation_id=?",[next_op.clone()]).await?;
    settle_child(&c, &second, &next, &next_op).await?;
    assert!(
        c.execute("UPDATE episodes SET episode_file_id=20 WHERE id=5", ())
            .await
            .is_err(),
        "private quarantine bytes remain protected"
    );
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    Ok(())
}

const CAPTURE: &str = "UPDATE downloaded_scan_commands SET candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv'),coverage_state='pending',tracked_json=(SELECT json_object('version',1,'provider_id',client_id,'provider_revision',client_revision,'remote_id',observed_hash) FROM rss_candidates WHERE media_type='tv'),processing_fence_json=(SELECT json_object('version',1,'policy_revision',d.policy_revision,'status',d.status,'resume_requested',d.resume_requested,'preflight_attempts',d.preflight_attempts,'total_preflight_attempts',d.total_preflight_attempts,'next_attempt_at',d.next_attempt_at,'created_at',d.created_at,'updated_at',d.updated_at,'error_code',d.error_code,'reasons_json',d.reasons_json,'scan_command_id',d.scan_command_id) FROM download_processing d JOIN rss_candidates r ON r.id=d.candidate_id WHERE r.media_type='tv') WHERE id=?";
const PARTIAL: &str = "UPDATE downloaded_scan_commands SET status='succeeded',outcome='unsuccessful',coverage_state='incomplete',completed_at=200 WHERE id=?";
const HANDBACK_PARTIAL: &str = "UPDATE download_processing SET status='blocked',scan_command_id=?,resume_requested=0,error_code='import_failed',reasons_json='[\"hc021_source_missing_prepare\",\"downloaded_scan_incomplete\"]',updated_at=200 WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')";
async fn successor(c: &Connection, previous: &str) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at,supersedes_command_id)VALUES(?,'tv','/download','copy',100,100,100,?)",params![id.clone(),previous]).await?;
    claim(c, &id).await?;
    Ok(id)
}
async fn capture(c: &Connection, id: &str) -> Result<(), Error> {
    c.execute(CAPTURE, [id]).await?;
    c.execute(
        "INSERT INTO downloaded_scan_coverage(command_id,episode_id)VALUES(?,1)",
        [id],
    )
    .await?;
    Ok(())
}
#[tokio::test]
async fn schema54_tracked_partial_lineage_handback_atomicity_and_full_coverage() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    c.execute(
        "INSERT INTO episodes(id,series_id,season,number,title)VALUES(2,1,1,2,'Second')",
        (),
    )
    .await?;
    let observed = c
        .query(
            "SELECT client_id,observed_hash FROM rss_candidates WHERE media_type='tv'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap();
    let provider: String = observed.get(0)?;
    let remote: String = observed.get(1)?;
    for (requested_provider, requested_remote) in [
        (uuid::Uuid::nil().to_string(), remote.clone()),
        (provider, format!("{remote}f")),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,requested_provider_id,requested_remote_id,ready_order_at,next_attempt_at,created_at)VALUES(?,'tv','/download','copy',?,?,100,100,100)",params![id.clone(),requested_provider,requested_remote]).await?;
        claim(&c, &id).await?;
        assert!(
            c.execute(CAPTURE, [id.clone()]).await.is_err(),
            "explicit request cannot capture another provider/remote identity"
        );
        assert_eq!(
            c.query(
                "SELECT candidate_id IS NULL FROM downloaded_scan_commands WHERE id=?",
                [id]
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
            1
        );
    }
    let mut parent = enqueue(&c, "tv").await?;
    claim(&c, &parent).await?;
    for version in ["true", "1.0"] {
        let invalid = CAPTURE.replace("'version',1", &format!("'version',json('{version}')"));
        assert!(
            c.execute(&invalid, [parent.clone()]).await.is_err(),
            "noninteger tracked/fence envelope version was accepted"
        );
    }
    capture(&c, &parent).await?;
    assert!(
        c.execute(
            "INSERT INTO downloaded_scan_coverage(command_id,movie_id)VALUES(?,1)",
            [parent.clone()]
        )
        .await
        .is_err(),
        "movie1 is not episode1 despite equal numeric IDs"
    );
    assert!(c.execute("UPDATE download_processing SET status='cancelled' WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')",()).await.is_err(),"captured receipt cannot drift beneath immutable handback fence");
    publish(&c, &parent).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute(PARTIAL, [parent.clone()]).await?;
    tx.execute_batch("CREATE TRIGGER fail_scan_handback BEFORE UPDATE ON download_processing BEGIN SELECT RAISE(ABORT,'injected handback failure'); END;").await?;
    assert!(
        tx.execute(HANDBACK_PARTIAL, [parent.clone()])
            .await
            .is_err()
    );
    tx.rollback().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM downloaded_scan_commands WHERE status='running' AND coverage_state='pending'").await?,1,"parent settlement rolls back with failed receipt CAS");
    for generation in 0..3 {
        if generation > 0 {
            parent = successor(&c, &parent).await?;
            capture(&c, &parent).await?;
            publish(&c, &parent).await?;
        }
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        tx.execute(PARTIAL, [parent.clone()]).await?;
        tx.execute(HANDBACK_PARTIAL, [parent.clone()]).await?;
        tx.commit().await?;
        assert!(c.execute("UPDATE download_processing SET status='queued',preflight_attempts=0 WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')",()).await.is_err(),"retained incomplete proof excludes ordinary singleton fallback");
    }
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM downloaded_scan_commands WHERE coverage_state='incomplete'"
        )
        .await?,
        3,
        "third successor can retain all earlier ancestors"
    );
    let last = successor(&c, &parent).await?;
    // 0058: the active successor holds the lineage; storage refuses a competing successor.
    let competing = uuid::Uuid::new_v4().to_string();
    assert!(
        c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at,supersedes_command_id)VALUES(?,'tv','/download','copy',100,100,100,?)",params![competing.clone(),parent.clone()]).await.is_err(),
        "a second successor of one predecessor was admitted"
    );
    capture(&c, &last).await?;
    let child = selected_tv(&c, &last, None).await?;
    publish(&c, &last).await?;
    let op = prepare(&c, &last, &child).await?;
    link_and_stage(&c, &child, &op).await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(20,1,'/tv/one/pack.mkv'); UPDATE episodes SET episode_file_id=20 WHERE id IN(1,2);").await?;
    history(&c, &op, 20).await?;
    history_members(&c, &op, 20).await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    assert!(c.execute("UPDATE downloaded_scan_coverage SET credited_operation_id=?,credited_file_id=20 WHERE command_id=?",params![op.clone(),last.clone()]).await.is_err(),"committed but unfinished cleanup is not coverage");
    c.execute(
        "UPDATE import_journal SET phase='complete',path_failure_code=NULL WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE downloaded_scan_children SET disposition='imported' WHERE id=?",
        [child],
    )
    .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("UPDATE downloaded_scan_coverage SET credited_operation_id=?,credited_file_id=20 WHERE command_id=?",params![op.clone(),last.clone()]).await?;
    tx.execute("UPDATE downloaded_scan_commands SET status='succeeded',outcome='success',coverage_state='complete',completed_at=200 WHERE id=?",[last.clone()]).await?;
    tx.execute("UPDATE download_processing SET status='imported',scan_command_id=?,resume_requested=0,error_code=NULL,reasons_json='[]',updated_at=200 WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')",[last.clone()]).await?;
    tx.commit().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing d JOIN rss_candidates r ON r.id=d.candidate_id WHERE r.media_type='tv' AND d.status='imported' AND d.policy_revision=1 AND d.preflight_attempts=1 AND d.total_preflight_attempts=1 AND d.created_at=100 AND d.next_attempt_at=100").await?,1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM downloaded_scan_complete_coverage").await?,
        1
    );
    assert!(
        c.execute(
            "UPDATE downloaded_scan_coverage SET credited_file_id=999 WHERE command_id=?",
            [last]
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn schema54_missing_json_keys_canonical_inodes_and_resume_generations() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let parent = enqueue(&c, "tv").await?;
    claim(&c, &parent).await?;
    // This fixture uses the SQL minimum identity envelope. Runtime must decode and recheck the
    // complete filesystem identity (size/times/mode and anchored parents) before any effect.
    for key in ["", "01", "-1", "+1", "18446744073709551616"] {
        assert!(c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,disposition)VALUES(?,?,0,'x','/download/x','{\"version\":1,\"dev\":1,\"ino\":0}',?,'0','{\"version\":1}','rejected')",params![uuid::Uuid::new_v4().to_string(),parent.clone(),key]).await.is_err());
    }
    c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,disposition)VALUES(?,?,0,'x','/download/x','{\"version\":1,\"dev\":18446744073709551615,\"ino\":0}','18446744073709551615','0','{\"version\":1}','rejected')",params![uuid::Uuid::new_v4().to_string(),parent.clone()]).await?;
    // Isolate JSON CHECKs from the separate imported-move lifecycle guard in a rolled-back tx.
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute_batch("DROP TRIGGER downloaded_scan_transition;")
        .await?;
    let valid = serde_json::json!({"version":1,"root_identity":{"dev":1,"ino":2},"parent_identity":{"dev":1,"ino":3},"phase":"observed","reason":null});
    tx.execute(
        "UPDATE downloaded_scan_commands SET cleanup_state='pending',cleanup_json=? WHERE id=?",
        params![valid.to_string(), parent.clone()],
    )
    .await?;
    for key in [
        "version",
        "root_identity",
        "parent_identity",
        "phase",
        "reason",
    ] {
        for null in [false, true] {
            let mut value = valid.clone();
            if null {
                value[key] = serde_json::Value::Null;
            } else {
                value.as_object_mut().unwrap().remove(key);
            }
            if key == "reason" && null {
                continue;
            }
            assert!(
                tx.execute(
                    "UPDATE downloaded_scan_commands SET cleanup_json=? WHERE id=?",
                    params![value.to_string(), parent.clone()]
                )
                .await
                .is_err(),
                "accepted missing/null mandatory {key}"
            );
        }
    }
    for version in [serde_json::json!(true), serde_json::json!(1.0)] {
        let mut value = valid.clone();
        value["version"] = version;
        assert!(
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_json=? WHERE id=?",
                params![value.to_string(), parent.clone()]
            )
            .await
            .is_err()
        );
    }
    for key in ["dev", "ino"] {
        let mut value = valid.clone();
        value["root_identity"].as_object_mut().unwrap().remove(key);
        assert!(
            tx.execute(
                "UPDATE downloaded_scan_commands SET cleanup_json=? WHERE id=?",
                params![value.to_string(), parent.clone()]
            )
            .await
            .is_err()
        );
    }
    tx.rollback().await?;
    let p = enqueue(&c, "tv").await?;
    claim(&c, &p).await?;
    let child = selected_tv(&c, &p, None).await?;
    publish(&c, &p).await?;
    let before_bytes: i64 = c
        .query(
            "SELECT manifest_bytes FROM downloaded_scan_children WHERE id=?",
            [child.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    let published_bytes: i64 = c
        .query(
            "SELECT inventory_bytes FROM downloaded_scan_commands WHERE id=?",
            [p.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    let reason = "[\"schema_size_probe\"]";
    c.execute(
        "UPDATE downloaded_scan_children SET reasons_json=? WHERE id=?",
        params![reason, child.clone()],
    )
    .await?;
    let after_bytes: i64 = c
        .query(
            "SELECT manifest_bytes FROM downloaded_scan_children WHERE id=?",
            [child.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    assert_eq!(
        after_bytes - before_bytes,
        reason.len() as i64 - 2,
        "generated bytes recompute for a permitted result update"
    );
    assert_eq!(
        c.query(
            "SELECT inventory_bytes FROM downloaded_scan_commands WHERE id=?",
            [p.clone()]
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?,
        published_bytes,
        "publication bytes remain immutable historical facts"
    );
    assert!(
        c.execute(
            "UPDATE downloaded_scan_children SET manifest_bytes=0 WHERE id=?",
            [child.clone()]
        )
        .await
        .is_err(),
        "caller cannot forge generated accounting"
    );
    let mut plan = c
        .query(
            "EXPLAIN QUERY PLAN SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?",
            [p.clone()],
        )
        .await?;
    let mut detail = String::new();
    while let Some(row) = plan.next().await? {
        detail.push_str(&row.get::<String>(3)?);
    }
    assert!(detail.contains("downloaded_scan_child_bytes"), "{detail}");
    assert!(
        detail.contains("downloaded_scan_coverage_parent"),
        "{detail}"
    );
    let op = prepare(&c, &p, &child).await?;
    link_and_stage(&c, &child, &op).await?;
    c.execute(
        "UPDATE downloaded_scan_children SET resume_generation=1,resume_requested=1 WHERE id=?",
        [child.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE downloaded_scan_children SET resume_generation=2,resume_requested=1 WHERE id=?",
            [child.clone()]
        )
        .await
        .is_err(),
        "pending authority cannot be stacked"
    );
    c.execute(
        "UPDATE downloaded_scan_children SET resume_requested=0 WHERE id=?",
        [child.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE downloaded_scan_children SET resume_requested=1 WHERE id=?",
            [child.clone()]
        )
        .await
        .is_err(),
        "delayed duplicate cannot reauthorize after consumption"
    );
    c.execute(
        "UPDATE downloaded_scan_children SET resume_generation=2,resume_requested=1 WHERE id=?",
        [child],
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn schema54_movie_replacement_and_typed_complete_handback() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    // Finish the historical independent movie operation so the new producer has no live conflict.
    c.execute_batch("INSERT INTO movie_files(id,movie_id,path)VALUES(10,1,'/movies/one/a.mkv'); INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256)VALUES('movie','movie',1,10,'/source/movie','/movies/one/a.mkv',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); UPDATE import_journal SET phase='complete',stage_json='{}' WHERE operation_id='movie';").await?;
    c.execute("UPDATE download_processing SET status='queued',preflight_attempts=0,error_code=NULL WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='movies')",()).await?;
    let parent = enqueue(&c, "movies").await?;
    claim(&c, &parent).await?;
    c.execute(&CAPTURE.replace("'tv'", "'movies'"), [parent.clone()])
        .await?;
    c.execute(
        "INSERT INTO downloaded_scan_coverage(command_id,movie_id)VALUES(?,1)",
        [parent.clone()],
    )
    .await?;
    let child = uuid::Uuid::new_v4().to_string();
    let op = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,selected,movie_id,expected_old_movie_file_id,destination,resolved_mode,quality_id,revision_json,edition)VALUES(?,?,0,'film.mkv','/download/film.mkv','{\"version\":1,\"dev\":1,\"ino\":3}','1','3','{\"version\":1,\"observation\":{\"version\":1,\"state\":\"unavailable\",\"reason\":\"probe_executable_missing\"}}',1,1,10,'/movies/one/new.mkv','copy',1,'{\"version\":1,\"real\":0,\"is_repack\":false}','Extended')",params![child.clone(),parent.clone()]).await?;
    assert!(c.execute("INSERT INTO downloaded_scan_child_episodes(child_id,command_id,episode_id,series_id)VALUES(?,?,1,1)",params![child.clone(),parent.clone()]).await.is_err());
    publish(&c, &parent).await?;
    c.execute("INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message)VALUES(?,'movie',1,'/download/film.mkv','copy','/movies/one/new.mkv','preview','')",[op.clone()]).await?;
    c.execute("INSERT INTO import_journal(operation_id,plan_json,phase)VALUES(?,'{\"destination\":\"/movies/one/new.mkv\"}','preview')",[op.clone()]).await?;
    c.execute("INSERT INTO scan_import_facts(operation_id,child_id,command_id,inventory_revision,run_generation,quality_id,revision_json,edition,expected_size,comparison_facts_json)VALUES(?,?,?,1,1,1,'{\"version\":1,\"real\":0,\"is_repack\":false}','Extended',1,'{\"version\":1}')",params![op.clone(),child.clone(),parent.clone()]).await?;
    let old=serde_json::json!({"version":1,"file_id":10,"path":"/movies/one/a.mkv","quarantine_name":format!(".hrrdarr-replaced-{op}-10")}).to_string();
    c.execute("INSERT INTO scan_import_replacements(operation_id,media_type,old_file_id,old_movie_file_id,old_file_json)VALUES(?,'movies',10,10,?)",params![op.clone(),old]).await?;
    link_and_stage(&c, &child, &op).await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE movie_files SET path='/movies/one/new.mkv',edition='Extended' WHERE id=10",
        (),
    )
    .await?;
    c.execute("INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256)VALUES(?,'movie',1,10,'/download/film.mkv','/movies/one/new.mkv',1,?)",params![op.clone(),"a".repeat(64)]).await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    assert!(
        c.execute(
            "UPDATE import_journal SET phase='complete' WHERE operation_id=?",
            [op.clone()]
        )
        .await
        .is_err()
    );
    c.execute("UPDATE scan_import_replacements SET retirement_state='quarantined',retirement_json='{\"directory\":{\"dev\":1,\"ino\":3}}' WHERE operation_id=?",[op.clone()]).await?;
    c.execute(
        "UPDATE import_journal SET phase='complete',path_failure_code=NULL WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute(
        "UPDATE downloaded_scan_children SET disposition='imported' WHERE id=?",
        [child],
    )
    .await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("UPDATE downloaded_scan_coverage SET credited_operation_id=?,credited_file_id=10 WHERE command_id=?",params![op.clone(),parent.clone()]).await?;
    tx.execute("UPDATE downloaded_scan_commands SET status='succeeded',outcome='success',coverage_state='complete',completed_at=200 WHERE id=?",[parent.clone()]).await?;
    tx.execute("UPDATE download_processing SET status='imported',scan_command_id=?,resume_requested=0,error_code=NULL,reasons_json='[]',updated_at=200 WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='movies')",[parent.clone()]).await?;
    tx.commit().await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM downloaded_scan_coverage WHERE movie_id=1 AND episode_id IS NULL AND credited_file_id=10").await?,1);
    assert_eq!(scalar(&c,"SELECT count(*) FROM download_processing d JOIN rss_candidates r ON r.id=d.candidate_id WHERE r.media_type='movies' AND d.status='imported' AND d.preflight_attempts=0 AND d.total_preflight_attempts=1").await?,1);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM episodes WHERE id=1 AND episode_file_id IS NULL"
        )
        .await?,
        1,
        "movie1 does not mutate episode1"
    );
    Ok(())
}

#[tokio::test]
async fn schema54_actual_membership_capacity_and_aggregate_byte_overflow() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let parent = enqueue(&c, "tv").await?;
    claim(&c, &parent).await?;
    let child = selected_tv(&c, &parent, None).await?;
    let membership_started = std::time::Instant::now();
    // Episode3 belongs to the other owner in library(); use IDs4..20002 for the remaining members.
    c.execute_batch("WITH RECURSIVE n(i)AS(SELECT 4 UNION ALL SELECT i+1 FROM n WHERE i<20002) INSERT INTO episodes(id,series_id,season,number,title)SELECT i,1,1,i,'Capacity member' FROM n;").await?;
    c.execute("INSERT INTO downloaded_scan_child_episodes(child_id,command_id,episode_id,series_id) SELECT ?,?,id,1 FROM episodes WHERE series_id=1 AND id BETWEEN 4 AND 20001",params![child.clone(),parent.clone()]).await?;
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM downloaded_scan_child_episodes").await?,
        20000
    );
    assert!(c.execute("INSERT INTO downloaded_scan_child_episodes(child_id,command_id,episode_id,series_id)VALUES(?,?,20002,1)",params![child,parent.clone()]).await.is_err());
    publish(&c, &parent).await?;
    eprintln!(
        "scan schema membership boundary: {:?}",
        membership_started.elapsed()
    );
    // This deliberately uses actual byte-heavy rows, not a caller-supplied fake counter.
    // It measures a storage bound only; no filesystem work occurs in these schema fixtures.
    let large = enqueue(&c, "movies").await?;
    claim(&c, &large).await?;
    // Keep one mutable selected result to test growth after publication at the same real limit.
    let mutable_child = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,selected,movie_id,destination,resolved_mode,quality_id,revision_json)VALUES(?,?,19000,'film.mkv','/download/film.mkv','{\"version\":1,\"dev\":2,\"ino\":3}','2','3','{\"version\":1,\"observation\":{\"version\":1,\"state\":\"unavailable\",\"reason\":\"probe_executable_missing\"}}',1,1,'/movies/one/film.mkv','copy',1,'{\"version\":1,\"real\":0,\"is_repack\":false}')",params![mutable_child.clone(),large.clone()]).await?;
    let decision = serde_json::json!({"version":1,"padding":"x".repeat(16000)}).to_string();
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let bytes_started = std::time::Instant::now();
    let mut accepted = 0;
    let mut rejection = None;
    for ordinal in 0..5000 {
        let result=tx.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition)VALUES(?,?,?,?,?,?,'rejected')",params![uuid::Uuid::new_v4().to_string(),large.clone(),ordinal,format!("{ordinal}.txt"),format!("/download/{ordinal}.txt"),decision.clone()]).await;
        if let Err(error) = result {
            rejection = Some(error.to_string());
            break;
        }
        accepted += 1;
    }
    eprintln!(
        "scan schema byte boundary: {:?}; accepted_children={accepted}",
        bytes_started.elapsed()
    );
    assert!(
        rejection
            .as_deref()
            .is_some_and(|error| error.contains("scan inventory byte capacity reached")),
        "unexpected insertion rejection: {rejection:?}"
    );
    assert!(
        accepted > 4000 && accepted < 5000,
        "expected the64MiB budget to reject the next bounded row, accepted {accepted}"
    );
    let bytes = tx
        .query(
            "SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?",
            [large.clone()],
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get::<i64>(0)?;
    assert!(bytes <= 67108864);
    let next_bytes = decision.len()
        + format!("{accepted}.txt").len()
        + format!("/download/{accepted}.txt").len()
        + 2
        + 128;
    assert!(
        bytes + next_bytes as i64 > 67108864,
        "rejection occurs at aggregate boundary"
    );
    let remaining = 67108864 - bytes;
    if remaining > 400 {
        let empty = serde_json::json!({"version":1,"padding":""}).to_string();
        let overhead =
            empty.len() + "boundary.txt".len() + "/download/boundary.txt".len() + 2 + 128;
        // 0057 counts the13-byte publication envelope once, leaving the same10-byte margin.
        let padding = remaining as usize - overhead - 13 - 10;
        let tail = serde_json::json!({"version":1,"padding":"x".repeat(padding)}).to_string();
        tx.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition)VALUES(?,?,19999,'boundary.txt','/download/boundary.txt',?,'rejected')",params![uuid::Uuid::new_v4().to_string(),large.clone(),tail]).await?;
    }
    tx.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json='{\"version\":1}',inventory_count=(SELECT child_count FROM downloaded_scan_inventory_size WHERE id=?),inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?)+13-coalesce(length(CAST(inventory_json AS BLOB)),0) WHERE id=?",params![large.clone(),large.clone(),large.clone()]).await?;
    let reasons = serde_json::to_string(
        &(0..64)
            .map(|n| format!("reason_{n}_{}", "x".repeat(110)))
            .collect::<Vec<_>>(),
    )?;
    assert!(reasons.len() < 8192);
    let error = tx
        .execute(
            "UPDATE downloaded_scan_children SET reasons_json=? WHERE id=?",
            params![reasons, mutable_child],
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("scan inventory byte capacity reached"),
        "unexpected update rejection: {error}"
    );
    tx.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn schema54_predecessor_remains_authority_until_complete() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    c.execute("INSERT INTO rss_candidate_imports(candidate_id,operation_id,quality_id,revision_json,provenance_json)SELECT id,'tv',1,'{\"version\":1,\"real\":0,\"is_repack\":false}','{}' FROM rss_candidates WHERE media_type='tv'",()).await?;
    let p = enqueue(&c, "tv").await?;
    claim(&c, &p).await?;
    c.execute(
        &CAPTURE.replace(
            "SET candidate_id=",
            "SET predecessor_operation_id='tv',candidate_id=",
        ),
        [p.clone()],
    )
    .await?;
    assert!(
        selected_tv(&c, &p, None).await.is_err(),
        "idle or failed predecessor is not released authority"
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM downloaded_scan_children").await?,
        0
    );
    // No child reservation prevents this exact predecessor's original commit/cleanup path.
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(30,1,'/tv/one/a.mkv'); UPDATE episodes SET episode_file_id=30 WHERE id=1; INSERT INTO import_history(operation_id,media_type,episode_id,episode_file_id,source,destination,size,sha256)VALUES('tv','episode',1,30,'/source/tv','/tv/one/a.mkv',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); UPDATE import_journal SET phase='complete',stage_json='{}',error_code=NULL WHERE operation_id='tv'; INSERT INTO episodes(id,series_id,season,number,title,episode_file_id)VALUES(2,1,1,2,'Second',30);").await?;
    selected_tv(&c, &p, Some(30)).await?;
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM rss_candidate_imports WHERE operation_id='tv'"
        )
        .await?,
        1
    );
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM scan_import_facts").await?,
        0,
        "predecessor is never reowned as a scan child"
    );
    Ok(())
}

#[tokio::test]
async fn schema54_quality_revision_zero_is_not_protocol_version_zero() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    // Pinned Sonarr AggregateQualityFixture144–170 treats explicit file-quality v0 as real
    // evidence. This producer value is not the version of any persisted protocol envelope.
    for revision in [
        r#"{"version":-1,"real":0,"is_repack":false}"#,
        r#"{"version":true,"real":0,"is_repack":false}"#,
        r#"{"version":1.0,"real":0,"is_repack":false}"#,
        r#"{"version":1.5,"real":0,"is_repack":false}"#,
        r#"{"version":"0","real":0,"is_repack":false}"#,
        r#"{"version":2147483648,"real":0,"is_repack":false}"#,
        r#"{"version":0,"real":-1,"is_repack":false}"#,
        r#"{"version":0,"real":true,"is_repack":false}"#,
        r#"{"version":0,"real":1.0,"is_repack":false}"#,
        r#"{"version":0,"real":0.5,"is_repack":false}"#,
        r#"{"version":0,"real":"0","is_repack":false}"#,
        r#"{"version":0,"real":2147483648,"is_repack":false}"#,
    ] {
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let parent = enqueue(&tx, "tv").await?;
        claim(&tx, &parent).await?;
        // Matching invalid child/facts values ensure rejection is the revision constraint,
        // rather than an unrelated child/facts equality or current-authority failure.
        let child = selected_tv_revision(&tx, &parent, None, revision).await?;
        publish(&tx, &parent).await?;
        let error = prepare(&tx, &parent, &child).await.unwrap_err();
        assert!(
            error.to_string().contains("invalid scan quality revision"),
            "{revision}: {error}"
        );
        tx.rollback().await?;
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let parent = enqueue(&tx, "tv").await?;
    claim(&tx, &parent).await?;
    let child = selected_tv_revision(
        &tx,
        &parent,
        None,
        r#"{"version":2147483647,"real":2147483647,"is_repack":true}"#,
    )
    .await?;
    publish(&tx, &parent).await?;
    prepare(&tx, &parent, &child).await?;
    tx.rollback().await?;

    let parent = enqueue(&c, "tv").await?;
    claim(&c, &parent).await?;
    let child = selected_tv_revision(
        &c,
        &parent,
        None,
        r#"{"version":0,"real":0,"is_repack":false}"#,
    )
    .await?;
    assert!(c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json='{\"version\":0}',inventory_count=(SELECT child_count FROM downloaded_scan_inventory_size WHERE id=?),inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?)+13-coalesce(length(CAST(inventory_json AS BLOB)),0) WHERE id=?",params![parent.clone(),parent.clone(),parent.clone()]).await.is_err(),"quality v0 must not permit protocol envelope v0");
    publish(&c, &parent).await?;
    let op = prepare(&c, &parent, &child).await?;
    link_and_stage(&c, &child, &op).await?;
    c.execute(
        "UPDATE import_journal SET phase='published' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    c.execute_batch("INSERT INTO episode_files(id,series_id,path)VALUES(20,1,'/tv/one/pack.mkv'); UPDATE episodes SET episode_file_id=20 WHERE id IN(1,2);").await?;
    history(&c, &op, 20).await?;
    history_members(&c, &op, 20).await?;
    c.execute(
        "UPDATE import_journal SET phase='committed' WHERE operation_id=?",
        [op.clone()],
    )
    .await?;
    settle_child(&c, &parent, &child, &op).await?;
    assert_eq!(scalar(&c,"SELECT count(*) FROM scan_import_facts f JOIN import_journal j ON j.operation_id=f.operation_id JOIN import_history h ON h.operation_id=f.operation_id WHERE json_type(f.revision_json,'$.version')='integer' AND json_extract(f.revision_json,'$.version')=0 AND j.phase='complete'").await?,1);
    assert_eq!(
        scalar(&c, "SELECT count(*) FROM import_history_episodes").await?,
        2
    );
    Ok(())
}
