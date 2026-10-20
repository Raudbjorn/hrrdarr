//! Upgrade retirement into the domain's recycle bin (Sonarr/Radarr move a replaced file into the
//! configured recycle bin instead of deleting it).
//!
//! Runs after every replaced original reached its private recovery artifact (`quarantined`) and
//! before the import may complete, inside the import's file-effect lease. Per original: decide
//! from the domain's settings (no bin: nothing is written and the artifact is retained exactly as
//! before) → journal intent with the artifact identity and the bin destination → one transfer
//! (same-device no-replace rename, or cross-device copy, verify, publish, then artifact removal)
//! → one transaction that records history and completes. Any failure that is not a storage
//! error settles the row as `retained`: the artifact keeps the bytes and the code says why. A
//! crash anywhere is reconciled from the journal and observed disk state on the next run of the
//! same import or at worker startup. No transaction is held across a transfer.
use super::*;
use crate::{
    api::MediaDomain,
    recycle_bin::fsops::{self, FsError, Identity, Moved, Source},
};
use fs::OldFile;

/// Unsettled journal rows reconciled per worker-startup pass.
const RECOVERY_BATCH: i64 = 64;

pub(super) struct Retired {
    pub old: OldFile,
    pub targets: Vec<MediaTarget>,
}

struct Row {
    id: Uuid,
    media: MediaDomain,
    old_file_id: i64,
    original_path: String,
    source: String,
    destination: Option<String>,
    identity: Option<Identity>,
    dest: Option<(u64, u64)>,
    phase: String,
}

fn bin_code(error: FsError) -> &'static str {
    match error {
        FsError::Missing => "recycle_bin_unavailable",
        FsError::Denied => "recycle_bin_unwritable",
        FsError::Unsafe | FsError::NotRegular => "recycle_bin_unsafe",
        FsError::Exists => "recycle_destination_exists",
        FsError::Limit => "transfer_limit",
        FsError::Changed => "recycle_source_changed",
        FsError::Io => "recycle_filesystem_error",
    }
}
fn source_code(error: FsError) -> &'static str {
    match error {
        FsError::Missing => "recycle_source_missing",
        FsError::Changed | FsError::NotRegular => "recycle_source_changed",
        FsError::Denied => "recycle_source_denied",
        FsError::Unsafe => "recycle_source_unsafe",
        _ => "recycle_filesystem_error",
    }
}
fn label(media: MediaDomain) -> &'static str {
    crate::recycle_bin::domain(media)
}
fn now() -> Result<i64> {
    crate::commands::now().map_err(|_| Error::internal())
}
fn settings_error(error: crate::recycle_bin::Error) -> Error {
    eprintln!(
        "{}",
        serde_json::json!({"level":"ERROR","component":"upgrade_recycle","condition":"settings_unavailable","code":error.code()})
    );
    Error::internal()
}

/// Settles every quarantined original of this committed operation. Storage failures propagate so
/// the operation stays committed and is resumed; filesystem outcomes settle as rows.
pub(super) async fn settle(
    c: &Connection,
    opid: &str,
    target: &MediaTarget,
    plan: &Plan,
    lease: &Arc<Permit>,
) -> Result<()> {
    let media = super::media(target);
    for retired in owned::quarantined(c, opid, target).await? {
        let row = match load(c, opid, media, retired.old.file_id).await? {
            Some(row) => row,
            None => {
                match admit(c, opid, media, plan.owner_id, &plan.root, &retired, lease).await? {
                    Some(row) => row,
                    None => continue,
                }
            }
        };
        drive(c, row, &retired.targets, lease).await?;
    }
    Ok(())
}

