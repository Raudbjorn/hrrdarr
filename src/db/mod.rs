//! Local, single-worker schema ownership. Remote schema changes require a verified backup strategy.
use libsql::{Connection, TransactionBehavior, params};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "media_type", content = "id", rename_all = "snake_case")]
pub enum MediaTarget {
    Episode(i64),
    Movie(i64),
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

const MIGRATIONS: &[(&str, &str)] = &[
    (
        "prototype",
        include_str!("../../migrations/0001_prototype.sql"),
    ),
    (
        "media_relations",
        include_str!("../../migrations/0002_media_relations.sql"),
    ),
    (
        "snapshot_imports",
        include_str!("../../migrations/0003_snapshot_imports.sql"),
    ),
];
const HISTORY_SQL: &str = "CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL,
    sql TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)";

pub struct Database {
    inner: libsql::Database,
    // Advisory ownership lasts for the application lifetime, including backup/migration.
    _owner: Option<File>,
    backup: Option<PathBuf>,
}

impl Database {
    pub async fn open_local(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() || path == Path::new(":memory:") {
            return Err("application databases require a persistent local path".into());
        }
        let lock_path = path.clone();
        let owner = tokio::task::spawn_blocking(move || -> Result<File, Error> {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(lock_path)?;
            file.try_lock()
                .map_err(|e| format!("database already owned or cannot be locked: {e}"))?;
            Ok(file)
        })
        .await??;
        let inner = libsql::Builder::new_local(&path).build().await?;
        let mut db = Self {
            inner,
            _owner: Some(owner),
            backup: None,
        };
        let conn = db.connect().await?;
        db.backup = migrate(&conn, Some(&path)).await?;
        Ok(db)
    }

    pub async fn open_remote(url: String, token: String) -> Result<Self, Error> {
        let inner = libsql::Builder::new_remote(url, token).build().await?;
        let db = Self {
            inner,
            _owner: None,
            backup: None,
        };
        migrate(&db.connect().await?, None).await?;
        Ok(db)
    }

    pub async fn connect(&self) -> Result<Connection, libsql::Error> {
        let conn = self.inner.connect()?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute("PRAGMA foreign_keys = ON", ()).await?;
        if scalar(&conn, "PRAGMA foreign_keys").await? != 1 {
            return Err(libsql::Error::Misuse(
                "foreign key enforcement unavailable".into(),
            ));
        }
        Ok(conn)
    }

    /// Source archives can contain credentials. Remote and public local databases are not
    /// accepted until they have an explicit private storage policy.
    pub fn permits_private_snapshots(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            self._owner
                .as_ref()
                .and_then(|f| f.metadata().ok())
                .is_some_and(|m| m.permissions().mode() & 0o077 == 0)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub fn migration_backup(&self) -> Option<&Path> {
        self.backup.as_deref()
    }
}

async fn scalar(conn: &Connection, sql: &str) -> Result<i64, libsql::Error> {
    conn.query(sql, ())
        .await?
        .next()
        .await?
        .ok_or(libsql::Error::QueryReturnedNoRows)?
        .get(0)
}

fn checksum(sql: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, sql.as_bytes()).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn version(conn: &Connection) -> Result<usize, Error> {
    let exists = scalar(
        conn,
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='schema_migrations'",
    )
    .await?;
    if exists == 0 {
        validate_prototype(conn).await?;
        return Ok(0);
    }
    let mut rows = conn
        .query(
            "SELECT version, name, checksum, sql FROM schema_migrations ORDER BY version",
            (),
        )
        .await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        let n: i64 = row.get(0)?;
        let Some(&(name, sql)) = MIGRATIONS.get(count) else {
            return Err(format!(
                "unknown migration version {n}; use the matching application version"
            )
            .into());
        };
        if n != (count + 1) as i64
            || row.get::<String>(1)? != name
            || row.get::<String>(2)? != checksum(sql)
            || row.get::<String>(3)? != sql
        {
            return Err(format!("migration history mismatch at version {n}; restore or reconcile the schema before startup").into());
        }
        count += 1;
    }
    if count == 0 {
        return Err("empty migration history is not a recognized schema".into());
    }
    Ok(count)
}

async fn validate_prototype(conn: &Connection) -> Result<(), Error> {
    let mut objects = conn
        .query(
            "SELECT type, name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
            (),
        )
        .await?;
    let mut names = Vec::new();
    while let Some(row) = objects.next().await? {
        if row.get::<String>(0)? != "table" {
            return Err("unversioned database has unsupported indexes, triggers or views; reconcile before upgrading".into());
        }
        names.push(row.get::<String>(1)?);
    }
    if names.is_empty() {
        return Ok(());
    }
    if names != ["episodes", "operations", "series"] {
        return Err(
            "unrecognized unversioned database; expected the three-table hrrdarr prototype".into(),
        );
    }
    for (table, expected) in [
        ("series", "id,title,year,path,poster"),
        ("episodes", "id,series_id,season,number,title,file_path"),
        (
            "operations",
            "id,episode_id,source,mode,destination,status,message",
        ),
    ] {
        let mut rows = conn
            .query(&format!("PRAGMA table_xinfo({table})"), ())
            .await?;
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await? {
            columns.push(row.get::<String>(1)?);
        }
        if columns.join(",") != expected {
            return Err(
                format!("unsupported prototype columns in {table}; no data changed").into(),
            );
        }
    }
    Ok(())
}

