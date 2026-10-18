//! Output-directory observations never authorize imports or filesystem changes.
use crate::{
    api::MediaDomain, db::Database, health_detectors::DetectorIssue, providers::RefreshClient,
};
use libsql::TransactionBehavior;
use std::path::Path;

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
    let mut rows = tx.query("SELECT p.id,p.revision,p.implementation FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type=? AND p.implementation='qbittorrent' ORDER BY p.id LIMIT 257", [media]).await.map_err(|_| "storage_error")?;
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
        if implementation != "qbittorrent" {
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
    if !conflict {
        if let Some(code) = failed {
            return Err(code);
        }
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
}
