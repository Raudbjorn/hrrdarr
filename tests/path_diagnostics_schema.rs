//! Schema53 proof uses the actual migration runner and an exact replay of its52-row prefix.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, TransactionBehavior, params};
use std::path::{Path, PathBuf};

const MIGRATION: &str = include_str!("../migrations/0053_path_diagnostics.sql");
const JOURNAL_QUERY: &str = "SELECT j.operation_id FROM import_journal j JOIN operations o ON o.id=j.operation_id WHERE j.phase!='complete' AND (j.path_failure_code IS NOT NULL OR j.error_code IS NOT NULL OR j.phase!='preview') AND o.media_type=? ORDER BY j.operation_id LIMIT 1025";
const RECEIPT_QUERY: &str = "SELECT d.candidate_id FROM download_processing d JOIN rss_candidates r ON r.id=d.candidate_id WHERE d.status NOT IN ('imported','cancelled') AND r.media_type=? ORDER BY d.candidate_id LIMIT 1025";

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("hrrdarr-path-schema-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
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
    while let Some(row) = rows.next().await? {
        let mut values = Vec::new();
        for column in 0..row.column_count() {
            values.push(format!("{:?}", row.get_value(column)?));
        }
        out.push(values.join("|"));
    }
    out.sort();
    Ok(out)
}
// Capture actual predecessor columns so later additive fields/tables cannot distort
// row preservation. Rollback witnesses still capture every table in their schema.
async fn data_queries(c: &Connection) -> Result<Vec<(String, String)>, Error> {
    let mut tables = c.query("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name", ()).await?;
    let mut out = Vec::new();
    while let Some(row) = tables.next().await? {
        let name: String = row.get(0)?;
        let table = format!("\"{}\"", name.replace('"', "\"\""));
        let mut columns = c.query(&format!("PRAGMA table_info({table})"), ()).await?;
        let mut projection = Vec::new();
        while let Some(column) = columns.next().await? {
            let column: String = column.get(1)?;
            if name == "import_journal" && column == "path_failure_code" {
                continue;
            }
            projection.push(format!("\"{}\"", column.replace('"', "\"\"")));
        }
        let filter = match name.as_str() {
            "schema_migrations" => " WHERE version<=52",
            // Reasoning: 0060 recycle_bin identities postdate this predecessor like 0053's own rows.
            // Reasoning: 0080 registers root_folder, collection_root_folder and mount_read_only after this predecessor; they are excluded like other later registrations.
            "health_checks" => {
                " WHERE check_key NOT IN ('remote_path_mapping','recycle_bin','root_folder','collection_root_folder','mount_read_only')"
            }
            _ => "",
        };
        out.push((
            name,
            format!("SELECT {} FROM {table}{filter}", projection.join(",")),
        ));
    }
    Ok(out)
}
async fn data_with(
    c: &Connection,
    queries: &[(String, String)],
) -> Result<Vec<(String, Vec<String>)>, Error> {
    let mut out = Vec::new();
    for (name, query) in queries {
        out.push((name.clone(), dump(c, query).await?));
    }
    Ok(out)
}
async fn data(c: &Connection) -> Result<Vec<(String, Vec<String>)>, Error> {
    data_with(c, &data_queries(c).await?).await
}
async fn schema(c: &Connection, additive_projection: bool) -> Result<Vec<String>, Error> {
    let filter = if additive_projection {
        " WHERE name NOT IN ('import_journal','import_journal_path_unresolved','download_processing_path_unresolved')"
    } else {
        ""
    };
    dump(
        c,
        &format!("SELECT type,name,tbl_name,sql FROM sqlite_schema{filter}"),
    )
    .await
}
async fn predecessor(source: &Connection, path: &Path) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    //52 rebuilds tables; mirror the real runner's isolated FK suspension and final integrity check.
    c.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut rows = source.query("SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=52 ORDER BY version", ()).await?;
    let mut applied = 0;
    while let Some(row) = rows.next().await? {
        let version: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let checksum: String = row.get(2)?;
        let sql: String = row.get(3)?;
        tx.execute_batch(&sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
            params![version, name, checksum, sql],
        )
        .await?;
        applied += 1;
    }
    assert_eq!(applied, 52);
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
        c.execute("UPDATE download_processing SET status='blocked',error_code='source_unavailable',reasons_json='[\"source_unavailable\"]' WHERE candidate_id=?", [candidate]).await?;
    }
    Ok(())
}

