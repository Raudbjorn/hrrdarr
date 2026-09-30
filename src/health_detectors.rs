//! CDH observations only. Permission remains authoritative in completed_download_handling.
use crate::{
    api::MediaDomain,
    db::Database,
    providers::{RefreshClient, qbittorrent::EndpointLocality},
};
use libsql::TransactionBehavior;
const MAX_HEALTH_CLIENTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    Sabnzbd,
    Nzbget,
    Other,
}
#[derive(Clone, Copy, Debug)]
pub struct ClientObservation {
    pub kind: ClientKind,
    pub locality: EndpointLocality,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetectorIssue {
    pub reason: &'static str,
    pub message: &'static str,
    pub wiki_url: &'static str,
    pub compatibility_type: &'static str,
}

/// All outcomes are Warning; absence means evaluated OK, errors mean unevaluated.
/// The status result is deliberately consulted before either TV setting flag.
pub fn classify(
    domain: MediaDomain,
    defined: bool,
    enabled: bool,
    statuses: Result<&[ClientObservation], &'static str>,
) -> Result<Option<DetectorIssue>, &'static str> {
    let migration = if domain == MediaDomain::Tv {
        let clients = statuses?;
        if defined {
            None
        } else if clients
            .iter()
            .any(|c| c.locality != EndpointLocality::Loopback)
        {
            Some((
                "cdh_undefined_nonlocal",
                "Confirm completed download handling. A download client endpoint is not loopback; review path accessibility between the client and hrrdarr.",
            ))
        } else if clients.iter().all(|c| c.kind == ClientKind::Sabnzbd) {
            Some((
                "cdh_undefined_sabnzbd",
                "Confirm completed download handling and review SABnzbd completed-download configuration.",
            ))
        } else if clients.iter().all(|c| c.kind == ClientKind::Nzbget) {
            Some((
                "cdh_undefined_nzbget",
                "Confirm completed download handling and review NZBGet completed-download configuration.",
            ))
        } else {
            Some((
                "cdh_undefined",
                "Confirm completed download handling and review download-client import configuration.",
            ))
        }
    } else {
        None
    };
    if let Some((reason, message)) = migration {
        return Ok(Some(DetectorIssue {
            reason,
            message,
            wiki_url: "https://wiki.servarr.com/sonarr/system#completedfailed-download-handling",
            compatibility_type: "ImportMechanismCheck",
        }));
    }
    Ok((!enabled).then_some(DetectorIssue {
        reason: "cdh_disabled",
        message: "Completed download handling is disabled. Enable it to import completed downloads automatically.",
        wiki_url: match domain {
            MediaDomain::Tv => "https://wiki.servarr.com/sonarr/system#completed-download-handling-is-disabled",
            MediaDomain::Movies => "https://wiki.servarr.com/radarr/system#completed-download-handling-is-disabled",
        },
        compatibility_type: "ImportMechanismCheck",
    }))
}

/// Caller owns the batch deadline/cancellation and generation-fenced publication.
/// No writer transaction, status cache, side effects, or detached probes.
pub async fn evaluate_current(
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
    let settings = crate::completed_download_handling::read(&tx, domain)
        .await
        .map_err(|_| "storage_error")?;
    let mut providers = Vec::new();
    if domain == MediaDomain::Tv {
        // ponytail: serial probing is bounded to 256 clients and the worker's 30s batch;
        // add resumable batches if measured deployments exceed that budget.
        let mut rows = tx.query("SELECT p.id,p.revision,p.implementation FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type='tv' AND p.implementation NOT IN ('torznab','newznab') ORDER BY p.id LIMIT ?", [(MAX_HEALTH_CLIENTS + 1) as i64]).await.map_err(|_| "storage_error")?;
        while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
            if providers.len() == MAX_HEALTH_CLIENTS {
                return Err("check_failed");
            }
            providers.push((
                row.get::<String>(0).map_err(|_| "storage_error")?,
                row.get::<i64>(1).map_err(|_| "storage_error")?,
                row.get::<String>(2).map_err(|_| "storage_error")?,
            ));
        }
    }
    tx.commit().await.map_err(|_| "storage_error")?;
    let mut observations = Vec::with_capacity(providers.len());
    for (id, revision, implementation) in providers {
        // Do not silently treat a future unsupported client as a successful status.
        if implementation != "qbittorrent" {
            return Err("check_failed");
        }
        let locality = client
            .inspect_client_status(&id, revision, domain)
            .await
            .map_err(|e| {
                if e.code == "refresh_timeout" {
                    "check_timeout"
                } else {
                    "check_failed"
                }
            })?;
        observations.push(ClientObservation {
            kind: ClientKind::Other,
            locality,
        });
    }
    classify(
        domain,
        settings.defined,
        settings.enabled,
        Ok(&observations),
    )
}
