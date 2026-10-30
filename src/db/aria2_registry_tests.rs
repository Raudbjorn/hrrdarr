//! Migration 0072 (Aria2): registry and scope rules, with the label-family guards from 0069.
use super::*;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn aria2_registry_row_and_family_scope_rules() -> Result<(), Error> {
    let dir = Scratch(std::env::temp_dir().join(format!("hrrdarr-aria2-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let db = Database::open_local(dir.0.join("db")).await?;
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT kind,protocol FROM provider_implementations WHERE implementation='aria2'",
            (),
        )
        .await?;
    let row = rows.next().await?.expect("registered");
    assert_eq!(
        (row.get::<String>(0)?, row.get::<String>(1)?),
        ("download_client".into(), "torrent".into())
    );
    let id = uuid::Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'aria2','Torrents',1,1,1,1,'http://127.0.0.1:1/jsonrpc')",[id.clone()]).await?;
    let scope = |media: &'static str, category: &'static str, priority: i64| {
        let (c, id) = (c.clone(), id.clone());
        async move {
            c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state) VALUES(?,'aria2',?,?,?,0,'started')",params![id,media,category,priority]).await
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
    for category in [".", ".."] {
        assert!(
            scope("movies", category, 0).await.is_err(),
            "directory traversal insert"
        );
        assert!(
            c.execute(
                "UPDATE provider_scopes SET category=? WHERE provider_id=? AND media_type='tv'",
                params![category, id.clone()]
            )
            .await
            .is_err(),
            "directory traversal update"
        );
    }
    scope("movies", "movies", 0).await?;
    Ok(())
}
