//! Output-directory observations never authorize imports or filesystem changes.
use crate::{
    api::MediaDomain, db::Database, health_detectors::DetectorIssue, providers::RefreshClient,
};
use libsql::TransactionBehavior;
use std::path::Path;

/// Download clients whose output directory can be inspected; the provider query and the
/// per-provider guard below both derive from this one list.
const INSPECTABLE_CLIENTS: [&str; 10] = [
    "qbittorrent",
    "sabnzbd",
    "transmission",
    "nzbget",
    "deluge",
    "rtorrent",
    "utorrent",
    "aria2",
    "flood",
    "downloadstation",
];

pub(super) async fn evaluate(
    db: &Database,
    client: &RefreshClient,
    domain: MediaDomain,
) -> Result<Option<DetectorIssue>, &'static str> {
    if !client.matches_database(db) {
        return Err("check_failed");
    }
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    // ponytail: bounded serial probes share the health worker's 30s deadline; resumable
    // batches are needed only if real deployments exceed these registry limits.
    let mut roots = Vec::new();
    let mut rows = tx
        .query("SELECT path FROM root_folders ORDER BY id LIMIT 10001", ())
        .await
        .map_err(|_| "storage_error")?;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        if roots.len() == 10000 {
            return Err("check_failed");
        }
        let path: String = row.get(0).map_err(|_| "storage_error")?;
        roots.push(crate::library::normalized_path(&path).ok_or("check_failed")?);
    }
    drop(rows);
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    let mut providers = Vec::new();
    // `INSPECTABLE_CLIENTS` is a closed list of literals, so interpolating it is injection-free.
    let clients = INSPECTABLE_CLIENTS.map(|c| format!("'{c}'")).join(",");
    let query = format!(
        "SELECT p.id,p.revision,p.implementation FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type=? AND p.implementation IN ({clients}) ORDER BY p.id LIMIT 257"
    );
    let mut rows = tx
        .query(&query, [media])
        .await
        .map_err(|_| "storage_error")?;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        if providers.len() == 256 {
            return Err("check_failed");
        }
        providers.push((
            row.get::<String>(0).map_err(|_| "storage_error")?,
            row.get::<i64>(1).map_err(|_| "storage_error")?,
            row.get::<String>(2).map_err(|_| "storage_error")?,
        ));
    }
    drop(rows);
    tx.commit().await.map_err(|_| "storage_error")?;
    drop(c);
    let mut conflict = false;
    let mut failed = None;
    for (id, revision, implementation) in providers {
        if !INSPECTABLE_CLIENTS.contains(&implementation.as_str()) {
            failed.get_or_insert("check_failed");
            continue;
        }
        let outputs = client
            .inspect_download_roots(&id, revision, domain)
            .await
            .map_err(|e| {
                if e.code == "refresh_timeout" {
                    "check_timeout"
                } else {
                    "check_failed"
                }
            });
        match outputs {
            Ok(outputs) => {
                if outputs
                    .iter()
                    .any(|output| roots.iter().any(|root| contains(root, output)))
                {
                    conflict = true;
                    break;
                }
            }
            Err(code) => {
                failed.get_or_insert(code);
            }
        }
    }
    if !conflict
        && let Some(code) = failed {
            return Err(code);
        }
    Ok(conflict.then_some(DetectorIssue {
        reason: "download_client_root_folder",
        message: "A download client writes into a configured library root. Configure a separate download directory before importing media.",
        wiki_url: match domain { MediaDomain::Tv => "https://wiki.servarr.com/sonarr/system#downloads-in-root-folder", MediaDomain::Movies => "https://wiki.servarr.com/radarr/system#downloads-in-root-folder" },
        compatibility_type: "DownloadClientRootFolderCheck",
    }))
}
fn contains(root: &str, output: &str) -> bool {
    Path::new(output).starts_with(Path::new(root))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_root_component_boundaries() {
        assert!(super::contains("/library/movies", "/library/movies"));
        assert!(super::contains(
            "/library/movies",
            "/library/movies/downloads"
        ));
        assert!(!super::contains("/library/movies", "/library/movies-old"));
        assert!(!super::contains("/library/movies", "/library"));
    }

    // Reasoning: the provider query and the per-provider guard both derive from
    // `INSPECTABLE_CLIENTS`; a client registered as a download client but missing from it was
    // silently never checked for writing into a library root.
    #[tokio::test]
    async fn every_registered_download_client_is_inspected() {
        let dir = std::env::temp_dir().join(format!("hrrdarr-roots-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let db = Database::open_local(dir.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        let mut rows = c
            .query(
                "SELECT implementation FROM provider_implementations WHERE kind='download_client' ORDER BY implementation",
                (),
            )
            .await
            .unwrap();
        let mut registered = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            registered.push(row.get::<String>(0).unwrap());
        }
        drop(rows);
        drop(c);
        drop(db);
        std::fs::remove_dir_all(&dir).ok();
        let mut inspected: Vec<String> =
            INSPECTABLE_CLIENTS.iter().map(|c| c.to_string()).collect();
        inspected.sort();
        assert_eq!(registered, inspected);
    }
}
