//! Actual56 upgrades and direct SQL authority checks; application normalization/hash verification is separate.
use hrrdarr::db::{Database, Error};
use libsql::{Connection, Value, params};
use std::path::{Path, PathBuf};
const MIGRATION: &str = include_str!("../migrations/0057_scan_media.sql");
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hrrdarr-scan-media-{}", uuid::Uuid::new_v4()));
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
async fn dump(c: &Connection, sql: &str) -> Result<Vec<Vec<Value>>, Error> {
    let mut rows = c.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(
            (0..r.column_count())
                .map(|i| r.get_value(i))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(out)
}
async fn predecessor(source: &Connection, path: &Path) -> Result<libsql::Database, Error> {
    let db = libsql::Builder::new_local(path).build().await?;
    let c = db.connect()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Raw historical builders must meet the real private-archive precondition too.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    c.execute_batch("PRAGMA foreign_keys=OFF;CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,name TEXT NOT NULL,checksum TEXT NOT NULL,sql TEXT NOT NULL,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
    let tx = c.transaction().await?;
    let mut rows=source.query("SELECT version,name,checksum,sql FROM schema_migrations WHERE version<=56 ORDER BY version",()).await?;
    let mut n = 0;
    while let Some(r) = rows.next().await? {
        let sql: String = r.get(3)?;
        tx.execute_batch(&sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
            params![
                r.get::<i64>(0)?,
                r.get::<String>(1)?,
                r.get::<String>(2)?,
                sql
            ],
        )
        .await?;
        n += 1;
    }
    assert_eq!(n, 56);
    tx.commit().await?;
    c.execute("PRAGMA foreign_keys=ON", ()).await?;
    Ok(db)
}
const CAP: i64 = 64 * 1024 * 1024;
const ENVELOPE: &str = "{\"version\":1}";
fn hash(raw: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, raw.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn measured(digest: &str) -> String {
    serde_json::json!({"version":1,"observation":{"version":1,"state":"measured","sha256":digest}})
        .to_string()
}
fn unavailable() -> String {
    serde_json::json!({"version":1,"observation":{"version":1,"state":"unavailable","reason":"probe_executable_missing"}}).to_string()
}
async fn library(c: &Connection) -> Result<(), Error> {
    c.execute_batch("INSERT INTO series(id,title,path,runtime)VALUES(1,'TV','/tv',0);INSERT INTO seasons(series_id,number)VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title)VALUES(1,1,1,1,'One');INSERT INTO movie_metadata(id,title)VALUES(1,'Film');INSERT INTO movies(id,metadata_id,path)VALUES(1,1,'/movies');").await?;
    Ok(())
}
async fn parent(c: &Connection) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO downloaded_scan_commands(id,media_type,source_path,requested_mode,ready_order_at,next_attempt_at,created_at)VALUES(?,'movies','/download','copy',100,100,100)",[id.clone()]).await?;
    c.execute("UPDATE downloaded_scan_commands SET status='running',attempts=1,run_generation=1,started_at=100 WHERE id=?",[id.clone()]).await?;
    Ok(id)
}
async fn child(
    c: &Connection,
    p: &str,
    ordinal: i64,
    decision: &str,
    selected: bool,
) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let name = format!("{ordinal:05}.mkv");
    if selected {
        c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,source_identity_json,source_dev,source_ino,decision_json,selected,movie_id,destination,resolved_mode,quality_id,revision_json)VALUES(?,?,?,?,?,'{\"version\":1,\"dev\":1,\"ino\":2}','1','2',?,1,1,'/movies/new.mkv','copy',1,'{\"version\":0,\"real\":0,\"is_repack\":false}')",params![id.clone(),p,ordinal,name.clone(),format!("/download/{name}"),decision]).await?;
    } else {
        c.execute("INSERT INTO downloaded_scan_children(id,command_id,ordinal,relative_path,source_path,decision_json,disposition)VALUES(?,?,?,?,?,?,'rejected')",params![id.clone(),p,ordinal,name.clone(),format!("/download/{name}"),decision]).await?;
    }
    Ok(id)
}
async fn media(c: &Connection, p: &str, ch: &str, raw: &str, digest: &str) -> Result<(), Error> {
    c.execute("INSERT INTO downloaded_scan_media_observations(child_id,command_id,media_info_json,sha256)VALUES(?,?,?,?)",params![ch,p,raw,digest]).await?;
    Ok(())
}
async fn bytes(c: &Connection, p: &str) -> Result<i64, Error> {
    Ok(c.query(
        "SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?",
        [p],
    )
    .await?
    .next()
    .await?
    .ok_or("missing inventory")?
    .get(0)?)
}
async fn publish(c: &Connection, p: &str) -> Result<(), Error> {
    c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json=?,inventory_count=(SELECT child_count FROM downloaded_scan_inventory_size WHERE id=?),inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?)+length(CAST(? AS BLOB))-coalesce(length(CAST(inventory_json AS BLOB)),0) WHERE id=?",params![ENVELOPE,p,p,ENVELOPE,p]).await?;
    Ok(())
}
async fn witness(c: &Connection) -> Result<Vec<Vec<Vec<Value>>>, Error> {
    let mut out = Vec::new();
    for table in [
        "series",
        "episodes",
        "movie_metadata",
        "movies",
        "downloaded_scan_commands",
        "downloaded_scan_children",
        "snapshot_imports",
        "import_policies",
        "health_checks",
    ] {
        // Reasoning: 0060 registers recycle_bin identities after this predecessor; preserved rows stay exact.
        let filter = if table == "health_checks" {
            " WHERE check_key!='recycle_bin'"
        } else {
            ""
        };
        // Reasoning: 0077/0078 append authority markers; compare every predecessor field.
        let columns = if table == "snapshot_imports" {
            "application,fingerprint,schema_version,imported_at,episode_metadata_version,history_version,profile_version,blocklist_version,custom_format_version,tag_version,revision_policy_version,delay_profile_version,release_profile_version,cdh_version,autotag_metadata_version,series_runtime_version,import_policy_version"
        } else {
            "*"
        };
        out.push(
            dump(
                c,
                &format!("SELECT {columns} FROM {table}{filter} ORDER BY rowid"),
            )
            .await?,
        );
    }
    Ok(out)
}

