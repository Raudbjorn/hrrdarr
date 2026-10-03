//! Scoped client health observations. Download permission remains authoritative in CDH settings.
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
        let mut rows = tx.query("SELECT p.id,p.revision,p.implementation FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type='tv' AND p.implementation='qbittorrent' ORDER BY p.id LIMIT ?", [(MAX_HEALTH_CLIENTS + 1) as i64]).await.map_err(|_| "storage_error")?;
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

/// Communication facts have their own severity; they never borrow CDH's status result.
#[derive(Debug, PartialEq, Eq)]
pub struct CommunicationIssue {
    pub severity: crate::health::HealthSeverity,
    pub reason: &'static str,
    pub message: String,
    pub wiki_url: &'static str,
    pub compatibility_type: &'static str,
}

/// Caller owns the aggregate deadline and atomic publication of per-check outcomes.
pub async fn evaluate_communication(
    db: &Database,
    client: &RefreshClient,
    domain: MediaDomain,
) -> Result<Option<CommunicationIssue>, &'static str> {
    use crate::providers::CommunicationProbeError;
    if !client.matches_database(db) {
        return Err("check_failed");
    }
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    let mut providers = Vec::new();
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    let mut rows = tx.query("SELECT p.id,p.revision,p.implementation,p.name FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type=? AND p.implementation='qbittorrent' ORDER BY p.id LIMIT ?", libsql::params![media,(MAX_HEALTH_CLIENTS+1) as i64]).await.map_err(|_| "storage_error")?;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        if providers.len() == MAX_HEALTH_CLIENTS {
            return Err("check_failed");
        }
        providers.push((
            row.get::<String>(0).map_err(|_| "storage_error")?,
            row.get::<i64>(1).map_err(|_| "storage_error")?,
            row.get::<String>(2).map_err(|_| "storage_error")?,
            row.get::<String>(3).map_err(|_| "storage_error")?,
        ));
    }
    drop(rows);
    tx.commit().await.map_err(|_| "storage_error")?;
    let empty = providers.is_empty();
    let mut remote_failure = None;
    for (id, revision, implementation, name) in providers {
        if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err("check_failed");
        }
        if implementation != "qbittorrent" {
            return Err("check_failed");
        }
        match client
            .probe_download_communication(&id, revision, domain)
            .await
        {
            Ok(()) => (),
            Err(CommunicationProbeError::RemoteFailure) => {
                remote_failure = Some((id, name));
                break;
            }
            Err(CommunicationProbeError::Unevaluated(code)) => return Err(code),
        }
    }
    if !empty && remote_failure.is_none() {
        return Ok(None);
    }
    Ok(Some(CommunicationIssue {
        severity: if empty {
            crate::health::HealthSeverity::Warning
        } else {
            crate::health::HealthSeverity::Error
        },
        reason: if empty {
            "download_client_none_available"
        } else {
            "download_client_communication_failed"
        },
        message: match remote_failure {
            None => "No enabled download client is configured for this media domain.".to_owned(),
            Some((id, name)) => format!(
                "Unable to retrieve items from download client {name} ({id}). Review its connection, authentication and protocol settings."
            ),
        },
        wiki_url: match (domain, empty) {
            (MediaDomain::Tv, true) => {
                "https://wiki.servarr.com/sonarr/system#no-download-client-is-available"
            }
            (MediaDomain::Tv, false) => {
                "https://wiki.servarr.com/sonarr/system#unable-to-communicate-with-download-client"
            }
            (MediaDomain::Movies, true) => {
                "https://wiki.servarr.com/radarr/system#no-download-client-is-available"
            }
            (MediaDomain::Movies, false) => {
                "https://wiki.servarr.com/radarr/system#unable-to-communicate-with-download-client"
            }
        },
        compatibility_type: "DownloadClientCheck",
    }))
}

