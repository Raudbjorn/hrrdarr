//! Migration 0066 (NZBGet): registry row, replaced scope triggers and case-insensitive category
//! uniqueness, exercised on a freshly migrated database and across reopen.
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn client(c: &Connection) -> Result<String, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'nzbget','Usenet',1,1,1,1,'http://127.0.0.1:1')",[id.clone()]).await?;
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
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,'nzbget',?,?,?,0,?)",params![id,media,category,priority,state]).await
}

#[tokio::test]
async fn nzbget_registry_scope_shape_options_and_categories_are_enforced() -> Result<(), Error> {
    let dir =
        Scratch(std::env::temp_dir().join(format!("hrrdarr-nzbget-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("db");
    {
        let db = Database::open_local(&path).await?;
        let c = db.connect().await?;
        let mut rows = c
            .query(
                "SELECT kind,protocol FROM provider_implementations WHERE implementation='nzbget'",
                (),
            )
            .await?;
        let row = rows.next().await?.expect("registered");
        assert_eq!(
            (row.get::<String>(0)?, row.get::<String>(1)?),
            ("download_client".into(), "usenet".into())
        );
        let id = client(&c).await?;
        // NZBGet's own priority scale, add-paused as initial_state.
        for priority in [-100, -2, -1, 0, 1, 2] {
            c.execute(
                "DELETE FROM provider_scopes WHERE provider_id=?",
                [id.clone()],
            )
            .await?;
            scope(&c, &id, "tv", Some("tv"), priority, Some("started")).await?;
        }
        for bad in [-50, 50, 3, 900] {
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
            "category required"
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
        // Category names compare case-insensitively across the two domains.
        assert!(
            scope(&c, &id, "movies", Some("TV"), 0, Some("stopped"))
                .await
                .is_err()
        );
        scope(&c, &id, "movies", Some("movies"), 0, Some("stopped")).await?;
    }
    // Reopen is a no-op and keeps the rows.
    let db = Database::open_local(&path).await?;
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT count(*) FROM provider_scopes WHERE implementation='nzbget'",
            (),
        )
        .await?;
    assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 2);
    Ok(())
}
