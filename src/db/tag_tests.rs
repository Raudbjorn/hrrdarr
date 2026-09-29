use super::*;
use crate::snapshots::{self, Application};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn scalar(c: &Connection, sql: &str) -> Result<i64, Error> {
    Ok(c.query(sql, ()).await?.next().await?.unwrap().get(0)?)
}
#[tokio::test]
async fn tags_schema36_rollback_reopen_archive_backfill_and_empty_local_edit() -> Result<(), Error>
{
    let _guard = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
    let s =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-tags-schema-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&s.0)?;
    for version in [233, 206, 242] {
        let app = if version == 233 {
            Application::Sonarr
        } else {
            Application::Radarr
        };
        let source_path = s.0.join(format!("source-{version}"));
        let raw = libsql::Builder::new_local(&source_path).build().await?;
        let c = raw.connect()?;
        c.execute_batch(&format!("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES({version});CREATE TABLE Tags(Id INTEGER,Label TEXT);INSERT INTO Tags VALUES(7,'source');")).await?;
        let sql = match version {
            233 => {
                "CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT,Tags TEXT);INSERT INTO Series VALUES(1,233,'TV',2020,'/tv/test',1,'[]','[7]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);"
            }
            206 => {
                "CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,206,NULL,'Old',2020,'/movies/old',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
            }
            _ => {
                "CREATE TABLE MovieMetadata(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER);INSERT INTO MovieMetadata VALUES(10,242,NULL,'New',2020);CREATE TABLE Movies(Id INTEGER,MovieMetadataId INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER,Tags TEXT);INSERT INTO Movies VALUES(1,10,'/movies/new',1,0,'[7]');CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);"
            }
        };
        c.execute_batch(sql).await?;
        drop(c);
        drop(raw);
        let bytes = std::fs::read(source_path)?;
        for touched in [false, true] {
            let path = s.0.join(format!("destination-{version}-{touched}"));
            let raw = libsql::Builder::new_local(&path).build().await?;
            let c = raw.connect()?;
            c.execute("PRAGMA foreign_keys=ON", ()).await?;
            c.execute(HISTORY_SQL, ()).await?;
            for (i, (name, sql)) in MIGRATIONS.iter().take(36).enumerate() {
                c.execute_batch(sql).await?;
                c.execute(
                    "INSERT INTO schema_migrations(version,name,checksum,sql)VALUES(?,?,?,?)",
                    params![i as i64 + 1, *name, checksum(sql), *sql],
                )
                .await?;
            }
            let tx = c.transaction().await?;
            snapshots::write_core_snapshot_fixture(&tx, app, bytes.clone()).await?;
            tx.commit().await?;
            let tx = c.transaction().await?;
            tx.execute_batch(MIGRATIONS[36].1).await?;
            assert!(tx.execute_batch(MIGRATIONS[36].1).await.is_err());
            tx.rollback().await?;
            assert_eq!(
                scalar(&c, "SELECT count(*) FROM sqlite_schema WHERE name='tags'").await?,
                0
            );
            assert_eq!(version_of(&c).await?, 36);
            drop(c);
            drop(raw);
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let db = Database::open_local(&path).await?;
            let c = db.connect().await?;
            assert_eq!(version_of(&c).await?, 42); // Latest schema42 includes health storage; historical starting version is unchanged.
            assert_eq!(
                scalar(&c, "SELECT tag_version FROM snapshot_imports").await?,
                0
            );
            let (media, table, key) = if version == 233 {
                (crate::api::MediaDomain::Tv, "series_tags", "series_id")
            } else {
                (crate::api::MediaDomain::Movies, "movie_tags", "movie_id")
            };
            if touched {
                crate::tags::assign(
                    &c,
                    media,
                    1,
                    &crate::tags::Assignment {
                        mode: crate::tags::Mode::Replace,
                        ids: vec![],
                    },
                    true,
                )
                .await
                .map_err(|_| "assignment failed")?;
            }
            let r = snapshots::import(&db, app, bytes.clone(), false).await?;
            if touched {
                assert!(!r.applied);
                assert!(r.conflicts > 0);
                assert_eq!(
                    scalar(&c, "SELECT tag_version FROM snapshot_imports").await?,
                    0
                );
            } else {
                assert!(r.applied, "{r:?}");
                assert_eq!(
                    scalar(&c, &format!("SELECT count(*) FROM {table} WHERE {key}=1")).await?,
                    1
                );
                assert!(
                    snapshots::import(&db, app, bytes.clone(), false)
                        .await?
                        .applied
                );
            }
            // Composite domain FK and label CHECK are storage contracts, independent of HTTP validation.
            assert!(
                c.execute(
                    "INSERT INTO tags(media_type,label)VALUES('tv','Bad Label')",
                    ()
                )
                .await
                .is_err()
            );
            if !touched {
                let tag = scalar(&c, "SELECT id FROM tags LIMIT 1").await?;
                let wrong = if version == 233 { "movies" } else { "tv" };
                assert!(
                    c.execute(
                        &format!("UPDATE {table} SET media_type=? WHERE tag_id=?"),
                        params![wrong, tag]
                    )
                    .await
                    .is_err()
                );
            }
            drop(c);
            drop(db);
            let reopened = Database::open_local(&path).await?;
            assert_eq!(version_of(&reopened.connect().await?).await?, 42); // Latest schema42 includes health storage; historical starting version is unchanged.
        }
    }
    Ok(())
}
async fn version_of(c: &Connection) -> Result<i64, Error> {
    scalar(c, "SELECT max(version) FROM schema_migrations").await
}
