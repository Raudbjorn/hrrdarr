//! Pure scoped relationship observations: all fixtures are local and disposable.
use hrrdarr::{
    api::MediaDomain, db::Database, health::HealthSeverity,
    health_detectors::evaluate_indexer_client,
};
use libsql::{Connection, params};
use uuid::Uuid;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn database() -> (Scratch, Database, Connection) {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("indexer-client-health-{}", Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let db = Database::open_local(scratch.0.join("native.db"))
        .await
        .unwrap();
    let c = db.connect().await.unwrap();
    (scratch, db, c)
}
async fn provider(c: &Connection, id: &str, kind: &str, enabled: i64, endpoint: &str) {
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES (?,?,?, ?,1,1,1,?)", params![id,kind,format!("Fixture {kind}"),enabled,endpoint]).await.unwrap();
}
async fn scope(c: &Connection, id: &str, kind: &str, media: &str, binding: Option<&str>) {
    if kind == "qbittorrent" {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES (?,'qbittorrent',?,?,0,0,'started','default',0,0,0)",params![id,media,media]).await.unwrap();
    } else {
        c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,categories,anime_categories,anime_standard_format_search,remove_year,download_client_id) VALUES (?,?,?,'[5000]','[]',?,?,?)",params![id,kind,media,if media=="tv" {Some(0)}else{None},if media=="movies" {Some(0)}else{None},binding]).await.unwrap();
    }
}
async fn warning(db: &Database, domain: MediaDomain) {
    let issue = evaluate_indexer_client(db, domain).await.unwrap().unwrap();
    assert_eq!(issue.severity, HealthSeverity::Warning);
    assert_eq!(issue.reason, "IndexerDownloadClient");
    assert_eq!(issue.compatibility_type, "IndexerDownloadClientCheck");
    let app = if domain == MediaDomain::Tv {
        "sonarr"
    } else {
        "radarr"
    };
    assert_eq!(
        issue.wiki_url,
        format!("https://wiki.servarr.com/{app}/system#invalid-indexer-download-client-setting")
    );
    assert!(issue.message.contains("Fixture"));
    assert!(issue.message.len() < 512);
}

#[tokio::test]
async fn both_domains_relationship_matrix_is_pure_and_ignores_transport_and_endpoint() {
    let (_scratch, db, c) = database().await;
    // An owned listener detects any unexpected network attempt without contacting services.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let client = Uuid::new_v4().to_string();
    provider(&c, &client, "qbittorrent", 1, &endpoint).await;
    for (domain, media) in [(MediaDomain::Tv, "tv"), (MediaDomain::Movies, "movies")] {
        assert_eq!(evaluate_indexer_client(&db, domain).await, Ok(None));
        scope(&c, &client, "qbittorrent", media, None).await;
        let indexer = Uuid::new_v4().to_string();
        let inactive = Uuid::new_v4().to_string();
        let missing = Uuid::new_v4().to_string();
        provider(&c, &inactive, "torznab", 0, "invalid endpoint").await;
        scope(&c, &inactive, "torznab", media, Some(&missing)).await;
        // Newznab -> qBittorrent is intentionally wrong transport but a valid relationship.
        provider(&c, &indexer, "newznab", 1, "invalid endpoint").await;
        scope(&c, &indexer, "newznab", media, None).await;
        assert_eq!(evaluate_indexer_client(&db, domain).await, Ok(None));
        c.execute(
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![client.as_str(), indexer.as_str()],
        )
        .await
        .unwrap();
        assert_eq!(evaluate_indexer_client(&db, domain).await, Ok(None));
        c.execute(
            "UPDATE providers SET enabled=0,revision=revision+1 WHERE id=?",
            [client.as_str()],
        )
        .await
        .unwrap();
        warning(&db, domain).await;
        c.execute(
            "UPDATE providers SET enabled=1,revision=revision+1 WHERE id=?",
            [client.as_str()],
        )
        .await
        .unwrap();
        c.execute(
            "DELETE FROM provider_scopes WHERE provider_id=? AND media_type=?",
            params![client.as_str(), media],
        )
        .await
        .unwrap();
        warning(&db, domain).await;
        scope(&c, &client, "qbittorrent", media, None).await;
        c.execute(
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![missing.as_str(), indexer.as_str()],
        )
        .await
        .unwrap();
        warning(&db, domain).await;
        c.execute(
            "UPDATE provider_scopes SET download_client_id=? WHERE provider_id=?",
            params![client.as_str(), indexer.as_str()],
        )
        .await
        .unwrap();
    }
    // data_version changes for writes from another connection; detector owns a separate one.
    let version = || async {
        c.query("PRAGMA data_version", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap()
    };
    let before = version().await;
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(evaluate_indexer_client(&db, domain).await, Ok(None));
    }
    assert_eq!(version().await, before);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    c.execute("DELETE FROM providers WHERE id=?", [client.as_str()])
        .await
        .unwrap();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        warning(&db, domain).await;
    }
}

