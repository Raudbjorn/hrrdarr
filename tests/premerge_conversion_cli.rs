//! `hrrdarr convert-premerge-pr32`: the explicit offline conversion of pre-merge PR #32 databases (#47)
//! through the real binary. Row-level mapping is covered by the library tests; this pins the
//! operator contract: it converts, never overwrites, never touches the source, and reports.
use hrrdarr::db::{Database, Error, premerge_ledger};
use libsql::{TransactionBehavior, params};
use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn checksum(sql: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, sql.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn operator_converts_once_and_never_overwrites() -> Result<(), Error> {
    let dir = std::env::temp_dir().join(format!("hrrdarr-convert-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir)?;
    let scratch = Scratch(dir.clone());
    let (source, destination) = (dir.join("old.db"), dir.join("new.db"));
    {
        let raw = libsql::Builder::new_local(&source).build().await?;
        let c = raw.connect()?;
        c.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, sql TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        for (i, (name, sql)) in premerge_ledger().into_iter().enumerate() {
            tx.execute_batch(sql).await?;
            tx.execute(
                "INSERT INTO schema_migrations(version,name,checksum,sql) VALUES(?,?,?,?)",
                params![i as i64 + 1, name, checksum(sql), sql],
            )
            .await?;
        }
        tx.commit().await?;
        c.execute(
            "INSERT INTO quality_profiles(id,media_type,name) VALUES(60,'movies','Kept')",
            (),
        )
        .await?;
    }
    let before = std::fs::read(&source)?;
    let run = |args: &[&PathBuf]| {
        Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
            .arg("convert-premerge-pr32")
            .args(args)
            .output()
    };

    let output = run(&[&source, &destination])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["event"], "premerge_pr32_converted");
    assert_eq!(report["report"]["source_version"], 76);
    assert_eq!(
        std::fs::read(&source)?,
        before,
        "the source is never modified"
    );

    let db = Database::open_local(&destination).await?;
    assert!(db.migration_backup().is_none());
    let c = db.connect().await?;
    let name: String = c
        .query("SELECT name FROM quality_profiles WHERE id=60", ())
        .await?
        .next()
        .await?
        .ok_or("profile lost")?
        .get(0)?;
    assert_eq!(name, "Kept");
    drop((c, db));

    let again = run(&[&source, &destination])?;
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("never overwrites"));
    let usage = Command::new(env!("CARGO_BIN_EXE_hrrdarr"))
        .arg("convert-premerge-pr32")
        .output()?;
    assert!(!usage.status.success());
    drop(scratch);
    Ok(())
}
