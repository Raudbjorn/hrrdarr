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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        // Unix path bytes are passed intact to conversion; only JSON display is lossy.
        let raw_source = dir.join(std::ffi::OsString::from_vec(b"old-\xff.db".to_vec()));
        let raw_destination = dir.join(std::ffi::OsString::from_vec(b"new-\xfe.db".to_vec()));
        std::fs::copy(&source, &raw_source)?;
        let output = run(&[&raw_source, &raw_destination])?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(report["source"], raw_source.to_string_lossy().as_ref());
        assert_eq!(
            report["destination"],
            raw_destination.to_string_lossy().as_ref()
        );
        assert_eq!(std::fs::read(&raw_source)?, before);
        let raw_parent = dir.join(std::ffi::OsString::from_vec(b"directory-\xff".to_vec()));
        std::fs::create_dir(&raw_parent)?;
        let unsupported = raw_parent.join("new.db");
        let rejected = run(&[&source, &unsupported])?;
        // The CLI accepts Unix bytes, but libSQL rejects non-UTF-8 working database paths.
        // Keep that limitation explicit: report a conversion error, never panic or publish.
        assert!(!rejected.status.success());
        let error = String::from_utf8_lossy(&rejected.stderr);
        assert!(error.contains("premerge_pr32_conversion_failed"), "{error}");
        assert!(!error.contains("panicked"), "{error}");
        assert!(!unsupported.exists());
        assert_eq!(std::fs::read_dir(&raw_parent)?.count(), 0);
        assert_eq!(std::fs::read(&source)?, before);
        // libSQL requires UTF-8 paths, so inspect a byte-identical copy under an ASCII filename.
        let inspect = dir.join("inspect.db");
        std::fs::copy(&raw_destination, &inspect)?;
        let db = Database::open_local(&inspect).await?;
        let c = db.connect().await?;
        assert_eq!(
            c.query("SELECT name FROM quality_profiles WHERE id=60", ())
                .await?
                .next()
                .await?
                .ok_or("profile lost at non-UTF-8 destination")?
                .get::<String>(0)?,
            "Kept"
        );
    }
    drop(scratch);
    Ok(())
}