#[tokio::test]
async fn schema57_actual56_preserves_published_rows_rollback_backup_and_reopen() -> Result<(), Error>
{
    let s = Scratch::new();
    let fresh = Database::open_local(s.0.join("fresh.db")).await?;
    let fc = fresh.connect().await?;
    assert_eq!(
        scalar(&fc, "SELECT max(version) FROM schema_migrations").await?,
        78 // Reasoning: migrations 0077/0078 append collection/provider authority; historical prefixes stay fixed.
    );
    let path = s.0.join("old.db");
    let old = predecessor(&fc, &path).await?;
    let c = old.connect()?;
    library(&c).await?;
    c.execute("INSERT INTO snapshot_imports(application,fingerprint,schema_version)VALUES('sonarr','kept',233)",()).await?;
    let p = parent(&c).await?;
    // Actual56 accepted selected children without an observation descriptor. Keep that historical fact exact.
    child(&c, &p, 0, ENVELOPE, true).await?;
    c.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json=?,inventory_count=1,inventory_bytes=(SELECT bytes FROM downloaded_scan_inventory_size WHERE id=?) WHERE id=?",params![ENVELOPE,p.clone(),p.clone()]).await?;
    let before = witness(&c).await?;
    let schema = dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?;
    let original_transition: String = c
        .query(
            "SELECT sql FROM sqlite_schema WHERE name='downloaded_scan_transition'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    let tx = c.transaction().await?;
    tx.execute_batch(MIGRATION).await?;
    assert!(tx.execute("INSERT INTO downloaded_scan_media_observations(child_id,command_id,media_info_json,sha256)VALUES('missing','missing','{}','bad')",()).await.is_err());
    tx.rollback().await?;
    assert_eq!(
        schema,
        dump(&c, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(before, witness(&c).await?);
    drop(c);
    drop(old);
    let db = Database::open_local(&path).await?;
    let backup = db.migration_backup().ok_or("missing backup")?;
    let copied = s.0.join("backup-copy.db");
    std::fs::copy(backup.join("database.db"), &copied)?;
    let bdb = libsql::Builder::new_local(&copied).build().await?;
    let bc = bdb.connect()?;
    assert_eq!(
        schema,
        dump(&bc, "SELECT type,name,sql FROM sqlite_schema ORDER BY name").await?
    );
    assert_eq!(before, witness(&bc).await?);
    let c = db.connect().await?;
    assert_eq!(before, witness(&c).await?);
    assert_eq!(
        scalar(
            &c,
            "SELECT count(*) FROM downloaded_scan_media_observations"
        )
        .await?,
        0
    );
    let transition: String = c
        .query(
            "SELECT sql FROM sqlite_schema WHERE name='downloaded_scan_transition'",
            (),
        )
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?;
    let delta = "+coalesce(length(CAST(NEW.inventory_json AS BLOB)),0)-coalesce(length(CAST(OLD.inventory_json AS BLOB)),0)";
    assert_eq!(transition.matches(delta).count(), 2);
    assert_eq!(
        transition.replace(delta, ""),
        original_transition,
        "only pending-envelope accounting changes the original transition authority"
    );
    // The immutable historical publication count stays exact; current retained bytes now include its envelope.
    let old_bytes = scalar(&c, "SELECT inventory_bytes FROM downloaded_scan_commands").await?;
    assert_eq!(bytes(&c, &p).await?, old_bytes + ENVELOPE.len() as i64);
    c.execute("UPDATE downloaded_scan_commands SET run_generation=run_generation+1,ready_order_at=101 WHERE id=?",[p]).await?;
    assert!(
        c.query("PRAGMA foreign_key_check", ())
            .await?
            .next()
            .await?
            .is_none()
    );
    let after = witness(&c).await?;
    drop(c);
    drop(db);
    let reopened = Database::open_local(&path).await?;
    assert!(reopened.migration_backup().is_none());
    assert_eq!(after, witness(&reopened.connect().await?).await?);
    Ok(())
}

#[tokio::test]
async fn scan_media_full_metadata_ownership_descriptor_and_publication_constraints()
-> Result<(), Error> {
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let p = parent(&c).await?;
    // Full per-stream metadata exceeds16KiB; these are schema-only storage witnesses.
    let raw=serde_json::json!({"audioStreams":(0..20).map(|_|serde_json::json!({"profile":"x".repeat(1000),"format":"AAC"})).collect::<Vec<_>>()}).to_string();
    assert!(raw.len() > 16384 && raw.len() <= 65536);
    let digest = hash(&raw);
    let ch = child(&c, &p, 0, &measured(&digest), true).await?;
    assert!(
        publish(&c, &p).await.is_err(),
        "measured child cannot publish without its metadata"
    );
    let other = parent(&c).await?;
    assert!(media(&c, &other, &ch, &raw, &digest).await.is_err());
    assert!(media(&c, &p, &ch, &raw, &"a".repeat(64)).await.is_err());
    for invalid in ["null", "[]", "\"string\"", "{", ""] {
        assert!(
            media(&c, &p, &ch, invalid, &digest).await.is_err(),
            "{invalid}"
        );
    }
    assert!(
        media(
            &c,
            &p,
            &ch,
            &format!("{{\"x\":\"{}\"}}", "x".repeat(65536)),
            &digest
        )
        .await
        .is_err()
    );
    media(&c, &p, &ch, &raw, &digest).await?;
    assert!(
        media(&c, &p, &ch, &raw, &digest).await.is_err(),
        "one observation per child"
    );
    assert!(
        c.execute(
            "UPDATE downloaded_scan_media_observations SET media_info_json='{}' WHERE child_id=?",
            [ch.clone()]
        )
        .await
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM downloaded_scan_media_observations WHERE child_id=?",
            [ch.clone()]
        )
        .await
        .is_err()
    );
    assert!(c.execute("INSERT INTO downloaded_scan_media_observations(child_id,command_id,media_info_json,sha256,observation_bytes)VALUES('x','y','{}',?,0)",[digest.clone()]).await.is_err());
    let unselected = child(&c, &p, 1, ENVELOPE, false).await?;
    assert!(
        media(&c, &p, &unselected, "{}", &hash("{}")).await.is_err(),
        "unexpected metadata on unprobed child"
    );
    publish(&c, &p).await?;
    let recorded = scalar(
        &c,
        "SELECT inventory_bytes FROM downloaded_scan_commands WHERE inventory_revision=1",
    )
    .await?;
    assert_eq!(recorded, bytes(&c, &p).await?);
    let observed:Vec<u8>=c.query("SELECT CAST(media_info_json AS BLOB) FROM downloaded_scan_media_observations WHERE child_id=?",[ch.clone()]).await?.next().await?.unwrap().get(0)?;
    assert_eq!(observed, raw.as_bytes());
    assert_eq!(
        scalar(
            &c,
            "SELECT observation_bytes FROM downloaded_scan_media_observations"
        )
        .await?,
        raw.len() as i64 + 192
    );
    // Even a previously admitted, missing measured row cannot be filled after publication.
    assert!(media(&c, &p, &unselected, "{}", &hash("{}")).await.is_err());
    // Release the original ready reservation without pretending that a transfer occurred.
    c.execute(
        "UPDATE downloaded_scan_children SET disposition='cancelled' WHERE id=?",
        [ch],
    )
    .await?;
    c.execute("UPDATE downloaded_scan_commands SET status='cancelled',cancel_requested=1,completed_at=101 WHERE id=?",[p]).await?;
    for invalid in [
        serde_json::json!({"version":1}),
        serde_json::json!({"version":1,"observation":null}),
        serde_json::json!({"version":1,"observation":{"state":"unavailable","reason":"probe_failed"}}),
        serde_json::json!({"version":1,"observation":{"version":null,"state":"unavailable","reason":"probe_failed"}}),
        serde_json::json!({"version":1,"observation":{"version":true,"state":"unavailable","reason":"probe_failed"}}),
        serde_json::json!({"version":1,"observation":{"version":1.0,"state":"unavailable","reason":"probe_failed"}}),
        serde_json::json!({"version":1,"observation":{"version":1,"state":"unavailable","reason":"invented"}}),
        serde_json::json!({"version":1,"observation":{"version":1,"state":"unavailable","reason":"probe_failed","sha256":null}}),
        serde_json::json!({"version":1,"observation":{"version":1,"state":"measured","sha256":"a".repeat(64),"reason":null}}),
        serde_json::json!({"version":1,"observation":{"version":1,"state":"measured","sha256":"A".repeat(64)}}),
        serde_json::json!({"version":1,"observation":{"version":1,"state":"measured","sha256":null}}),
    ] {
        // Use the same unpublished parent: rejected INSERTs cannot leave child reservations behind.
        let error = child(&c, &other, 0, &invalid.to_string(), true)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid scan observation descriptor"),
            "unexpected descriptor rejection: {error}"
        );
        assert_eq!(
            c.query(
                "SELECT count(*) FROM downloaded_scan_children WHERE command_id=?",
                [other.clone()]
            )
            .await?
            .next()
            .await?
            .unwrap()
            .get::<i64>(0)?,
            0
        );
    }
    let unavailable_child = child(&c, &other, 0, &unavailable(), true).await?;
    assert!(
        media(&c, &other, &unavailable_child, "{}", &hash("{}"))
            .await
            .is_err()
    );
    publish(&c, &other).await?;
    let late = parent(&c).await?;
    let late_child = child(&c, &late, 0, &measured(&hash("{}")), false).await?;
    c.execute(
        "UPDATE downloaded_scan_commands SET cancel_requested=1 WHERE id=?",
        [late.clone()],
    )
    .await?;
    assert!(
        media(&c, &late, &late_child, "{}", &hash("{}"))
            .await
            .is_err()
    );
    let mut plans=c.query("EXPLAIN QUERY PLAN SELECT sum(observation_bytes) FROM downloaded_scan_media_observations WHERE command_id=?",[other]).await?;
    let mut covered = false;
    while let Some(row) = plans.next().await? {
        covered |= row
            .get::<String>(3)?
            .contains("INDEX downloaded_scan_media_bytes");
    }
    assert!(covered);
    Ok(())
}

fn padded_json(size: usize) -> String {
    let overhead = "{\"padding\":\"\"}".len();
    assert!(size >= overhead);
    serde_json::json!({"padding":"x".repeat(size-overhead)}).to_string()
}
#[tokio::test]
async fn scan_media_aggregate_exact_boundary_envelope_and_mutation_accounting() -> Result<(), Error>
{
    let s = Scratch::new();
    let db = Database::open_local(s.0.join("db")).await?;
    let c = db.connect().await?;
    library(&c).await?;
    let p = parent(&c).await?;
    let mutable = child(&c, &p, 19999, &unavailable(), true).await?;
    let tx = c.transaction().await?;
    let raw = padded_json(65536);
    let digest = hash(&raw);
    let decision = measured(&digest);
    let mut accepted = 0_i64;
    let mut rejected = false;
    // Real64KiB payloads exercise indexed aggregate accounting, not caller-provided counters.
    for ordinal in 0..1100 {
        tx.execute("SAVEPOINT item", ()).await?;
        let ch = child(&tx, &p, ordinal, &decision, false).await?;
        match media(&tx, &p, &ch, &raw, &digest).await {
            Ok(()) => {
                tx.execute("RELEASE item", ()).await?;
                accepted += 1;
            }
            Err(error) => {
                assert!(
                    error
                        .to_string()
                        .contains("scan inventory byte capacity reached"),
                    "unexpected media failure: {error}"
                );
                tx.execute_batch("ROLLBACK TO item; RELEASE item;").await?;
                rejected = true;
                break;
            }
        }
    }
    assert!(rejected && accepted > 1000 && accepted < 1100);
    let used = bytes(&tx, &p).await?;
    let remainder = (CAP - used) as usize;
    // The pending child's paths and proof have fixed lengths; compute its exact retained-byte cost.
    let fixed_child = 2 * "00000.mkv".len() + "/download/".len() + decision.len() + 2 + 128;
    let tail_size = remainder - fixed_child - 192 - ENVELOPE.len();
    assert!(tail_size > 0 && tail_size <= 65536);
    let tail = padded_json(tail_size);
    let tail_digest = hash(&tail);
    let ch = child(&tx, &p, accepted, &measured(&tail_digest), false).await?;
    media(&tx, &p, &ch, &tail, &tail_digest).await?;
    assert_eq!(bytes(&tx, &p).await?, CAP - ENVELOPE.len() as i64);
    // A14-byte envelope exceeds the cap by exactly one byte, without changing publication state.
    let too_large = "{\"version\": 1}";
    let error=tx.execute("UPDATE downloaded_scan_commands SET inventory_revision=1,inventory_json=?,inventory_count=(SELECT child_count FROM downloaded_scan_inventory_size WHERE id=?),inventory_bytes=67108864 WHERE id=?",params![too_large,p.clone(),p.clone()]).await.unwrap_err();
    assert!(error.to_string().contains("scan inventory"));
    publish(&tx, &p).await?;
    assert_eq!(bytes(&tx, &p).await?, CAP);
    assert_eq!(
        scalar(&tx, "SELECT inventory_bytes FROM downloaded_scan_commands").await?,
        CAP
    );
    assert!(
        tx.execute(
            "UPDATE downloaded_scan_children SET reasons_json='[\"x\"]' WHERE id=?",
            [mutable]
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("scan inventory byte capacity reached")
    );
    assert!(
        tx.execute(
            "UPDATE downloaded_scan_commands SET inventory_bytes=inventory_bytes-1 WHERE id=?",
            [p.clone()]
        )
        .await
        .is_err()
    );
    tx.commit().await?;
    assert_eq!(bytes(&c, &p).await?, CAP);
    Ok(())
}