const COLUMNS: &str = "id,media_type,old_file_id,original_path,source,destination,source_dev,source_ino,source_size,source_mtime,source_mtime_ns,dest_dev,dest_ino,phase";
fn parse_row(r: &libsql::Row) -> Result<Row> {
    let number = |v: Option<String>| -> Result<Option<u64>> {
        v.map(|s| s.parse::<u64>().map_err(|_| Error::internal()))
            .transpose()
    };
    let identity = match (
        number(r.get(6)?)?,
        number(r.get(7)?)?,
        r.get::<Option<i64>>(8)?,
        r.get::<Option<i64>>(9)?,
        r.get::<Option<i64>>(10)?,
    ) {
        (Some(dev), Some(ino), Some(size), Some(mtime), Some(mtime_ns)) => Some(Identity {
            dev,
            ino,
            size: u64::try_from(size).map_err(|_| Error::internal())?,
            mtime,
            mtime_ns,
        }),
        (None, None, None, None, None) => None,
        _ => return Err(Error::internal()),
    };
    let dest = match (number(r.get(11)?)?, number(r.get(12)?)?) {
        (Some(dev), Some(ino)) => Some((dev, ino)),
        (None, None) => None,
        _ => return Err(Error::internal()),
    };
    Ok(Row {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| Error::internal())?,
        media: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| Error::internal())?,
        old_file_id: r.get(2)?,
        original_path: r.get(3)?,
        source: r.get(4)?,
        destination: r.get(5)?,
        identity,
        dest,
        phase: r.get(13)?,
    })
}
async fn load(
    c: &Connection,
    opid: &str,
    media: MediaDomain,
    old_file_id: i64,
) -> Result<Option<Row>> {
    let row = c
        .query(
            &format!("SELECT {COLUMNS} FROM upgrade_recycles WHERE operation_id=? AND media_type=? AND old_file_id=?"),
            params![opid, label(media), old_file_id],
        )
        .await?
        .next()
        .await?;
    row.as_ref().map(parse_row).transpose()
}
async fn reload(c: &Connection, id: Uuid) -> Result<Row> {
    let row = c
        .query(
            &format!("SELECT {COLUMNS} FROM upgrade_recycles WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or_else(Error::internal)?;
    parse_row(&row)
}

/// Prepared outside any transaction: the artifact still is the captured original and a free
/// destination exists in the bin (`bin/<series or movie folder>/<relative folders>/<name>`).
fn prepare(
    source: &str,
    expected: Identity,
    bin: &str,
    layout: std::result::Result<(Vec<String>, String), &'static str>,
) -> std::result::Result<(Identity, String), (&'static str, Option<Identity>)> {
    let present = match fsops::inspect(source) {
        Ok(Source::Present(present)) => present,
        Ok(Source::Absent) => return Err(("recycle_source_missing", None)),
        Err(error) => return Err((source_code(error), None)),
    };
    if present.identity != expected {
        return Err(("recycle_source_changed", None));
    }
    let identity = present.identity;
    let (subfolder, leaf) = layout.map_err(|code| (code, Some(identity)))?;
    fsops::prepare_destination(bin, &subfolder, &leaf)
        .map(|destination| (identity, destination.path))
        .map_err(|error| (bin_code(error), Some(identity)))
}

async fn admit(
    c: &Connection,
    opid: &str,
    media: MediaDomain,
    owner_id: i64,
    owner_path: &str,
    retired: &Retired,
    lease: &Arc<Permit>,
) -> Result<Option<Row>> {
    let settings = crate::recycle_bin::read(c, media)
        .await
        .map_err(settings_error)?;
    // No bin configured: permanent retirement semantics are unchanged; nothing is recorded.
    let Some(bin) = settings.path else {
        return Ok(None);
    };
    let old = &retired.old;
    let source = old.recovery_path()?;
    let expected = old.bin_identity();
    let layout = crate::recycle_bin::deletion::layout(owner_path, &old.path).map_err(|e| e.code());
    let (s, b) = (source.clone(), bin.clone());
    let prepared = leased(lease, move || Ok(prepare(&s, expected, &b, layout))).await?;
    let id = Uuid::new_v4();
    let (destination, identity, phase, code) = match prepared {
        Ok((identity, destination)) => (Some(destination), Some(identity), "admitted", None),
        Err((code, identity)) => (None, identity, "retained", Some(code)),
    };
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if load(&tx, opid, media, old.file_id).await?.is_some() {
            return Ok(false);
        }
        let current = crate::recycle_bin::read(&tx, media)
            .await
            .map_err(settings_error)?;
        if current.path.as_deref() != Some(bin.as_str()) {
            return Err(Error::conflict(
                "recycle_bin_changed",
                "Recycle bin settings changed while retiring the replaced file; retry the import",
            ));
        }
        let timestamp = now()?;
        // A destination is recorded only for an admitted transfer; a retained row keeps the
        // identity it could measure so the artifact remains attributable.
        tx.execute(
            "INSERT INTO upgrade_recycles(id,operation_id,media_type,old_file_id,owner_id,original_path,source,bin_path,destination,source_dev,source_ino,source_size,source_mtime,source_mtime_ns,phase,error_code,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id.to_string(),
                opid,
                label(media),
                old.file_id,
                owner_id,
                old.path.clone(),
                source.clone(),
                bin.clone(),
                destination.clone(),
                identity.map(|v| v.dev.to_string()),
                identity.map(|v| v.ino.to_string()),
                identity.map(|v| v.size as i64),
                identity.map(|v| v.mtime),
                identity.map(|v| v.mtime_ns),
                phase,
                code,
                timestamp,
                timestamp
            ],
        )
        .await?;
        if code.is_some() {
            crate::health::recycle_bin_changed(&tx, media)
                .await
                .map_err(|_| Error::internal())?;
        }
        Ok(true)
    }
    .await;
    match outcome {
        Ok(inserted) => {
            tx.commit().await?;
            if let Some(code) = code.filter(|_| inserted) {
                log_retained(id, media, old.file_id, code);
            }
        }
        Err(error) => {
            tx.rollback().await?;
            return Err(error);
        }
    }
    load(c, opid, media, old.file_id).await
}
fn log_retained(id: Uuid, media: MediaDomain, file_id: i64, code: &str) {
    eprintln!(
        "{}",
        serde_json::json!({"level":"ERROR","component":"upgrade_recycle","event":"upgrade_recycle_retained","recycle_id":id,"media_type":label(media),"file_id":file_id,"code":code,"effect":"replaced file retained in its private recovery artifact"})
    );
}