#[tokio::test]
async fn schema53_fresh_actual52_upgrade_backup_and_reopen_preserve_existing_state()
-> Result<(), Error> {
    let scratch = Scratch::new();
    let fresh = Database::open_local(scratch.0.join("fresh")).await?;
    let c = fresh.connect().await?;
    assert_eq!(
        scalar(&c, "SELECT max(version) FROM schema_migrations").await?,
        81 // Reasoning: main migrations 1-78 stay fixed; Wanted 0079, library-root health 0080 and Backup 0081 append three entries.
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks WHERE check_key='remote_path_mapping' AND startup=1 AND scheduled=1 AND compatibility_type='RemotePathMappingCheck' AND generation=0 AND pending_reasons=0 AND due_at IS NULL AND severity IS NULL AND observed_generation IS NULL AND checked_at IS NULL").await?, 2);
    let path = scratch.0.join("upgrade");
    let old = predecessor(&c, &path).await?;
    let before = old.connect()?;
    seed(&before).await?;
    let queries = data_queries(&before).await?;
    let rows = data_with(&before, &queries).await?;
    let unchanged_schema = schema(&before, true).await?;
    let original_schema = schema(&before, false).await?;
    // Keep the exact 0053-only SQL preservation assertion, before later producer
    // migrations deliberately rebuild processing and extend admission predicates.
    let tx = before.transaction().await?;
    tx.execute_batch(MIGRATION).await?;
    assert_eq!(data_with(&tx, &queries).await?, rows);
    assert_eq!(schema(&tx, true).await?, unchanged_schema);
    tx.rollback().await?;
    assert_eq!(schema(&before, false).await?, original_schema);
    drop(before);
    drop(old);
    let upgraded = Database::open_local(&path).await?;
    let backup = upgraded
        .migration_backup()
        .ok_or("missing upgrade backup")?
        .to_path_buf();
    let after = upgraded.connect().await?;
    assert_eq!(
        scalar(&after, "SELECT max(version) FROM schema_migrations").await?,
        81 // Reasoning: main migrations 1-78 stay fixed; Wanted 0079, library-root health 0080 and Backup 0081 append three entries.
    );
    assert_eq!(data_with(&after, &queries).await?, rows);
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM series WHERE runtime IS NOT NULL"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM snapshot_imports WHERE series_runtime_version!=0"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM download_processing WHERE scan_command_id IS NOT NULL"
        )
        .await?,
        0
    );
    assert_eq!(
        scalar(
            &after,
            "SELECT count(*) FROM import_journal WHERE path_failure_code IS NULL"
        )
        .await?,
        3
    );
    // migration_backup identifies the recovery directory, which also contains its manifest.
    let copy = libsql::Builder::new_local(backup.join("database.db"))
        .build()
        .await?;
    let archived = copy.connect()?;
    assert_eq!(data(&archived).await?, rows);
    assert_eq!(schema(&archived, false).await?, original_schema);
    assert_eq!(
        scalar(&archived, "SELECT max(version) FROM schema_migrations").await?,
        52
    );
    let committed_schema = schema(&after, false).await?;
    drop(after);
    drop(upgraded);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(data_with(&reopened.connect().await?, &queries).await?, rows);
    assert_eq!(
        schema(&reopened.connect().await?, false).await?,
        committed_schema
    );
    Ok(())
}