async fn validate_legacy_paths(conn: &Connection) -> Result<(), Error> {
    let mut rows = conn
        .query(
            "SELECT id, file_path FROM episodes WHERE file_path IS NOT NULL",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let path: String = row.get(1)?;
        if path.trim().is_empty() || path.trim().parse::<i64>().is_ok() {
            return Err(format!("episode {} has an ambiguous legacy file_path; reconcile it with the source file record before upgrading (no path guessed)", row.get::<i64>(0)?).into());
        }
    }
    Ok(())
}

async fn integrity(conn: &Connection) -> Result<(), Error> {
    let mut rows = conn.query("PRAGMA integrity_check", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or("integrity check returned no result")?;
    if row.get::<String>(0)? != "ok" || rows.next().await?.is_some() {
        return Err("database integrity check failed".into());
    }
    if conn
        .query("PRAGMA foreign_key_check", ())
        .await?
        .next()
        .await?
        .is_some()
    {
        return Err(
            "database foreign key check failed; reconcile orphaned relationships before startup"
                .into(),
        );
    }
    Ok(())
}

async fn migrate(conn: &Connection, local_path: Option<&Path>) -> Result<Option<PathBuf>, Error> {
    integrity(conn).await?;
    let current = version(conn).await?;
    if current == MIGRATIONS.len() {
        return Ok(None);
    }
    let path = local_path.ok_or("pending remote schema migrations require a verified remote backup/recovery strategy; database unchanged")?;
    let has_data = scalar(
        conn,
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .await?
        > 0;
    let backup = if has_data {
        Some(backup(conn, path).await?)
    } else {
        None
    };
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result: Result<(), Error> = async {
        if version(&tx).await? != current { return Err("schema changed during migration preflight".into()); }
        if current == 0 { tx.execute(HISTORY_SQL, ()).await?; }
        for (index, &(name, sql)) in MIGRATIONS.iter().enumerate().skip(current) {
            if index == 1 { validate_legacy_paths(&tx).await?; }
            tx.execute_batch(sql).await.map_err(|e| format!("migration {} ({name}) failed; reconcile legacy relationships before retrying: {e}", index + 1))?;
            integrity(&tx).await?;
            tx.execute("INSERT INTO schema_migrations (version, name, checksum, sql) VALUES (?1, ?2, ?3, ?4)",
                params![(index + 1) as i64, name, checksum(sql), sql]).await?;
        }
        Ok(())
    }.await;
    match result {
        Ok(()) => tx.commit().await?,
        Err(error) => {
            tx.rollback().await.map_err(|rollback| format!("migration failed ({error}); rollback also failed ({rollback}); restore the pre-migration backup"))?;
            let recovery = backup.as_ref().map_or_else(
                || "fresh database; no pre-existing data".to_owned(),
                |path| format!("recovery backup: {}", path.display()),
            );
            return Err(format!("{error}; migration rolled back; {recovery}").into());
        }
    }
    Ok(backup)
}

async fn backup(conn: &Connection, path: &Path) -> Result<PathBuf, Error> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = parent.join(format!(".hrrdarr-migration-{}", uuid::Uuid::new_v4()));
    let create_path = directory.clone();
    tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(create_path)
    })
    .await??;
    // The exclusively created private directory prevents collisions and symlink replacement.
    // No manifest is published for a partial/invalid snapshot; such directories are retained.
    let snapshot = directory.join("database.db");
    conn.execute(
        "VACUUM main INTO ?1",
        params![snapshot.to_str().ok_or("backup path is not UTF-8")?],
    )
    .await?;
    let copy = libsql::Builder::new_local(&snapshot).build().await?;
    let copy_conn = copy.connect()?;
    integrity(&copy_conn).await?;
    let schema_version = version(&copy_conn).await?;
    drop(copy_conn);
    drop(copy);
    let mut manifest = serde_json::json!({
        "format": 1,
        "purpose": "pre-migration recovery",
        "database": "database.db",
        "includes": ["database records, including any stored credentials"],
        "excludes": ["media", "environment", "external configuration", "external service state"],
        "schema_version": schema_version,
        "backend": "local-libsql",
        "application_version": env!("CARGO_PKG_VERSION"),
        "created_at_unix_seconds": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
    });
    let sync_path = directory.clone();
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
        use std::io::{Read, Write};
        let mut source = File::open(&snapshot)?;
        source.sync_all()?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = [0u8; 65536];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        manifest["sha256"] = hex(digest.finish().as_ref()).into();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(sync_path.join("manifest.pending"))?;
        file.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(
            sync_path.join("manifest.pending"),
            sync_path.join("manifest.json"),
        )?;
        File::open(&sync_path)?.sync_all()?;
        File::open(sync_path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
        Ok(())
    })
    .await??;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_pending_schema_is_rejected_without_writes() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let error = migrate(&conn, None).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("pending remote schema migrations")
        );
        assert_eq!(
            scalar(&conn, "SELECT count(*) FROM sqlite_schema")
                .await
                .unwrap(),
            0
        );
    }
}
