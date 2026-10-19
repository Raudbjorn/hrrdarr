//! Read-only diagnosis. A successful path lookup never clears an unfinished import.
use super::{HealthIdentity, HealthIssue, HealthScope, HealthSeverity};
use crate::{
    api::MediaDomain,
    db::Database,
    providers::{PathOutput, RefreshClient, qbittorrent::EndpointLocality},
};
use libsql::{Connection, TransactionBehavior};
use serde_json::Value;
use std::{io::Read, path::Path, time::Duration};

const JOURNAL_QUERY: &str = "SELECT substr(CAST(j.operation_id AS BLOB),1,37),substr(CAST(j.path_failure_code AS BLOB),1,65),substr(CAST(j.error_code AS BLOB),1,65),substr(CAST(j.plan_json AS BLOB),1,16385),substr(CAST(i.provenance_json AS BLOB),1,16385),substr(CAST(j.phase AS BLOB),1,17),EXISTS(SELECT 1 FROM scan_import_facts f WHERE f.operation_id=j.operation_id),EXISTS(SELECT 1 FROM scan_import_facts f JOIN downloaded_scan_children c ON c.id=f.child_id AND c.command_id=f.command_id AND c.operation_id=f.operation_id JOIN downloaded_scan_commands p ON p.id=f.command_id WHERE f.operation_id=j.operation_id AND p.media_type=CASE o.media_type WHEN 'episode' THEN 'tv' WHEN 'movie' THEN 'movies' END AND i.operation_id IS NULL) FROM import_journal j JOIN operations o ON o.id=j.operation_id LEFT JOIN rss_candidate_imports i ON i.operation_id=j.operation_id WHERE j.phase!='complete' AND (j.path_failure_code IS NOT NULL OR j.error_code IS NOT NULL OR j.phase!='preview') AND o.media_type=? AND (i.candidate_id IS NULL OR EXISTS(SELECT 1 FROM rss_candidates r JOIN providers p ON p.id=r.client_id JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=r.media_type WHERE r.id=i.candidate_id AND p.enabled=1 AND p.revision=r.client_revision AND ((o.media_type='movie' AND r.media_type='movies' AND r.movie_id=o.movie_id) OR (o.media_type='episode' AND r.media_type='tv' AND EXISTS(SELECT 1 FROM rss_candidate_episodes e JOIN episodes ep ON ep.id=e.episode_id WHERE e.candidate_id=r.id AND e.episode_id=o.episode_id AND ep.series_id=r.series_id))))) ORDER BY j.operation_id LIMIT 1025";
const RECEIPT_QUERY: &str = "SELECT substr(CAST(d.reasons_json AS BLOB),1,8193),substr(CAST(d.error_code AS BLOB),1,65),d.total_preflight_attempts,substr(CAST(d.status AS BLOB),1,17) FROM download_processing d JOIN rss_candidates r ON r.id=d.candidate_id JOIN providers p ON p.id=r.client_id JOIN provider_scopes s ON s.provider_id=p.id AND s.media_type=r.media_type WHERE d.status NOT IN ('imported','cancelled') AND r.media_type=? AND p.enabled=1 AND p.revision=r.client_revision AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id WHERE i.candidate_id=d.candidate_id AND (j.phase!='preview' OR j.path_failure_code IS NOT NULL OR j.error_code IS NOT NULL)) ORDER BY d.candidate_id LIMIT 1025";

/// Downloaded-scan source cleanup: only running parents can hold a pending removal intent, so
/// the 1024-command pool bounds this read (status-prefixed `downloaded_scan_ready` index).
const SCAN_CLEANUP_QUERY: &str = "SELECT substr(CAST(p.id AS BLOB),1,37),substr(CAST(json_extract(p.cleanup_json,'$.phase') AS BLOB),1,17),substr(CAST(json_type(p.cleanup_json,'$.error') AS BLOB),1,17),substr(CAST(json_extract(p.cleanup_json,'$.error') AS BLOB),1,65) FROM downloaded_scan_commands p INDEXED BY downloaded_scan_ready WHERE p.status='running' AND p.media_type=? AND p.cleanup_state='pending' ORDER BY p.id LIMIT 1025";
/// Measured selected-child prepare refusals (no operation ever existed). Evidence stays current
/// until a later request for the same domain and source publishes a new inventory (0058
/// partial `downloaded_scan_prepare_refused` and `downloaded_scan_source_request` indexes).
const SCAN_PREPARE_QUERY: &str = "SELECT substr(CAST(c.error_code AS BLOB),1,65) FROM downloaded_scan_children c INDEXED BY downloaded_scan_prepare_refused JOIN downloaded_scan_commands p ON p.id=c.command_id WHERE c.disposition='failed' AND c.operation_id IS NULL AND substr(c.error_code,1,6)='hc021_' AND p.media_type=? AND NOT EXISTS(SELECT 1 FROM downloaded_scan_commands q WHERE q.media_type=p.media_type AND q.source_path=p.source_path AND q.inventory_revision=1 AND (q.created_at>p.created_at OR (q.created_at=p.created_at AND q.id>p.id))) ORDER BY c.command_id,c.id LIMIT 1025";