/// Drives one row to a terminal phase from the journal and observed disk state.
/// `copied` → artifact removal → complete; `admitted` → transfer or reconciliation.
async fn drive(
    c: &Connection,
    mut row: Row,
    targets: &[MediaTarget],
    lease: &Arc<Permit>,
) -> Result<()> {
    let mut failure: Option<&'static str> = None;
    // Each pass either settles, or performs the single transfer and re-observes once.
    for _ in 0..3 {
        match row.phase.as_str() {
            "complete" | "retained" => return Ok(()),
            "copied" => return finish_copy(c, &row, targets, lease).await,
            "admitted" => {}
            _ => return Err(Error::internal()),
        }
        let identity = row.identity.ok_or_else(Error::internal)?;
        let destination = row.destination.clone().ok_or_else(Error::internal)?;
        let (s, d) = (row.source.clone(), destination.clone());
        let observed = leased(lease, move || Ok((fsops::observe(&s), fsops::observe(&d)))).await?;
        let source_now = match observed.0 {
            Ok(v) => v,
            Err(error) => return retain(c, &row, source_code(error)).await,
        };
        let dest_now = match observed.1 {
            Ok(v) => v,
            Err(error) => return retain(c, &row, bin_code(error)).await,
        };
        let ours = dest_now.is_some_and(|v| v.same_inode(identity.dev, identity.ino));
        match (source_now, ours) {
            (None, true) => return complete(c, &row, "rename", None, targets).await,
            (Some(found), true) if found.same_inode(identity.dev, identity.ino) => {
                // Interrupted no-replace link fallback: both names are the same inode.
                let s = row.source.clone();
                let removed = leased(lease, move || {
                    Ok(fsops::remove_linked_original(
                        &s,
                        identity.dev,
                        identity.ino,
                    ))
                })
                .await?;
                let code = removed.err().map(|_| "recycle_source_retained");
                return complete(c, &row, "link", code, targets).await;
            }
            (Some(found), false) if found == identity => {
                let (d, id) = (destination.clone(), row.id.to_string());
                if let Err(error) =
                    leased(lease, move || Ok(fsops::remove_partial(&d, &id))).await?
                {
                    return retain(c, &row, bin_code(error)).await;
                }
                if dest_now.is_some() {
                    // Never replace or adopt an unrecorded file at the destination.
                    return retain(c, &row, "recycle_destination_exists").await;
                }
                if let Some(code) = failure {
                    return retain(c, &row, code).await;
                }
                match transfer(&row, identity, &destination, lease).await? {
                    Ok(Moved::Rename) => {
                        crash_point(&row.original_path)?;
                        return complete(c, &row, "rename", None, targets).await;
                    }
                    Ok(Moved::Link) => {
                        crash_point(&row.original_path)?;
                        return complete(c, &row, "link", None, targets).await;
                    }
                    Ok(Moved::Copy { dev, ino }) => {
                        checkpoint_copied(c, &row, dev, ino).await?;
                        crash_point(&row.original_path)?;
                        row = reload(c, row.id).await?;
                    }
                    Err(error) => {
                        eprintln!(
                            "event=upgrade_recycle_transfer_failed recycle_id={} code={}",
                            row.id,
                            error.code()
                        );
                        failure = Some(bin_code(error));
                    }
                }
            }
            (Some(_), _) => return retain(c, &row, "recycle_source_changed").await,
            (None, false) => return retain(c, &row, "recycle_source_missing").await,
        }
    }
    Err(Error::internal())
}
async fn transfer(
    row: &Row,
    identity: Identity,
    destination: &str,
    lease: &Arc<Permit>,
) -> Result<std::result::Result<Moved, FsError>> {
    let (source, destination, id) = (
        row.source.clone(),
        destination.to_owned(),
        row.id.to_string(),
    );
    leased(lease, move || {
        Ok((|| {
            let present = match fsops::inspect(&source)? {
                Source::Present(present) if present.identity == identity => present,
                _ => return Err(FsError::Changed),
            };
            let (parent, name) = fsops::split(&destination)?;
            let target = fsops::Destination {
                dir: fsops::open_dir(&parent)?,
                name,
                path: destination,
            };
            fsops::move_into_bin(&present, &target, &id)
        })())
    })
    .await
}
async fn finish_copy(
    c: &Connection,
    row: &Row,
    targets: &[MediaTarget],
    lease: &Arc<Permit>,
) -> Result<()> {
    let identity = row.identity.ok_or_else(Error::internal)?;
    let (dev, ino) = row.dest.ok_or_else(Error::internal)?;
    let destination = row.destination.clone().ok_or_else(Error::internal)?;
    let observed = leased(lease, move || Ok(fsops::observe(&destination))).await?;
    match observed {
        Ok(Some(found)) if found.same_inode(dev, ino) => {
            // The verified copy is in the bin; remove the artifact only if it is still ours.
            let s = row.source.clone();
            let removed = leased(lease, move || Ok(fsops::remove_original(&s, &identity))).await?;
            let code = removed.err().map(|_| "recycle_source_retained");
            complete(c, row, "copy", code, targets).await
        }
        Ok(_) => retain(c, row, "recycled_copy_missing").await,
        Err(error) => retain(c, row, bin_code(error)).await,
    }
}
async fn checkpoint_copied(c: &Connection, row: &Row, dev: u64, ino: u64) -> Result<()> {
    let changed = c
        .execute(
            "UPDATE upgrade_recycles SET phase='copied',transfer='copy',dest_dev=?,dest_ino=?,updated_at=? WHERE id=? AND phase='admitted'",
            params![dev.to_string(), ino.to_string(), now()?, row.id.to_string()],
        )
        .await?;
    if changed != 1 {
        return Err(Error::internal());
    }
    Ok(())
}
/// History for every target and the terminal phase commit together.
async fn complete(
    c: &Connection,
    row: &Row,
    transfer: &str,
    code: Option<&str>,
    targets: &[MediaTarget],
) -> Result<()> {
    let destination = row.destination.clone().ok_or_else(Error::internal)?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        for target in targets {
            let (kind, episode, movie) = match target {
                MediaTarget::Episode(id) => ("episode", Some(*id), None),
                MediaTarget::Movie(id) => ("movie", None, Some(*id)),
            };
            tx.execute(
                "INSERT INTO upgrade_recycle_history(id,recycle_id,media_type,episode_id,movie_id,file_id,path,recycle_path,reason) VALUES(?,?,?,?,?,?,?,?,'upgrade')",
                params![
                    Uuid::new_v4().to_string(),
                    row.id.to_string(),
                    kind,
                    episode,
                    movie,
                    row.old_file_id,
                    row.original_path.clone(),
                    destination.clone()
                ],
            )
            .await?;
        }
        let changed = tx
            .execute(
                "UPDATE upgrade_recycles SET phase='complete',transfer=coalesce(transfer,?),error_code=?,updated_at=? WHERE id=? AND phase IN ('admitted','copied')",
                params![transfer, code, now()?, row.id.to_string()],
            )
            .await?;
        if changed != 1 {
            return Err(Error::internal());
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            eprintln!(
                "{}",
                serde_json::json!({"level":"INFO","component":"upgrade_recycle","event":"replaced_file_recycled","recycle_id":row.id,"media_type":label(row.media),"file_id":row.old_file_id,"transfer":transfer,"targets":targets.len(),"code":code})
            );
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
async fn retain(c: &Connection, row: &Row, code: &'static str) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let changed = tx
            .execute(
                "UPDATE upgrade_recycles SET phase='retained',error_code=?,updated_at=? WHERE id=? AND phase IN ('admitted','copied')",
                params![code, now()?, row.id.to_string()],
            )
            .await?;
        if changed != 1 {
            return Err(Error::internal());
        }
        crate::health::recycle_bin_changed(&tx, row.media)
            .await
            .map_err(|_| Error::internal())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            log_retained(row.id, row.media, row.old_file_id, code);
            Ok(())
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Worker-startup roll-forward for rows a crash left between artifact and bin. The caller holds
/// the global file-effect permit, so no import owns these rows. The import itself still completes
/// on its next resume; this only finishes the recorded transfer.
pub(crate) async fn recover(db: &Database, guard: &DeletionGuard) -> Result<()> {
    let lease = guard._permit.clone();
    let c = db.connect().await?;
    let mut rows = c
        .query(
            "SELECT r.id,r.operation_id,o.media_type,o.episode_id,o.movie_id FROM upgrade_recycles r JOIN operations o ON o.id=r.operation_id WHERE r.phase IN ('admitted','copied') ORDER BY r.created_at,r.id LIMIT ?",
            [RECOVERY_BATCH],
        )
        .await?;
    let mut pending = Vec::new();
    while let Some(r) = rows.next().await? {
        let target = match r.get::<String>(2)?.as_str() {
            "episode" => MediaTarget::Episode(r.get(3)?),
            "movie" => MediaTarget::Movie(r.get(4)?),
            _ => return Err(Error::internal()),
        };
        pending.push((
            Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| Error::internal())?,
            r.get::<String>(1)?,
            target,
        ));
    }
    drop(rows);
    for (id, opid, target) in pending {
        // One unresolvable row never prevents the others from rolling forward.
        let outcome = async {
            let row = reload(&c, id).await?;
            let retired = owned::quarantined(&c, &opid, &target).await?;
            let Some(entry) = retired.iter().find(|r| r.old.file_id == row.old_file_id) else {
                return Err(Error::conflict(
                    "retirement_missing",
                    "The recycled original is no longer a quarantined retirement",
                ));
            };
            drive(&c, row, &entry.targets, &lease).await
        }
        .await;
        if let Err(error) = outcome {
            eprintln!(
                "event=upgrade_recycle_recovery_failed recycle_id={id} code={}",
                error.code()
            );
        }
    }
    Ok(())
}

/// Operator-facing settlement summary for an operation, if any original was journaled.
pub(super) async fn summary(
    c: &Connection,
    opid: &str,
) -> Result<Option<(String, Option<String>)>> {
    let row = c
        .query(
            "SELECT phase,error_code FROM upgrade_recycles WHERE operation_id=? ORDER BY CASE phase WHEN 'retained' THEN 0 WHEN 'admitted' THEN 1 WHEN 'copied' THEN 1 ELSE 2 END,created_at,id LIMIT 1",
            [opid],
        )
        .await?
        .next()
        .await?;
    row.map(|r| Ok((r.get::<String>(0)?, r.get::<Option<String>>(1)?)))
        .transpose()
}

#[cfg(test)]
static CRASH_MARKERS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// Test-only simulated process death after the transfer, before the completing transaction.
#[cfg(test)]
pub(super) fn arm_crash(marker: &str) {
    CRASH_MARKERS.lock().unwrap().push(marker.to_owned());
}
#[cfg(test)]
pub(super) fn disarm_crash(marker: &str) {
    CRASH_MARKERS.lock().unwrap().retain(|m| m != marker);
}
fn crash_point(_path: &str) -> Result<()> {
    #[cfg(test)]
    if CRASH_MARKERS
        .lock()
        .map(|markers| markers.iter().any(|m| _path.contains(m.as_str())))
        .unwrap_or(false)
    {
        return Err(Error::internal());
    }
    Ok(())
}