#[tokio::test]
async fn schema53_closed_codes_and_bounded_raw_byte_read_boundary() -> Result<(), Error> {
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    let mut codes = vec!["unresolved_attempt".to_owned()];
    for role in ["source", "root", "destination", "stage", "retirement"] {
        for kind in ["missing", "denied", "changed", "invalid", "io"] {
            for boundary in ["precommit", "cleanup"] {
                if role != "retirement" || boundary == "cleanup" {
                    codes.push(format!("{role}_{kind}_{boundary}"));
                }
            }
        }
    }
    assert_eq!(codes.len(), 46);
    for code in &codes {
        c.execute(
            "UPDATE import_journal SET path_failure_code=? WHERE operation_id='tv'",
            [code.clone()],
        )
        .await?;
        let row = c.query("SELECT CAST(path_failure_code AS BLOB) FROM import_journal WHERE operation_id='tv'", ()).await?.next().await?.unwrap();
        assert_eq!(row.get::<Vec<u8>>(0)?, code.as_bytes());
    }
    for code in [
        "",
        "source_unknown_precommit",
        "retirement_missing_precommit",
        "source_missing_prepare",
        "hc021_unresolved_attempt",
        "SOURCE_MISSING_PRECOMMIT",
        "unresolved_attempt\0suffix",
    ] {
        assert!(
            c.execute(
                "UPDATE import_journal SET path_failure_code=? WHERE operation_id='tv'",
                [code]
            )
            .await
            .is_err(),
            "accepted {code:?}"
        );
    }
    assert!(
        c.execute(
            "UPDATE import_journal SET path_failure_code=? WHERE operation_id='tv'",
            ["x".repeat(65)]
        )
        .await
        .is_err()
    );
    for value in [
        libsql::Value::Blob(b"unresolved_attempt".to_vec()),
        libsql::Value::Integer(1),
        libsql::Value::Real(1.5),
    ] {
        assert!(
            c.execute(
                "UPDATE import_journal SET path_failure_code=? WHERE operation_id='tv'",
                params![value]
            )
            .await
            .is_err()
        );
    }
    c.execute(
        "UPDATE import_journal SET path_failure_code=NULL WHERE operation_id='tv'",
        (),
    )
    .await?;
    // Controlled corruption bypasses constraints only inside this rolled-back transaction.
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    tx.execute("PRAGMA ignore_check_constraints=ON", ()).await?;
    tx.execute("UPDATE import_journal SET path_failure_code=CAST(x'ff0061' AS TEXT) WHERE operation_id='tv'", ()).await?;
    let row = tx.query("SELECT typeof(path_failure_code),length(CAST(path_failure_code AS BLOB)),substr(CAST(path_failure_code AS BLOB),1,65) FROM import_journal WHERE operation_id='tv'", ()).await?.next().await?.unwrap();
    assert_eq!(row.get::<String>(0)?, "text");
    assert_eq!(row.get::<i64>(1)?, 3);
    let bytes: Vec<u8> = row.get(2)?;
    assert_eq!(bytes, [255, 0, 97]);
    assert!(std::str::from_utf8(&bytes).is_err());
    tx.execute("UPDATE import_journal SET path_failure_code=CAST(zeroblob(1048576) AS TEXT) WHERE operation_id='tv'", ()).await?;
    let row = tx.query("SELECT length(CAST(path_failure_code AS BLOB)),substr(CAST(path_failure_code AS BLOB),1,65) FROM import_journal WHERE operation_id='tv'", ()).await?.next().await?.unwrap();
    assert_eq!(row.get::<i64>(0)?, 1048576);
    assert_eq!(
        row.get::<Vec<u8>>(1)?.len(),
        65,
        "SQL bounds the bytes returned before decoding"
    );
    tx.rollback().await?;
    c.execute("PRAGMA ignore_check_constraints=OFF", ()).await?;
    Ok(())
}

#[tokio::test]
async fn schema53_registry_capacity_and_history_failure_roll_back_every_change() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let fresh = Database::open_local(scratch.0.join("fresh")).await?;
    let source = fresh.connect().await?;
    for fail_history in [false, true] {
        let path = scratch
            .0
            .join(if fail_history { "history" } else { "capacity" });
        let old = predecessor(&source, &path).await?;
        let c = old.connect()?;
        seed(&c).await?;
        if fail_history {
            c.execute_batch("CREATE TRIGGER reject_schema53 BEFORE INSERT ON schema_migrations WHEN NEW.version=53 BEGIN SELECT RAISE(ABORT,'injected history failure'); END;").await?;
        } else {
            let count = scalar(&c, "SELECT count(*) FROM health_checks").await?;
            for id in count..127 {
                c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,compatibility_type) VALUES('system',?,0,0,'Fixture')", [format!("padding_{id}")]).await?;
            }
            assert_eq!(scalar(&c, "SELECT count(*) FROM health_checks").await?, 127);
        }
        let before_schema = schema(&c, false).await?;
        let before_data = data(&c).await?;
        // Explicit rollback proves DDL and the first of two registry inserts are transactional.
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let applied = tx.execute_batch(MIGRATION).await;
        if fail_history {
            applied?;
            assert!(tx.execute("INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(53,'path_diagnostics','fixture',?)", [MIGRATION]).await.is_err());
        } else {
            assert!(applied.is_err());
        }
        tx.rollback().await?;
        assert_eq!(schema(&c, false).await?, before_schema);
        assert_eq!(data(&c).await?, before_data);
        drop(c);
        drop(old);
        assert!(Database::open_local(&path).await.is_err());
        let unchanged = libsql::Builder::new_local(&path).build().await?;
        let c = unchanged.connect()?;
        assert_eq!(schema(&c, false).await?, before_schema);
        assert_eq!(data(&c).await?, before_data);
        let mut backups = 0;
        for entry in std::fs::read_dir(&scratch.0)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".hrrdarr-migration-")
            {
                let archived = libsql::Builder::new_local(entry.path().join("database.db"))
                    .build()
                    .await?;
                let archived = archived.connect()?;
                if schema(&archived, false).await? == before_schema {
                    assert_eq!(data(&archived).await?, before_data);
                    backups += 1;
                }
            }
        }
        assert_eq!(
            backups, 1,
            "the rejected migration must leave one exact recovery backup"
        );
    }
    Ok(())
}