const MAX_RECORDS: usize = 1024;
const MAX_CLIENTS: usize = 256;
const PLAN_BYTES: usize = 16384;
const REASON_BYTES: usize = 8192;
const OBSERVATION_BUDGET: Duration = Duration::from_secs(3);
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Accessible,
    Missing,
    Denied,
    Invalid,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Docker {
    Present,
    Unknown,
}
#[derive(Clone, Copy)]
struct Finding {
    reason: &'static str,
    message: &'static str,
    anchor: &'static str,
}
const SCAN_CLEANUP: Finding = Finding {
    reason: "path_scan_cleanup_uncertain",
    message: "Removing emptied downloaded-scan source folders stopped after it started, so its effect is unknown. Check the scanned folder, then explicitly resume that scan's source-folder cleanup; only empty folders are ever removed.",
    anchor: "remote-path-import-failed",
};
const GENERIC: Finding = Finding {
    reason: "path_import_failed",
    message: "An import remains unresolved. Inspect its recorded status and retry the same operation after checking download mappings and access to source, destination and staging directories.",
    anchor: "remote-path-import-failed",
};

fn text(row: &libsql::Row, column: i32, limit: usize) -> Result<Option<String>> {
    let value = row
        .get::<Option<Vec<u8>>>(column)
        .map_err(|_| "storage_error")?;
    value
        .map(|bytes| {
            if bytes.len() > limit || bytes.contains(&0) {
                return Err("check_failed");
            }
            String::from_utf8(bytes).map_err(|_| "check_failed")
        })
        .transpose()
}
fn json(value: &str) -> Result<Value> {
    serde_json::from_str(value).map_err(|_| "check_failed")
}
fn path_code(code: &str, prepare: bool) -> Result<Finding> {
    let parts: Vec<_> = code.split('_').collect();
    if parts.len() != 3 {
        return Err("check_failed");
    }
    let (role, kind, boundary) = (parts[0], parts[1], parts[2]);
    let role_ok = if prepare {
        matches!(role, "source" | "root" | "destination")
    } else {
        matches!(
            role,
            "source" | "root" | "destination" | "stage" | "retirement"
        )
    };
    if !role_ok
        || !matches!(kind, "missing" | "denied" | "changed" | "invalid" | "io")
        || (prepare && boundary != "prepare")
        || (!prepare && !matches!(boundary, "precommit" | "cleanup"))
        || (role == "retirement" && boundary != "cleanup")
    {
        return Err("check_failed");
    }
    if kind == "denied" {
        return Ok(Finding {
            reason: "path_permission_denied",
            message: match role {
                "source" => {
                    "An import recorded denied access to its source. Check access for the hrrdarr service identity, then explicitly retry the operation."
                }
                "root" => {
                    "An import recorded denied access to its library root. Check mount and service access, then explicitly retry the operation."
                }
                "destination" => {
                    "An import recorded denied access to its destination. Check service access, then explicitly retry the operation."
                }
                "stage" => {
                    "An import recorded denied access to staging files. Preserve retained files and explicitly retry after correcting access."
                }
                _ => {
                    "An import recorded denied access while retiring a replaced file. Preserve recovery files and explicitly resume cleanup after correcting access."
                }
            },
            anchor: "permissions-error",
        });
    }
    if boundary == "cleanup" {
        return Ok(Finding {
            reason: "path_cleanup_failed",
            message: "Imported files have unfinished cleanup. Preserve source and recovery files and explicitly resume the recorded operation; source absence during cleanup is not proof of pre-import loss.",
            anchor: "remote-path-import-failed",
        });
    }
    if role == "source" && kind == "missing" && !prepare {
        return Ok(Finding {
            reason: "path_source_removed",
            message: "A previously approved source disappeared before import completed. Restore or review the source and explicitly retry the recorded operation.",
            anchor: "remote-path-file-removed",
        });
    }
    if role == "source" && kind == "changed" {
        return Ok(Finding {
            reason: "path_source_changed",
            message: "An import recorded a changed source identity or content. Preserve existing library files and review the source before retrying.",
            anchor: "remote-path-import-failed",
        });
    }
    Ok(Finding {
        reason: "path_import_unavailable",
        message: match role {
            "source" => {
                "An import could not prepare or access its reported source. Check download output and mappings before explicitly retrying; this does not establish that an approved file was removed."
            }
            "root" => {
                "An import could not use its library root. Check the mount, directory identity and service access before explicitly retrying."
            }
            "destination" => {
                "An import could not use its destination. Check the directory and retained files before explicitly retrying."
            }
            _ => {
                "An import could not use its staging files. Preserve retained files and inspect the operation before explicitly retrying."
            }
        },
        anchor: "remote-path-import-failed",
    })
}
fn receipt_marker(code: &str) -> Result<Option<Finding>> {
    let code = code.strip_prefix("hc021_").ok_or("check_failed")?;
    match code {
        "unresolved_attempt" => Ok(None),
        "mapping_missing" | "mapping_changed" => Ok(Some(Finding {
            reason: "path_mapping_failed",
            message: "A download import recorded a missing or changed path mapping. Review the mapping and explicitly retry the same receipt.",
            anchor: "bad-remote-path-mapping",
        })),
        "output_missing" | "output_invalid" => Ok(Some(Finding {
            reason: "path_download_output_invalid",
            message: "A download import recorded absent or unusable client output. Correct the client output settings or mapping and explicitly retry the receipt.",
            anchor: "remote-path-import-failed",
        })),
        other => path_code(other, true).map(Some),
    }
}
fn receipt_reasons(raw: &str) -> Result<(bool, Option<Finding>)> {
    let reasons: Vec<String> = serde_json::from_str(raw).map_err(|_| "check_failed")?;
    if reasons.len() > 64
        || reasons.iter().any(|v| {
            v.len() > 128
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
    {
        return Err("check_failed");
    }
    let mut seen = std::collections::HashSet::new();
    let mut finding = None;
    let mut marked = false;
    for (i, reason) in reasons.iter().enumerate() {
        if !seen.insert(reason) {
            return Err("check_failed");
        }
        if reason.starts_with("hc021_") {
            if i != 0 {
                return Err("check_failed");
            }
            marked = true;
            finding = receipt_marker(reason)?;
        }
    }
    Ok((marked, finding))
}
fn valid_plan(raw: &str) -> Result<()> {
    let plan: crate::import::Plan = serde_json::from_str(raw).map_err(|_| "check_failed")?;
    if !(1..=super::MAX_INTEGER).contains(&plan.owner_id) {
        return Err("check_failed");
    }
    for path in [&plan.root, &plan.source, &plan.destination] {
        let parsed = Path::new(path);
        let normalized: std::path::PathBuf = parsed.components().collect();
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || !parsed.is_absolute()
            || normalized.to_str() != Some(path.as_str())
            || parsed.components().any(|part| {
                !matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
        {
            return Err("check_failed");
        }
    }
    if !Path::new(&plan.destination).starts_with(&plan.root) || plan.source == plan.destination {
        return Err("check_failed");
    }
    Ok(())
}

struct Evidence {
    finding: Option<Finding>,
    unknown: Option<&'static str>,
    providers: Vec<(String, i64, String)>,
}
impl Evidence {
    fn record(&mut self, value: Result<Option<Finding>>) {
        match value {
            Ok(Some(finding)) => {
                self.finding.get_or_insert(finding);
            }
            Ok(None) => {}
            Err(code) => {
                self.unknown.get_or_insert(code);
            }
        }
    }
}
async fn snapshot(c: &Connection, domain: MediaDomain) -> Result<Option<Evidence>> {
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    let operation_media = match domain {
        MediaDomain::Tv => "episode",
        MediaDomain::Movies => "movie",
    };
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await
        .map_err(|_| "storage_error")?;
    if !crate::completed_download_handling::read(&tx, domain)
        .await
        .map_err(|_| "storage_error")?
        .enabled
    {
        tx.commit().await.map_err(|_| "storage_error")?;
        return Ok(None);
    }
    let mut evidence = Evidence {
        finding: None,
        unknown: None,
        providers: Vec::new(),
    };
    let mut rows=tx.query("SELECT substr(CAST(p.id AS BLOB),1,37),p.revision,substr(CAST(p.implementation AS BLOB),1,65) FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.enabled=1 AND s.media_type=? AND p.implementation NOT IN ('torznab','newznab') ORDER BY p.id LIMIT 257",[media]).await.map_err(|_|"storage_error")?;
    let mut provider_count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        provider_count += 1;
        if provider_count > MAX_CLIENTS {
            evidence.unknown = Some("check_failed");
            break;
        }
        let value = (|| {
            let id = text(&row, 0, 36)?.ok_or("check_failed")?;
            uuid::Uuid::parse_str(&id).map_err(|_| "check_failed")?;
            let implementation = text(&row, 2, 64)?.ok_or("check_failed")?;
            let revision: i64 = row.get(1).map_err(|_| "storage_error")?;
            if !(1..=super::MAX_INTEGER).contains(&revision) {
                return Err("check_failed");
            }
            Ok((id, revision, implementation))
        })();
        match value {
            Ok(provider) => evidence.providers.push(provider),
            Err(code) => evidence.record(Err(code)),
        }
    }
    drop(rows);
    // The partial predicate matches migration0053; linked receipts have one journal authority.
    let mut rows = tx
        .query(JOURNAL_QUERY, [operation_media])
        .await
        .map_err(|_| "storage_error")?;
    let mut count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        count += 1;
        if count > MAX_RECORDS {
            evidence.unknown = Some("check_failed");
            break;
        }
        let result = (|| {
            let id = text(&row, 0, 36)?.ok_or("check_failed")?;
            uuid::Uuid::parse_str(&id).map_err(|_| "check_failed")?;
            let code = text(&row, 1, 64)?;
            let error = text(&row, 2, 64)?;
            valid_plan(&text(&row, 3, PLAN_BYTES)?.ok_or("check_failed")?)?;
            if let Some(raw) = text(&row, 4, PLAN_BYTES)? {
                let value = json(&raw)?;
                if !value.is_object()
                    || ![
                        "policy_revision",
                        "mapping_id",
                        "mapping_revision",
                        "expected_size",
                    ]
                    .iter()
                    .all(|k| value.get(k).and_then(Value::as_u64).is_some_and(|v| v > 0))
                {
                    return Err("check_failed");
                }
            }
            let phase = text(&row, 5, 16)?.ok_or("check_failed")?;
            if !matches!(
                phase.as_str(),
                "preview" | "staging" | "staged" | "published" | "committed"
            ) {
                return Err("check_failed");
            }
            // A scan-owned journal shares the no-RSS branch with manual imports: its ownership
            // must be the exact linked child of a same-domain scan, or it is never healthy.
            let scan_owned = row.get::<i64>(6).map_err(|_| "storage_error")? == 1;
            let scan_valid = row.get::<i64>(7).map_err(|_| "storage_error")? == 1;
            if scan_owned && !scan_valid {
                return Err("check_failed");
            }
            match code.as_deref() {
                Some("unresolved_attempt") | None => {
                    if crate::import::operation_active(&id).map_err(|_| "check_failed")? {
                        return Err("check_failed");
                    }
                    if error.is_some() {
                        Ok(Some(GENERIC))
                    } else {
                        Err("check_failed")
                    }
                }
                Some(code) => path_code(code, false).map(Some),
            }
        })();
        evidence.record(result);
    }
    drop(rows);
    scan_evidence(&tx, media, &mut evidence).await?;
    // A clean linked preview has not armed the journal yet; its receipt still owns the evidence.
    let mut rows = tx
        .query(RECEIPT_QUERY, [media])
        .await
        .map_err(|_| "storage_error")?;
    count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        count += 1;
        if count > MAX_RECORDS {
            evidence.unknown = Some("check_failed");
            break;
        }
        let result = (|| {
            let attempt: i64 = row.get(2).map_err(|_| "storage_error")?;
            if !(0..=super::MAX_INTEGER).contains(&attempt) {
                return Err("check_failed");
            }
            let raw = text(&row, 0, REASON_BYTES)?.ok_or("check_failed")?;
            let error = text(&row, 1, 64)?;
            let (marked, diagnosed) = receipt_reasons(&raw)?;
            let status = text(&row, 3, 16)?.ok_or("check_failed")?;
            if !matches!(
                status.as_str(),
                "queued" | "checking" | "importing" | "blocked"
            ) {
                return Err("check_failed");
            }
            // A never-admitted queue entry has no path observation; malformed evidence still fails above.
            if attempt == 0 && status == "queued" && !marked && error.is_none() {
                return Ok(None);
            }
            match diagnosed {
                Some(finding) => Ok(Some(finding)),
                None if matches!(
                    error.as_deref(),
                    Some(
                        "path_mapping_missing"
                            | "path_mapping_changed"
                            | "source_unavailable"
                            | "import_failed"
                            | "import_conflict"
                    )
                ) =>
                {
                    Ok(Some(GENERIC))
                }
                None => Err("check_failed"),
            }
        })();
        evidence.record(result);
    }
    drop(rows);
    tx.commit().await.map_err(|_| "storage_error")?;
    Ok(Some(evidence))
}
/// Bounded downloaded-scan evidence that has no journal of its own: an armed or uncertain
/// source-cleanup removal intent, and measured selected-child prepare refusals.
async fn scan_evidence(c: &Connection, media: &str, evidence: &mut Evidence) -> Result<()> {
    let mut rows = c
        .query(SCAN_CLEANUP_QUERY, [media])
        .await
        .map_err(|_| "storage_error")?;
    let mut count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        count += 1;
        if count > MAX_RECORDS {
            evidence.unknown = Some("check_failed");
            break;
        }
        let result = (|| {
            let id = text(&row, 0, 36)?.ok_or("check_failed")?;
            uuid::Uuid::parse_str(&id).map_err(|_| "check_failed")?;
            let phase = text(&row, 1, 16)?.ok_or("check_failed")?;
            match text(&row, 2, 16)?.as_deref() {
                None | Some("null") => {}
                Some("text") => {
                    let code = text(&row, 3, 64)?.ok_or("check_failed")?;
                    if code.is_empty()
                        || !code
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    {
                        return Err("check_failed");
                    }
                }
                Some(_) => return Err("check_failed"),
            }
            match phase.as_str() {
                // Observation only: nothing has been removed.
                "observed" => Ok(None),
                "remove_intent" => {
                    // A removal in flight has not settled either way yet.
                    if crate::import::operation_active(&format!("scan-cleanup-{id}"))
                        .map_err(|_| "check_failed")?
                    {
                        return Err("check_failed");
                    }
                    Ok(Some(SCAN_CLEANUP))
                }
                _ => Err("check_failed"),
            }
        })();
        evidence.record(result);
    }
    drop(rows);
    let mut rows = c
        .query(SCAN_PREPARE_QUERY, [media])
        .await
        .map_err(|_| "storage_error")?;
    count = 0;
    while let Some(row) = rows.next().await.map_err(|_| "storage_error")? {
        count += 1;
        if count > MAX_RECORDS {
            evidence.unknown = Some("check_failed");
            break;
        }
        let result = (|| {
            let code = text(&row, 0, 64)?.ok_or("check_failed")?;
            // Only closed measured prepare causes; anything else is malformed evidence.
            let cause = code.strip_prefix("hc021_").ok_or("check_failed")?;
            path_code(cause, true).map(Some)
        })();
        evidence.record(result);
    }
    drop(rows);
    Ok(())
}
/// Tests exercise the same retained-evidence snapshot without unrelated client probes.
#[cfg(test)]
pub(crate) async fn retained_path_diagnostic(
    db: &Database,
    domain: MediaDomain,
) -> Result<Option<&'static str>> {
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let Some(evidence) = snapshot(&c, domain).await? else {
        return Ok(None);
    };
    if let Some(finding) = evidence.finding {
        return Ok(Some(finding.reason));
    }
    match evidence.unknown {
        Some(code) => Err(code),
        None => Ok(None),
    }
}