#[tokio::test]
async fn malformed_catalog_is_unevaluated_even_alongside_known_results() {
    // Bypass constraints only in these disposable corruption fixtures. Normal API writes
    // cannot create these states; detectors must still not publish a false healthy result.
    let corruptions = [
        "UPDATE providers SET enabled=2 WHERE name='corrupt'",
        "UPDATE providers SET enabled=0.5 WHERE name='corrupt'",
        "UPDATE providers SET enabled='invalid' WHERE name='corrupt'",
        "UPDATE providers SET enabled=x'01' WHERE name='corrupt'",
        "UPDATE providers SET settings_version=2 WHERE name='corrupt'",
        "UPDATE providers SET enabled=0,settings_version=2 WHERE name='corrupt'",
        "UPDATE providers SET settings_version=1.5 WHERE name='corrupt'",
        "UPDATE providers SET implementation='future' WHERE name='corrupt'",
        "UPDATE providers SET name='safe'||char(0)||'tail' WHERE name='corrupt'",
        "UPDATE provider_scopes SET provider_id=provider_id||char(0)||'tail' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE provider_scopes SET download_client_id=provider_id||char(0)||'tail' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE provider_scopes SET download_client_id=provider_id||'tail' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE providers SET name=char(10) WHERE name='corrupt'",
        "UPDATE providers SET name=printf('%0300d',1) WHERE name='corrupt'",
        "UPDATE provider_scopes SET download_client_id='bad' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE provider_scopes SET download_client_id=x'00' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE provider_scopes SET provider_id='bad' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
        "UPDATE provider_scopes SET implementation='newznab' WHERE provider_id IN (SELECT id FROM providers WHERE name='corrupt')",
    ];
    for corruption in corruptions {
        let (_scratch, db, c) = database().await;
        for media in ["tv", "movies"] {
            let good = Uuid::from_u128(1).to_string();
            if media == "tv" {
                provider(&c, &good, "torznab", 1, "invalid").await;
            }
            scope(
                &c,
                &good,
                "torznab",
                media,
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        }
        let bad = Uuid::from_u128(u128::MAX).to_string();
        provider(&c, &bad, "torznab", 1, "invalid").await;
        for media in ["tv", "movies"] {
            scope(&c, &bad, "torznab", media, None).await;
        }
        c.execute(
            "UPDATE providers SET name='corrupt',revision=revision+1 WHERE id=?",
            [bad.as_str()],
        )
        .await
        .unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON; DROP TRIGGER provider_identity_immutable; DROP TRIGGER provider_revision_step;").await.unwrap();
        c.execute(corruption, ()).await.unwrap();
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            assert_eq!(
                evaluate_indexer_client(&db, domain).await,
                Err("check_failed"),
                "{corruption}: {domain:?}"
            );
        }
    }
}

#[tokio::test]
async fn bound_target_corruption_overflow_and_storage_failure_are_unevaluated() {
    let (_scratch, db, c) = database().await;
    let client = Uuid::new_v4().to_string();
    let indexer = Uuid::new_v4().to_string();
    provider(&c, &client, "qbittorrent", 1, "invalid").await;
    provider(&c, &indexer, "torznab", 1, "invalid").await;
    for media in ["tv", "movies"] {
        scope(&c, &indexer, "torznab", media, Some(&client)).await;
    }
    // Target has no scopes: it must still be validated before reporting lost scope.
    c.execute_batch("PRAGMA ignore_check_constraints=ON; DROP TRIGGER provider_identity_immutable; DROP TRIGGER provider_revision_step;").await.unwrap();
    for change in [
        "enabled=2",
        "enabled=0.5",
        "settings_version=2",
        "implementation='future'",
    ] {
        c.execute(
            &format!("UPDATE providers SET {change} WHERE id=?"),
            [client.as_str()],
        )
        .await
        .unwrap();
        for domain in [MediaDomain::Tv, MediaDomain::Movies] {
            assert_eq!(
                evaluate_indexer_client(&db, domain).await,
                Err("check_failed"),
                "{change}"
            );
        }
        c.execute("UPDATE providers SET enabled=1,settings_version=1,implementation='qbittorrent' WHERE id=?",[client.as_str()]).await.unwrap();
    }
    // Corrupt a target scope while leaving the source indexer scope intact.
    for media in ["tv", "movies"] {
        scope(&c, &client, "qbittorrent", media, None).await;
    }
    c.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER provider_scope_options_update; DROP TRIGGER provider_client_options_update;").await.unwrap();
    c.execute(
        "UPDATE provider_scopes SET implementation='future' WHERE provider_id=?",
        [client.as_str()],
    )
    .await
    .unwrap();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(
            evaluate_indexer_client(&db, domain).await,
            Err("check_failed")
        );
    }
    c.execute(
        "DELETE FROM provider_scopes WHERE provider_id=?",
        [client.as_str()],
    )
    .await
    .unwrap();
    for n in 0..256 {
        let id = Uuid::from_u128(n + 1).to_string();
        provider(&c, &id, "torznab", 1, "invalid").await;
        for media in ["tv", "movies"] {
            scope(&c, &id, "torznab", media, None).await;
        }
        if n == 254 {
            // The existing bound indexer plus these 255 rows reaches the supported limit.
            for domain in [MediaDomain::Tv, MediaDomain::Movies] {
                warning(&db, domain).await;
            }
        }
    }
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(
            evaluate_indexer_client(&db, domain).await,
            Err("check_failed")
        );
    }
    c.execute(
        "ALTER TABLE provider_scopes RENAME TO unavailable_scopes",
        (),
    )
    .await
    .unwrap();
    for domain in [MediaDomain::Tv, MediaDomain::Movies] {
        assert_eq!(
            evaluate_indexer_client(&db, domain).await,
            Err("storage_error")
        );
    }
}
