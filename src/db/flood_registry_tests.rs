//! Migration 0074 (Flood): the registry row on top of the label-family triggers from 0070.
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn flood_registry_row_and_family_scope_rules() -> Result<(), Error> {
    let dir = Scratch(std::env::temp_dir().join(format!("hrrdarr-flood-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let db = Database::open_local(dir.0.join("db")).await?;
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT kind,protocol FROM provider_implementations WHERE implementation='flood'",
            (),
        )
        .await?;
    let row = rows.next().await?.expect("registered");
    assert_eq!(
        (row.get::<String>(0)?, row.get::<String>(1)?),
        ("download_client".into(), "torrent".into())
    );
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'flood','Torrents',1,1,1,1,'http://127.0.0.1:1/')",[id.clone()]).await?;
    let scope = |media: &'static str, category: &'static str, priority: i64| {
        let (c, id) = (c.clone(), id.clone());
        async move {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,'flood',?,?,?,0,'started')",params![id,media,category,priority]).await
        }
    };
    scope("tv", "tv-label", 1).await?;
    assert!(
        scope("movies", "tv-label", 0).await.is_err(),
        "shared label"
    );
    assert!(
        scope("movies", "Movies", 0).await.is_err(),
        "uppercase label"
    );
    assert!(
        scope("movies", "movies", 2).await.is_err(),
        "placement out of range"
    );
    scope("movies", "movies", 0).await?;
    Ok(())
}