fn io_access(error: std::io::Error) -> Access {
    match error.kind() {
        std::io::ErrorKind::NotFound => Access::Missing,
        std::io::ErrorKind::PermissionDenied => Access::Denied,
        _ => Access::Unknown,
    }
}
fn directory(path: &str) -> Access {
    if !Path::new(path).is_absolute() {
        return Access::Invalid;
    }
    // Reuse the importer's anchored NOFOLLOW walk without granting import authority.
    let fd = match crate::import::filesystem_directory(Path::new(path)) {
        Ok(fd) => fd,
        Err(error)
            if error.kind() == std::io::ErrorKind::InvalidInput
                || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
                || error.raw_os_error() == Some(rustix::io::Errno::NOTDIR.raw_os_error()) =>
        {
            return Access::Invalid;
        }
        Err(error) => return io_access(error),
    };
    match rustix::fs::accessat(
        &fd,
        ".",
        rustix::fs::Access::READ_OK | rustix::fs::Access::EXEC_OK,
        rustix::fs::AtFlags::EACCESS,
    ) {
        Ok(()) => Access::Accessible,
        Err(error) => io_access(error.into()),
    }
}

fn docker() -> Docker {
    if std::fs::symlink_metadata("/.dockerenv").is_ok_and(|m| m.file_type().is_file()) {
        return Docker::Present;
    }
    let Ok(file) = std::fs::File::open("/proc/self/cgroup") else {
        return Docker::Unknown;
    };
    let mut bytes = Vec::new();
    if file.take(65537).read_to_end(&mut bytes).is_err() || bytes.len() > 65536 {
        return Docker::Unknown;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Docker::Unknown;
    };
    if text.lines().any(|line| {
        line.split('/').any(|part| {
            part == "docker" || (part.starts_with("docker-") && part.ends_with(".scope"))
        })
    }) {
        Docker::Present
    } else {
        Docker::Unknown
    }
}
async fn observe(path: Option<String>) -> Result<(Access, Docker)> {
    let permit = crate::root_folders::OBSERVERS
        .try_acquire()
        .map_err(|_| "check_failed")?;
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        (path.as_deref().map_or(Access::Invalid, directory), docker())
    });
    tokio::time::timeout(OBSERVATION_BUDGET, job)
        .await
        .map_err(|_| "check_timeout")?
        .map_err(|_| "check_failed")
}
fn output(access: Access, locality: EndpointLocality, docker: Docker) -> Result<Option<Finding>> {
    if access == Access::Accessible {
        return Ok(None);
    }
    if access == Access::Unknown {
        return Err("check_failed");
    }
    if access == Access::Denied {
        return Ok(Some(Finding {
            reason: "path_output_permission_denied",
            message: "The hrrdarr service was denied access to a configured download output directory. Correct service permissions before importing.",
            anchor: "permissions-error",
        }));
    }
    if docker == Docker::Present
        && (access == Access::Missing || locality == EndpointLocality::Loopback)
    {
        return Ok(Some(Finding {
            reason: "path_output_container_unavailable",
            message: "A download output directory is missing or unusable inside this container. Check container bind mounts and remote path mappings.",
            anchor: "docker-bad-remote-path-mapping",
        }));
    }
    if locality == EndpointLocality::Loopback {
        return Ok(Some(Finding {
            reason: "path_output_local_unavailable",
            message: "A loopback download client reports an output directory that is missing or unusable here. Check client settings, mount availability and service access; containers may need path mappings.",
            anchor: "bad-download-client-settings",
        }));
    }
    Ok(Some(Finding {
        reason: "path_output_mapping_unavailable",
        message: "A download client reports an output directory that is missing or unusable on this host. Check remote path mappings and client output settings; endpoint locality is unknown.",
        anchor: "bad-remote-path-mapping",
    }))
}
pub(super) async fn evaluate(
    db: &Database,
    client: &RefreshClient,
    identity: &HealthIdentity,
) -> Result<Option<HealthIssue>> {
    if !client.matches_database(db) || identity.check_key != "remote_path_mapping" {
        return Err("check_failed");
    }
    let domain = match identity.scope {
        HealthScope::Tv => MediaDomain::Tv,
        HealthScope::Movies => MediaDomain::Movies,
        _ => return Err("check_failed"),
    };
    let c = db.connect().await.map_err(|_| "storage_error")?;
    let Some(mut evidence) = snapshot(&c, domain).await? else {
        return Ok(None);
    };
    drop(c);
    for (id, revision, implementation) in std::mem::take(&mut evidence.providers) {
        // An unrelated stalled client must not erase an already-established import failure.
        if evidence.finding.is_some() {
            break;
        }
        if !matches!(
            implementation.as_str(),
            "qbittorrent"
                | "sabnzbd"
                | "transmission"
                | "nzbget"
                | "deluge"
                | "rtorrent"
                | "utorrent"
                | "aria2"
                | "flood"
                | "downloadstation"
        ) {
            evidence.record(Err("check_failed"));
            continue;
        }
        let facts = match client.inspect_path_output(&id, revision, domain).await {
            Ok(facts) => facts,
            Err(error) => {
                evidence.record(Err(match error.code {
                    "refresh_timeout" => "check_timeout",
                    "storage_error" => "storage_error",
                    _ => "check_failed",
                }));
                continue;
            }
        };
        let path = match facts.output {
            PathOutput::Local(path) => Some(path),
            PathOutput::Missing => {
                evidence.record(Ok(Some(Finding { reason:"path_output_missing",message:"A download client did not report a usable output directory. Check its scoped category and default output settings.",anchor:"bad-download-client-settings" })));
                continue;
            }
            PathOutput::Invalid => None,
        };
        let result = match observe(path).await {
            Ok((access, docker)) => output(access, facts.locality, docker),
            Err(code) => Err(code),
        };
        evidence.record(result);
    }
    if let Some(finding) = evidence.finding {
        let product = if domain == MediaDomain::Tv {
            "sonarr"
        } else {
            "radarr"
        };
        return Ok(Some(HealthIssue {
            identity: identity.clone(),
            severity: HealthSeverity::Error,
            reason: finding.reason.into(),
            message: finding.message.into(),
            wiki_url: format!(
                "https://wiki.servarr.com/{product}/system#{}",
                finding.anchor
            ),
            compatibility_type: "RemotePathMappingCheck".into(),
        }));
    }
    match evidence.unknown {
        Some(code) => Err(code),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn causes_do_not_invent_permissions_or_precommit_loss() {
        assert_eq!(
            path_code("source_missing_precommit", false).unwrap().reason,
            "path_source_removed"
        );
        assert_eq!(
            path_code("source_missing_cleanup", false).unwrap().reason,
            "path_cleanup_failed"
        );
        assert_ne!(
            path_code("source_missing_prepare", true).unwrap().reason,
            "path_source_removed"
        );
        assert_eq!(
            path_code("destination_denied_precommit", false)
                .unwrap()
                .reason,
            "path_permission_denied"
        );
        assert!(path_code("retirement_missing_precommit", false).is_err());
        assert!(path_code("stage_missing_prepare", true).is_err());
        assert!(path_code("source_denied_precommit_extra", false).is_err());
        for role in ["source", "root", "destination", "stage", "retirement"] {
            for kind in ["missing", "denied", "changed", "invalid", "io"] {
                assert!(path_code(&format!("{role}_{kind}_cleanup"), false).is_ok());
            }
        }
    }
    #[test]
    fn plan_diagnosis_requires_the_complete_import_schema() {
        let identity = serde_json::json!({"dev":1,"ino":2,"size":3,"mtime":4,"mtime_ns":5,"ctime":6,"ctime_ns":7});
        let plan = serde_json::json!({"owner_id":1,"root":"/library","source":"/downloads/source.mkv","destination":"/library/target.mkv","mode":"copy","root_identity":identity,"source_parent":identity,"destination_parent":identity,"source_identity":identity,"stage_name":".hrrdarr-import-test","quarantine_name":".hrrdarr-move-test"});
        assert!(valid_plan(&plan.to_string()).is_ok());
        for field in ["size", "mtime", "mtime_ns", "ctime", "ctime_ns"] {
            let mut malformed = plan.clone();
            malformed["source_identity"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                valid_plan(&malformed.to_string()).is_err(),
                "missing identity field {field}"
            );
        }
        let mut malformed = plan.clone();
        malformed.as_object_mut().unwrap().remove("mode");
        assert!(valid_plan(&malformed.to_string()).is_err());
        malformed["mode"] = serde_json::json!("unknown-mode");
        assert!(valid_plan(&malformed.to_string()).is_err());
        for (field, value) in [
            ("owner_id", serde_json::json!(0)),
            ("destination", serde_json::json!("/other/target.mkv")),
            ("source", serde_json::json!("/downloads/../source.mkv")),
            ("source", serde_json::json!("/downloads/source\n.mkv")),
        ] {
            let mut malformed = plan.clone();
            malformed[field] = value;
            assert!(valid_plan(&malformed.to_string()).is_err());
        }
    }
    #[test]
    fn marker_validation_never_hides_corrupt_evidence() {
        assert!(
            receipt_reasons(r#"["hc021_unresolved_attempt"]"#)
                .unwrap()
                .1
                .is_none()
        );
        assert_eq!(
            receipt_reasons(r#"["hc021_source_denied_prepare","import_conflict"]"#)
                .unwrap()
                .1
                .unwrap()
                .reason,
            "path_permission_denied"
        );
        for value in [
            r#"["hc021_future"]"#,
            r#"["ordinary","hc021_unresolved_attempt"]"#,
            r#"["hc021_mapping_missing","hc021_mapping_changed"]"#,
            r#"["same","same"]"#,
            r#"["bad/token"]"#,
            r#"["UPPER"]"#,
            r#"["í"]"#,
            r#"["bad\nreason"]"#,
            r#"["hc021_source_missing_prepare\u0000"]"#,
            r#"{}"#,
        ] {
            assert!(receipt_reasons(value).is_err(), "{value}");
        }
        assert!(receipt_reasons(&serde_json::to_string(&vec!["x".repeat(129)]).unwrap()).is_err());
        assert!(matches!(receipt_reasons(r#"[""]"#), Ok((false, None))));
    }
    #[test]
    fn locality_and_docker_are_guidance_not_permission_evidence() {
        assert!(
            output(
                Access::Accessible,
                EndpointLocality::Unknown,
                Docker::Unknown
            )
            .unwrap()
            .is_none()
        );
        assert!(output(Access::Unknown, EndpointLocality::Loopback, Docker::Present).is_err());
        assert_eq!(
            output(Access::Missing, EndpointLocality::Loopback, Docker::Unknown)
                .unwrap()
                .unwrap()
                .reason,
            "path_output_local_unavailable"
        );
        assert_eq!(
            output(Access::Invalid, EndpointLocality::Unknown, Docker::Present)
                .unwrap()
                .unwrap()
                .reason,
            "path_output_mapping_unavailable"
        );
        assert_eq!(
            output(Access::Missing, EndpointLocality::Unknown, Docker::Present)
                .unwrap()
                .unwrap()
                .reason,
            "path_output_container_unavailable"
        );
        assert_eq!(
            output(Access::Denied, EndpointLocality::Unknown, Docker::Present)
                .unwrap()
                .unwrap()
                .reason,
            "path_output_permission_denied"
        );
    }
    #[test]
    fn permission_denial_is_measured_as_executing_identity() {
        use std::os::unix::{fs::PermissionsExt, process::CommandExt};
        const CHILD: &str = "HRRDARR_PATH_DENIED_CHILD";
        if let Some(path) = std::env::var_os(CHILD) {
            assert_eq!(directory(path.to_str().unwrap()), Access::Denied);
            return;
        }
        let path =
            std::env::temp_dir().join(format!("hrrdarr-path-denied-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
        let observed = directory(path.to_str().unwrap());
        if observed == Access::Accessible {
            // Privileged test runners must actually drop privilege; chmod alone proves nothing.
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact").arg("health::path_diagnostics::tests::permission_denial_is_measured_as_executing_identity")
                .env(CHILD,&path).uid(65534).gid(65534).output();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::remove_dir(&path).unwrap();
            let output = output.unwrap();
            assert!(
                output.status.success(),
                "Dropped-identity permission probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::remove_dir(&path).unwrap();
            assert_eq!(observed, Access::Denied);
        }
    }
    #[tokio::test]
    async fn successful_only_uses_health_observation_and_preserves_pending_work() {
        use super::super::{PathDiagnosticChange as Change, path_diagnostics_changed};
        let path =
            std::env::temp_dir().join(format!("hrrdarr-path-generation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Database::open_local(path.join("db")).await.unwrap();
        let c = db.connect().await.unwrap();
        // Exercise the actual bounded queries, including provider and journal authority joins.
        for (query, scope, index) in [
            (JOURNAL_QUERY, "episode", "import_journal_path_unresolved"),
            (JOURNAL_QUERY, "movie", "import_journal_path_unresolved"),
            (RECEIPT_QUERY, "tv", "download_processing_path_unresolved"),
            (
                RECEIPT_QUERY,
                "movies",
                "download_processing_path_unresolved",
            ),
            (SCAN_CLEANUP_QUERY, "tv", "downloaded_scan_ready"),
            (SCAN_CLEANUP_QUERY, "movies", "downloaded_scan_ready"),
            (SCAN_PREPARE_QUERY, "tv", "downloaded_scan_prepare_refused"),
            (
                SCAN_PREPARE_QUERY,
                "movies",
                "downloaded_scan_source_request",
            ),
        ] {
            let mut rows = c
                .query(&format!("EXPLAIN QUERY PLAN {query}"), [scope])
                .await
                .unwrap();
            let mut plan = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                plan.push(row.get::<String>(3).unwrap());
            }
            assert!(
                plan.iter().any(|step| step.contains(index)),
                "{scope}: {plan:?}"
            );
        }
        for sql in [
            "SELECT CAST(X'ff' AS BLOB)",
            "SELECT CAST(X'610062' AS BLOB)",
            "SELECT CAST('over-limit' AS BLOB)",
        ] {
            let row = c
                .query(sql, ())
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            assert!(text(&row, 0, 4).is_err());
        }
        assert!(
            path_diagnostics_changed(&c, MediaDomain::Tv, Change::Admission)
                .await
                .is_err()
        );
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        path_diagnostics_changed(&tx, MediaDomain::Tv, Change::Failure)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let row=c.query("SELECT generation,pending_reasons,due_at FROM health_checks WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1);
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
        assert_eq!(row.get::<Option<i64>>(2).unwrap(), None);
        // SuccessfulOnly is previous completed Health success, even after admission stales it.
        c.execute("UPDATE health_checks SET checked_at=1,observed_generation=generation,observed_epoch='00000000-0000-0000-0000-000000000001',severity=0 WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        path_diagnostics_changed(&tx, MediaDomain::Tv, Change::Admission)
            .await
            .unwrap();
        path_diagnostics_changed(&tx, MediaDomain::Tv, Change::Failure)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let row=c.query("SELECT generation,pending_reasons,due_at FROM health_checks WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 3);
        assert_eq!(row.get::<i64>(1).unwrap(), 8);
        assert!(row.get::<Option<i64>>(2).unwrap().is_some());
        c.execute("UPDATE health_checks SET severity=3,reason='existing',message='Existing issue',wiki_url='https://wiki.servarr.com/sonarr/system',due_at=1 WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        path_diagnostics_changed(&tx, MediaDomain::Tv, Change::Failure)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let row=c.query("SELECT pending_reasons,due_at FROM health_checks WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap().next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 8);
        assert_eq!(row.get::<i64>(1).unwrap(), 1);
        // Exhaustion must not partially commit the caller's authoritative mutation.
        c.execute(
            "DELETE FROM health_checks WHERE scope='tv' AND check_key='remote_path_mapping'",
            (),
        )
        .await
        .unwrap();
        c.execute("INSERT INTO health_checks(scope,check_key,startup,scheduled,generation,checked_at,observed_generation,observed_epoch,severity,compatibility_type) VALUES('tv','remote_path_mapping',1,1,9007199254740991,1,1,'00000000-0000-0000-0000-000000000001',0,'RemotePathMappingCheck')",()).await.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        tx.execute("UPDATE health_checks SET checked_at=2 WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap();
        assert!(
            path_diagnostics_changed(&tx, MediaDomain::Tv, Change::Completion)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        assert_eq!(c.query("SELECT checked_at FROM health_checks WHERE scope='tv' AND check_key='remote_path_mapping'",()).await.unwrap().next().await.unwrap().unwrap().get::<i64>(0).unwrap(),1);
        drop(c);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
}