#[tokio::test]
async fn schema53_partial_indexes_select_scoped_unresolved_rows_and_overflow() -> Result<(), Error>
{
    let scratch = Scratch::new();
    let db = Database::open_local(scratch.0.join("db")).await?;
    let c = db.connect().await?;
    seed(&c).await?;
    // These reduced queries prove index predicates and scoped bounds only; the Health module
    // verifies the complete production queries with their applicability and handoff joins.
    for (sql, media, index) in [
        (JOURNAL_QUERY, "episode", "import_journal_path_unresolved"),
        (RECEIPT_QUERY, "tv", "download_processing_path_unresolved"),
    ] {
        let mut plan = c
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), [media])
            .await?;
        let mut detail = String::new();
        while let Some(row) = plan.next().await? {
            detail.push_str(&row.get::<String>(3)?);
        }
        assert!(
            detail.contains(index),
            "planner did not select {index}: {detail}"
        );
        assert!(c.query(sql, [media]).await?.next().await?.is_some());
    }
    // Large opposite-domain unfinished set cannot consume the requested domain's limit.
    c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2048) INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message) SELECT 'bulk-'||printf('%04d',i),'movie',1,'/source/'||i,'copy','/movies/one/'||i,'staging','' FROM n;
      INSERT INTO import_journal(operation_id,plan_json,phase,path_failure_code) SELECT id,'{}','staging','unresolved_attempt' FROM operations WHERE id LIKE 'bulk-%';").await?;
    c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2048) INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message) SELECT 'done-'||printf('%04d',i),'movie',2,'/source/done','copy','/movies/completed/a.mkv','complete','' FROM n;
      INSERT INTO import_journal(operation_id,plan_json,stage_json,phase) SELECT id,'{}','{}','staging' FROM operations WHERE id LIKE 'done-%';
      INSERT INTO import_history(operation_id,media_type,movie_id,movie_file_id,source,destination,size,sha256) SELECT id,'movie',2,2,source,destination,1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' FROM operations WHERE id LIKE 'done-%';
      UPDATE import_journal SET phase='complete' WHERE operation_id LIKE 'done-%';").await?;
    assert_eq!(scalar(&c, "SELECT count(*) FROM import_journal INDEXED BY import_journal_path_unresolved WHERE phase!='complete' AND (path_failure_code IS NOT NULL OR error_code IS NOT NULL OR phase!='preview')").await?, 2050, "completed history is absent from the partial index");
    let mut tv = c.query(JOURNAL_QUERY, ["episode"]).await?;
    assert_eq!(tv.next().await?.unwrap().get::<String>(0)?, "tv");
    assert!(tv.next().await?.is_none());
    let mut movies = c.query(JOURNAL_QUERY, ["movie"]).await?;
    let mut count = 0;
    while movies.next().await?.is_some() {
        count += 1;
    }
    assert_eq!(
        count, 1025,
        "the evaluator must see the overflow row, not a truncated healthy set"
    );
    // Clearing a control error on retry does not hide a retained reserved marker.
    c.execute("UPDATE download_processing SET status='queued',preflight_attempts=0,error_code=NULL,reasons_json='[\"hc021_unresolved_attempt\"]' WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')", ()).await?;
    assert!(
        c.query(RECEIPT_QUERY, ["tv"])
            .await?
            .next()
            .await?
            .is_some()
    );
    c.execute("UPDATE download_processing SET status='cancelled' WHERE candidate_id=(SELECT id FROM rss_candidates WHERE media_type='tv')", ()).await?;
    assert!(
        c.query(RECEIPT_QUERY, ["tv"])
            .await?
            .next()
            .await?
            .is_none()
    );
    assert_eq!(scalar(&c, "SELECT count(*) FROM download_processing WHERE status='cancelled' AND reasons_json='[\"hc021_unresolved_attempt\"]'").await?, 1);
    Ok(())
}