/// Pure relationship observation. Endpoint validity and transport compatibility belong to
/// other checks; the caller owns the deadline and generation-fenced publication.
pub async fn evaluate_indexer_client(
    db: &Database,
    domain: MediaDomain,
) -> Result<Option<CommunicationIssue>, &'static str> {
    const MAX_SCOPED_PROVIDERS: i64 = 256;
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    // No enabled filter: corrupt enable values must not disappear from evaluation.
    // Validate storage types in SQL because libsql's integer getter permits coercion.
    // Truncate before materializing strings so damaged storage cannot allocate unbounded data.
    let mut rows = tx.query(
        "SELECT substr(s.provider_id,1,37), substr(p.name,1,129), substr(p.implementation,1,16),
         p.enabled, substr(s.download_client_id,1,37),
         CASE WHEN typeof(s.provider_id)='text' AND length(CAST(s.provider_id AS BLOB))=36
          AND instr(s.provider_id,char(0))=0 AND typeof(p.name)='text'
          AND length(CAST(p.name AS BLOB)) BETWEEN 1 AND 128 AND instr(p.name,char(0))=0
          AND typeof(p.implementation)='text' AND p.implementation IN ('torznab','newznab','qbittorrent','torrentrss')
          AND typeof(p.settings_version)='integer' AND p.settings_version=1
          AND typeof(p.enabled)='integer' AND p.enabled IN (0,1)
          AND typeof(s.implementation)='text' AND s.implementation=p.implementation
          AND (s.download_client_id IS NULL OR (typeof(s.download_client_id)='text'
           AND length(CAST(s.download_client_id AS BLOB))=36 AND instr(s.download_client_id,char(0))=0))
         THEN 1 ELSE 0 END,
         t.id IS NOT NULL,
         CASE WHEN t.implementation='qbittorrent' AND typeof(t.implementation)='text'
          AND typeof(t.settings_version)='integer' AND t.settings_version=1
          AND typeof(t.enabled)='integer' AND t.enabled IN (0,1)
         THEN 1 ELSE 0 END,
         t.enabled, ts.provider_id IS NOT NULL,
         CASE WHEN ts.provider_id IS NULL OR (typeof(ts.implementation)='text' AND ts.implementation=t.implementation) THEN 1 ELSE 0 END
         FROM provider_scopes s LEFT JOIN providers p ON p.id=s.provider_id
         LEFT JOIN providers t ON t.id=s.download_client_id
         LEFT JOIN provider_scopes ts ON ts.provider_id=t.id AND ts.media_type=s.media_type
         WHERE s.media_type=? ORDER BY s.provider_id LIMIT ?",
        libsql::params![media, MAX_SCOPED_PROVIDERS + 1],
    ).await.map_err(|_| "storage_error")?;
    let mut first = None;
    let mut broken = 0usize;
    let mut count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        count += 1;
        if count > MAX_SCOPED_PROVIDERS || row.get::<i64>(5).map_err(|_| "check_failed")? != 1 {
            return Err("check_failed");
        }
        let id = row.get::<String>(0).map_err(|_| "check_failed")?;
        let name = row.get::<String>(1).map_err(|_| "check_failed")?;
        let canonical_uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s);
        if !canonical_uuid(&id)
            || name.trim().is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
        {
            return Err("check_failed");
        }
        let implementation = row.get::<String>(2).map_err(|_| "check_failed")?;
        let binding = row.get::<Option<String>>(4).map_err(|_| "check_failed")?;
        if binding.as_deref().is_some_and(|id| !canonical_uuid(id))
            || (implementation == "qbittorrent" && binding.is_some())
        {
            return Err("check_failed");
        }
        if implementation == "qbittorrent" || row.get::<i64>(3).map_err(|_| "check_failed")? == 0 {
            continue;
        }
        if binding.is_none() {
            continue;
        }
        let exists = row.get::<i64>(6).map_err(|_| "check_failed")? == 1;
        if exists
            && (row.get::<i64>(7).map_err(|_| "check_failed")? != 1
                || row.get::<i64>(10).map_err(|_| "check_failed")? != 1)
        {
            return Err("check_failed");
        }
        if !exists
            || row.get::<i64>(8).map_err(|_| "check_failed")? == 0
            || row.get::<i64>(9).map_err(|_| "check_failed")? == 0
        {
            broken += 1;
            if first.is_none() {
                first = Some((name, id));
            }
        }
    }
    drop(rows);
    tx.commit().await.map_err(|_| "storage_error")?;
    Ok(first.map(|(name, id)| CommunicationIssue {
        severity: crate::health::HealthSeverity::Warning,
        reason: "IndexerDownloadClient",
        message: format!("{broken} indexer(s) reference a missing, disabled or unscoped download client. Review indexer {name} ({id}) and its download client setting."),
        wiki_url: match domain {
            MediaDomain::Tv => "https://wiki.servarr.com/sonarr/system#invalid-indexer-download-client-setting",
            MediaDomain::Movies => "https://wiki.servarr.com/radarr/system#invalid-indexer-download-client-setting",
        },
        compatibility_type: "IndexerDownloadClientCheck",
    }))
}
