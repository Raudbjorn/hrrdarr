//! Migration 0068 (Deluge and the label-torrent family): registry row, replaced scope triggers
//! and label uniqueness, exercised on a freshly migrated database and across reopen.
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn client(c: &Connection) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'deluge','Torrents',1,1,1,1,'http://127.0.0.1:1')",[id.clone()]).await?;
    Ok(id)
}
async fn scope(
    c: &Connection,
    id: &str,
    media: &str,
    category: Option<&str>,
    priority: i64,
    state: Option<&str>,
) -> Result<u64, libsql::Error> {
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,'deluge',?,?,?,0,?)",params![id,media,category,priority,state]).await
}

#[tokio::test]
async fn deluge_registry_scope_shape_options_and_labels_are_enforced() -> Result<(), Error> {
    let dir =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-deluge-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("db");
    {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        let mut rows = c
            .query(
                "SELECT kind,protocol FROM provider_implementations WHERE implementation='deluge'",
                (),
            )
            .await?;
        let row = rows.next().await?.expect("registered");
        assert_eq!(
            (row.get::<String>(0)?, row.get::<String>(1)?),
            ("download_client".into(), "torrent".into())
        );
        // The rest of the family (rTorrent and uTorrent are registered by 0069 and 0070) is anticipated by the triggers but not registered yet.
        let mut rows = c
            .query(
                "SELECT count(*) FROM provider_implementations WHERE implementation IN ('flood','aria2','downloadstation')",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
        let id = client(&c).await?;
        // Queue placement is last (0) or first (1); add-paused is initial_state.
        for priority in [0, 1] {
            c.execute(
                "DELETE FROM provider_scopes WHERE provider_id=?",
                [id.clone()],
            )
            .await?;
            scope(&c, &id, "tv", Some("tv-label"), priority, Some("started")).await?;
        }
        for bad in [-1, 2, 900] {
            assert!(
                scope(&c, &id, "movies", Some("movies"), bad, Some("started"))
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            scope(&c, &id, "movies", None, 0, Some("started"))
                .await
                .is_err(),
            "label required"
        );
        assert!(
            scope(&c, &id, "movies", Some("movies"), 0, None)
                .await
                .is_err(),
            "state required"
        );
        assert!(
            scope(&c, &id, "movies", Some("movies"), 0, Some("forced"))
                .await
                .is_err()
        );
        // Labels are the strictest family rule: lowercase letters, digits and `_ . -`.
        for bad in ["Movies", "mov ies", "mov/ies", "mov,ies", ""] {
            assert!(
                scope(&c, &id, "movies", Some(bad), 0, Some("started"))
                    .await
                    .is_err(),
                "{bad:?}"
            );
        }
        assert!(
            scope(&c, &id, "movies", Some("tv-label"), 0, Some("stopped"))
                .await
                .is_err(),
            "TV and movies never share a label"
        );
        scope(&c, &id, "movies", Some("movies.v2_x"), 0, Some("stopped")).await?;
        // The options trigger rejects stray indexer or qBittorrent-only options.
        assert!(
            c.execute(
                "UPDATE provider_scopes SET content_layout='original' WHERE provider_id=? AND media_type='movies'",
                [id.clone()],
            )
            .await
            .is_err()
        );
    }
    // Reopen is a no-op and keeps the rows.
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT count(*) FROM provider_scopes WHERE implementation='deluge'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 2);
    Ok(())
}
