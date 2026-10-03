use super::*;
use std::time::Duration;

pub(crate) async fn startup(db: &Database) -> Result<()> {
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = startup_at(&tx, now()?, Uuid::new_v4()).await;
    finish(tx, result).await
}
async fn startup_at(c: &Connection, timestamp: i64, epoch: Uuid) -> Result<()> {
    // Old grace ownership cannot expire this process's new lifecycle. Preserve all other work.
    c.execute("UPDATE health_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE is_grace=1 AND status IN ('queued','running','retry_wait')",[timestamp]).await?;
    c.execute("UPDATE health_checks SET pending_reasons=pending_reasons & 15,due_at=CASE WHEN (pending_reasons & 15)=0 THEN NULL ELSE due_at END WHERE (pending_reasons & 16)!=0",()).await?;
    c.execute("UPDATE health_lifecycle SET epoch=?,started_at=?,grace_due_at=?,grace_phase='pending',next_scheduled_at=CASE WHEN next_scheduled_at=0 THEN ? ELSE next_scheduled_at END,schedule_error=NULL WHERE id=1",params![epoch.to_string(),timestamp,timestamp+900,timestamp+21600]).await?;
    match mark(
        c,
        &selection(c, HealthScope::All, "startup=1").await?,
        STARTUP,
        timestamp,
    )
    .await
    {
        Err(Error(_, "health_generation_exhausted")) => {
            // An exhausted health counter must not disable unrelated background commands.
            c.execute(
                "UPDATE health_lifecycle SET schedule_error='health_invariant' WHERE id=1",
                (),
            )
            .await?;
            Ok(())
        }
        result => result,
    }
}
async fn enqueue(
    c: &Connection,
    keys: &[HealthSelectionToken],
    priority: CommandPriority,
    manual: bool,
    timestamp: i64,
) -> Result<Option<(Uuid, bool)>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let old = active(c).await?;
    if let Some(cmd) = &old {
        if !matches!(cmd.status, CommandStatus::Queued) {
            return Ok(Some((cmd.id, false)));
        }
    }
    let id = if let Some(cmd) = &old {
        if priority.number() > cmd.priority.number() {
            c.execute(
                "UPDATE health_commands SET priority=? WHERE id=?",
                params![priority.number(), cmd.id.to_string()],
            )
            .await?;
        }
        cmd.id
    } else {
        if crate::commands::command_capacity(c).await? >= crate::commands::MAX_COMMANDS {
            return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
        }
        let id = Uuid::new_v4();
        c.execute("INSERT INTO health_commands(id,priority,next_attempt_at,created_at,epoch) VALUES(?,?,?,?,?)",params![id.to_string(),priority.number(),timestamp,timestamp,lifecycle(c).await?.epoch.to_string()]).await?;
        id
    };
    for key in keys {
        c.execute("INSERT INTO health_command_checks(command_id,scope,check_key,admitted_generation) VALUES(?,?,?,?) ON CONFLICT(command_id,scope,check_key) DO NOTHING",params![id.to_string(),key.identity.scope.text(),key.identity.check_key.clone(),key.generation]).await?;
        if manual {
            c.execute("UPDATE health_command_checks SET admitted_generation=? WHERE command_id=? AND scope=? AND check_key=?",params![key.generation,id.to_string(),key.identity.scope.text(),key.identity.check_key.clone()]).await?;
        }
    }
    // Mark grace ownership only after durable membership covers every startup key.
    let life = lifecycle(c).await?;
    if life.grace_phase == "pending"
        && life.started_at > 0
        && life.grace_due_at <= timestamp
        && read(c, id).await?.epoch == life.epoch
    {
        let complete=c.query("SELECT NOT EXISTS(SELECT 1 FROM health_checks h WHERE startup=1 AND ((pending_reasons & 16)=0 OR NOT EXISTS(SELECT 1 FROM health_command_checks m WHERE m.command_id=? AND m.scope=h.scope AND m.check_key=h.check_key)))",[id.to_string()]).await?.next().await?.ok_or_else(invariant)?.get::<i64>(0)?==1;
        if complete {
            c.execute(
                "UPDATE health_commands SET is_grace=1 WHERE id=?",
                [id.to_string()],
            )
            .await?;
            c.execute(
                "UPDATE health_lifecycle SET grace_phase='rechecking' WHERE id=1",
                (),
            )
            .await?;
        }
    }
    c.execute(
        "UPDATE health_lifecycle SET schedule_error=NULL WHERE id=1 AND schedule_error='command_history_full'",
        (),
    )
    .await?;
    Ok(Some((id, old.is_none())))
}
pub(super) async fn manual(
    c: &Connection,
    input: HealthCommandInput,
    timestamp: i64,
) -> Result<HealthAdmission> {
    let keys = selection(c, input.scope, "1").await?;
    if keys.is_empty() {
        return Err(Error(StatusCode::CONFLICT, "health_scope_unavailable"));
    }
    mark(c, &keys, MANUAL, timestamp).await?;
    let selection = selection(c, input.scope, "1").await?;
    let (id, new) = enqueue(c, &selection, input.priority, true, timestamp)
        .await?
        .ok_or_else(invariant)?;
    if !matches!(read(c, id).await?.status, CommandStatus::Queued) {
        Ok(HealthAdmission::Pending {
            active_command_id: id,
            requested_scope: input.scope,
            selection,
            pending: true,
        })
    } else if new {
        Ok(HealthAdmission::Queued {
            command_id: id,
            requested_scope: input.scope,
            selection,
            pending: false,
        })
    } else {
        Ok(HealthAdmission::CoalescedQueued {
            command_id: id,
            requested_scope: input.scope,
            selection,
            pending: false,
        })
    }
}
pub(crate) async fn sweep(c: &Connection, timestamp: i64) -> Result<()> {
    let life = lifecycle(c).await?;
    // A router without an acquired worker owner must not pretend migration time was startup.
    if life.started_at == 0 {
        return Ok(());
    }
    let scheduled = if life.next_scheduled_at <= timestamp {
        selection(c, HealthScope::All, "scheduled=1").await?
    } else {
        Vec::new()
    };
    let grace = if life.grace_phase == "pending" && life.grace_due_at <= timestamp {
        selection(
            c,
            HealthScope::All,
            "startup=1 AND (pending_reasons & 16)=0",
        )
        .await?
    } else {
        Vec::new()
    };
    // Preflight both triggers together: the same key can increment twice this sweep.
    if scheduled.iter().chain(&grace).any(|key| {
        let increments = i64::from(scheduled.iter().any(|v| v.identity == key.identity))
            + i64::from(grace.iter().any(|v| v.identity == key.identity));
        key.generation > MAX_INTEGER - increments
    }) {
        c.execute(
            "UPDATE health_lifecycle SET schedule_error='health_invariant' WHERE id=1",
            (),
        )
        .await?;
        return Ok(());
    }
    if life.next_scheduled_at <= timestamp {
        mark(c, &scheduled, SCHEDULED, timestamp).await?;
        c.execute(
            "UPDATE health_lifecycle SET next_scheduled_at=? WHERE id=1",
            [timestamp + 21600],
        )
        .await?;
    }
    if !grace.is_empty() {
        // Scheduled marking may have advanced these same identities in this transaction.
        let grace = selection(
            c,
            HealthScope::All,
            "startup=1 AND (pending_reasons & 16)=0",
        )
        .await?;
        mark(c, &grace, GRACE, timestamp).await?;
    }
    let keys = selection(c, HealthScope::All, &format!("due_at<={timestamp}")).await?;
    match enqueue(c, &keys, CommandPriority::Normal, false, timestamp).await {
        Err(Error(_, "command_history_full")) => {
            c.execute(
                "UPDATE health_lifecycle SET schedule_error='command_history_full' WHERE id=1",
                (),
            )
            .await?;
            Ok(())
        }
        Err(e) => Err(e),
        Ok(_) => Ok(()),
    }
}
pub(crate) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<HealthCommand> {
    let life = lifecycle(c).await?;
    c.execute("UPDATE health_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL,epoch=? WHERE id=?",params![timestamp,life.epoch.to_string(),id.to_string()]).await?;
    c.execute("UPDATE health_command_checks AS m SET captured_generation=h.generation,captured_reasons=h.pending_reasons,captured_due_at=h.due_at FROM health_checks h WHERE m.command_id=? AND m.scope=h.scope AND m.check_key=h.check_key",[id.to_string()]).await?;
    read(c, id).await
}
async fn still_running(c: &Connection, cmd: &HealthCommand) -> Result<bool> {
    Ok(c.query("SELECT 1 FROM health_commands c JOIN health_lifecycle l ON l.id=1 WHERE c.id=? AND c.status='running' AND c.attempts=? AND c.epoch=? AND l.epoch=c.epoch",params![cmd.id.to_string(),i64::from(cmd.attempts),cmd.epoch.to_string()]).await?.next().await?.is_some())
}
async fn clear_matching(
    c: &Connection,
    cmd: &HealthCommand,
    admitted: bool,
    error: &str,
    grace_only: bool,
) -> Result<()> {
    for m in &cmd.members {
        let generation = if admitted {
            Some(m.admitted_generation)
        } else {
            m.captured_generation
        };
        if let Some(generation) = generation {
            c.execute("UPDATE health_checks SET pending_reasons=CASE WHEN ? THEN pending_reasons & 15 ELSE 0 END,due_at=CASE WHEN ? AND (pending_reasons & 15)!=0 THEN due_at ELSE NULL END,last_error=? WHERE scope=? AND check_key=? AND generation=?",params![grace_only,grace_only,error,m.identity.scope.text(),m.identity.check_key.clone(),generation]).await?;
        }
    }
    Ok(())
}
async fn reset_grace(
    c: &Connection,
    cmd: &HealthCommand,
    timestamp: i64,
    delay: i64,
) -> Result<()> {
    if cmd.is_grace {
        c.execute("UPDATE health_lifecycle SET grace_phase='pending',grace_due_at=max(started_at,?) WHERE id=1 AND epoch=? AND grace_phase='rechecking'",params![timestamp+delay,cmd.epoch.to_string()]).await?;
    }
    Ok(())
}
pub(super) async fn cancel(c: &Connection, id: Uuid, timestamp: i64) -> Result<HealthCommand> {
    let mut cmd = read(c, id).await?;
    if matches!(
        cmd.status,
        CommandStatus::Queued | CommandStatus::Running | CommandStatus::RetryWait
    ) {
        clear_matching(c, &cmd, true, "cancelled", cmd.is_grace).await?;
        reset_grace(c, &cmd, timestamp, 60).await?;
        c.execute("UPDATE health_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=?", params![timestamp, id.to_string()]).await?;
        // Retention may prune this row immediately when the clock moves backwards.
        // Return the transaction's known outcome rather than rolling cancellation back on 404.
        cmd.status = CommandStatus::Cancelled;
        cmd.completed_at = Some(timestamp);
        cmd.error_code = None;
    }
    Ok(cmd)
}

async fn fail(c: &Connection, cmd: &HealthCommand, timestamp: i64, code: &str) -> Result<()> {
    let retry = cmd.attempts < 3 && code != "stale_inputs";
    for m in &cmd.members {
        c.execute(
            "UPDATE health_checks SET last_error=? WHERE scope=? AND check_key=?",
            params![code, m.identity.scope.text(), m.identity.check_key.clone()],
        )
        .await?;
    }
    if !retry {
        if code != "stale_inputs" {
            clear_matching(c, cmd, false, code, cmd.is_grace).await?;
        }
        reset_grace(
            c,
            cmd,
            timestamp,
            if code == "stale_inputs" { 0 } else { 60 },
        )
        .await?;
    }
    settle_failure(c, cmd, timestamp, code, retry).await
}
async fn settle_failure(
    c: &Connection,
    cmd: &HealthCommand,
    timestamp: i64,
    code: &str,
    retry: bool,
) -> Result<()> {
    let delay = (1i64 << cmd.attempts) + i64::from(Uuid::new_v4().as_bytes()[0] % 3);
    c.execute("UPDATE health_commands SET status=?,next_attempt_at=?,completed_at=?,error_code=? WHERE id=?",params![if retry{"retry_wait"}else{"failed"},timestamp+delay,if retry{None}else{Some(timestamp)},code,cmd.id.to_string()]).await?;
    Ok(())
}
pub(crate) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async {
        if let Some(cmd) = active(&tx).await? {
            if matches!(cmd.status, CommandStatus::Running) {
                fail(&tx, &cmd, timestamp, code).await?;
            }
        }
        Ok(())
    }
    .await;
    finish(tx, result).await
}
async fn issue(c: &Connection, id: &HealthIdentity) -> Result<Option<HealthIssue>> {
    let r=c.query("SELECT severity,reason,message,wiki_url,compatibility_type FROM health_checks WHERE scope=? AND check_key=?",params![id.scope.text(),id.check_key.clone()]).await?.next().await?.ok_or_else(invariant)?;
    match r.get::<Option<i64>>(0)? {
        Some(severity) if severity > 0 => Ok(Some(HealthIssue {
            identity: id.clone(),
            severity: HealthSeverity::parse(severity)?,
            reason: r.get(1)?,
            message: r.get(2)?,
            wiki_url: r.get(3)?,
            compatibility_type: r.get(4)?,
        })),
        _ => Ok(None),
    }
}
async fn transition(
    c: &Connection,
    cmd: &HealthCommand,
    v: &HealthIssue,
    kind: &str,
    in_grace: bool,
    timestamp: i64,
) -> Result<()> {
    let seq = c
        .query(
            "SELECT seq FROM sqlite_sequence WHERE name='health_transitions'",
            (),
        )
        .await?
        .next()
        .await?
        .map(|r| r.get::<i64>(0))
        .transpose()?
        .unwrap_or(0);
    if seq >= MAX_INTEGER {
        return Err(Error(StatusCode::CONFLICT, "health_generation_exhausted"));
    }
    c.execute("INSERT INTO health_transitions(event_id,epoch,command_id,command_attempt,scope,check_key,kind,in_grace,created_at,severity,reason,message,wiki_url,compatibility_type) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",params![Uuid::new_v4().to_string(),cmd.epoch.to_string(),cmd.id.to_string(),i64::from(cmd.attempts),v.identity.scope.text(),v.identity.check_key.clone(),kind,in_grace,timestamp,v.severity.number(),v.reason.clone(),v.message.clone(),v.wiki_url.clone(),v.compatibility_type.clone()]).await?;
    Ok(())
}
/// A completed observation (including an Error issue) differs from inability to evaluate.
type CheckOutcome = std::result::Result<Option<HealthIssue>, &'static str>;

#[cfg(test)]
async fn publish(
    c: &Connection,
    cmd: &HealthCommand,
    results: std::result::Result<Vec<Option<HealthIssue>>, &'static str>,
    timestamp: i64,
) -> Result<()> {
    let outcomes = match results {
        Ok(values) => values.into_iter().map(Ok).collect(),
        Err(code) => cmd.members.iter().map(|_| Err(code)).collect(),
    };
    publish_outcomes(c, cmd, outcomes, timestamp).await
}

async fn publish_outcomes(
    c: &Connection,
    cmd: &HealthCommand,
    results: Vec<CheckOutcome>,
    timestamp: i64,
) -> Result<()> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if !still_running(&tx, cmd).await? { return Ok(()); }
        // A changed input invalidates this whole attempt, including completed observations.
        let stale = tx.query("SELECT 1 FROM health_command_checks m JOIN health_checks h ON h.scope=m.scope AND h.check_key=m.check_key WHERE m.command_id=? AND (m.captured_generation IS NULL OR m.captured_generation!=h.generation) LIMIT 1", [cmd.id.to_string()]).await?.next().await?.is_some();
        if stale { return fail(&tx,cmd,timestamp,"stale_inputs").await; }
        if results.len() != cmd.members.len() { return Err(invariant()); }
        let error = results.iter().find_map(|v| v.as_ref().err().copied());
        let retry = error.is_some() && cmd.attempts < 3;
        let life = lifecycle(&tx).await?;
        let in_grace = life.grace_phase != "expired";
        for (member, result) in cmd.members.iter().zip(results) {
            match result {
                Ok(new) => {
                    if new.as_ref().is_some_and(|v| v.identity != member.identity || v.severity == HealthSeverity::Ok) { return Err(invariant()); }
                    let old = issue(&tx,&member.identity).await?;
                    match (&old,&new) {
                        (None,Some(v)) => transition(&tx,cmd,v,"issue",in_grace,timestamp).await?,
                        (Some(v),None) => transition(&tx,cmd,v,"restored",in_grace,timestamp).await?,
                        _ => {}
                    }
                    tx.execute("UPDATE health_checks SET observed_generation=generation,observed_epoch=?,checked_at=?,last_error=NULL,severity=?,reason=?,message=?,wiki_url=? WHERE scope=? AND check_key=?",params![cmd.epoch.to_string(),timestamp,new.as_ref().map_or(0,|v|v.severity.number()),new.as_ref().map(|v|v.reason.clone()),new.as_ref().map(|v|v.message.clone()),new.as_ref().map(|v|v.wiki_url.clone()),member.identity.scope.text(),member.identity.check_key.clone()]).await?;
                }
                Err(code) => {
                    // Only the failed member's error changes; its observed payload remains intact.
                    tx.execute("UPDATE health_checks SET last_error=? WHERE scope=? AND check_key=?",params![code,member.identity.scope.text(),member.identity.check_key.clone()]).await?;
                }
            }
            if !retry {
                // Partial grace exhaustion services only its grace reason, preserving other work.
                let grace_only = cmd.is_grace && error.is_some();
                tx.execute("UPDATE health_checks SET pending_reasons=CASE WHEN ? THEN pending_reasons & 15 ELSE 0 END,due_at=CASE WHEN ? AND (pending_reasons & 15)!=0 THEN due_at ELSE NULL END WHERE scope=? AND check_key=? AND generation=?",params![grace_only,grace_only,member.identity.scope.text(),member.identity.check_key.clone(),member.captured_generation]).await?;
            }
        }
        if let Some(code) = error {
            // Retain ALL selected pending markers while retrying: claim must recapture every
            // frozen member, and a previously completed observation may change next attempt.
            if !retry { reset_grace(&tx,cmd,timestamp,60).await?; }
            settle_failure(&tx,cmd,timestamp,code,retry).await?;
            return Ok(());
        }
        if cmd.is_grace {
            if life.epoch != cmd.epoch || life.grace_phase != "rechecking" || life.grace_due_at > timestamp { return Err(invariant()); }
            let missing=tx.query("SELECT 1 FROM health_checks h WHERE startup=1 AND NOT EXISTS(SELECT 1 FROM health_command_checks m WHERE m.command_id=? AND m.scope=h.scope AND m.check_key=h.check_key) LIMIT 1",[cmd.id.to_string()]).await?.next().await?.is_some();
            if missing { return Err(invariant()); }
            for key in selection(&tx,HealthScope::All,"severity>0").await? {
                if let Some(v)=issue(&tx,&key.identity).await? { transition(&tx,cmd,&v,"issue",false,timestamp).await?; }
            }
            tx.execute("UPDATE health_lifecycle SET grace_phase='expired',last_batch_completed_at=? WHERE id=1",[timestamp]).await?;
        }
        tx.execute("UPDATE health_lifecycle SET last_batch_completed_at=? WHERE id=1",[timestamp]).await?;
        tx.execute("UPDATE health_commands SET status='succeeded',completed_at=?,error_code=NULL WHERE id=?",params![timestamp,cmd.id.to_string()]).await?;
        Ok(())
    }.await;
    let result = finish(tx, outcome).await;
    if matches!(&result, Err(Error(_, "health_generation_exhausted"))) {
        c.execute(
            "UPDATE health_lifecycle SET schedule_error='health_invariant' WHERE id=1 AND epoch=?",
            [cmd.epoch.to_string()],
        )
        .await?;
    }
    result
}
pub(crate) async fn run(
    db: &Database,
    client: &crate::providers::RefreshClient,
    cmd: HealthCommand,
) -> Result<()> {
    run_with_deadline(db, client, cmd, Duration::from_secs(30)).await
}
async fn run_with_deadline(
    db: &Database,
    client: &crate::providers::RefreshClient,
    cmd: HealthCommand,
    deadline: Duration,
) -> Result<()> {
    // Keep outcomes outside the cancellable current-check future: deadline must not erase
    // completed independent observations. Communication runs first to observe remote failures
    // before a CDH status probe can populate the shared transport's cooldown.
    let mut results: Vec<CheckOutcome> = cmd.members.iter().map(|_| Err("check_timeout")).collect();
    let mut order: Vec<usize> = (0..cmd.members.len()).collect();
    order.sort_by_key(|&i| cmd.members[i].identity.check_key != "download_client_communication");
    let until = tokio::time::Instant::now() + deadline;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for index in order {
        let probe = evaluate(db, client, &cmd.members[index]);
        tokio::pin!(probe);
        let outcome = loop {
            tokio::select! {
                biased;
                _=tokio::time::sleep_until(until)=>break None,
                _=tick.tick()=>{if !still_running(&db.connect().await?,&cmd).await? { return Ok(()); }},
                value=&mut probe=>break Some(value),
            }
        };
        match outcome {
            Some(value) => results[index] = value,
            None => break,
        }
    }
    publish_outcomes(&db.connect().await?, &cmd, results, now()?).await
}
async fn evaluate(
    db: &Database,
    client: &crate::providers::RefreshClient,
    member: &HealthMember,
) -> CheckOutcome {
    let domain = match member.identity.scope {
        HealthScope::Tv => MediaDomain::Tv,
        HealthScope::Movies => MediaDomain::Movies,
        _ => return Err("check_failed"),
    };
    match member.identity.check_key.as_str() {
        "removed_metadata" => removed_metadata::evaluate(db, &member.identity).await,
        "indexer_search" | "indexer_rss" => indexers::evaluate(db, &member.identity).await,
        "indexer_download_client" => {
            Ok(crate::health_detectors::evaluate_indexer_client(db, domain)
                .await?
                .map(|v| HealthIssue {
                    identity: member.identity.clone(),
                    severity: v.severity,
                    reason: v.reason.into(),
                    message: v.message,
                    wiki_url: v.wiki_url.into(),
                    compatibility_type: v.compatibility_type.into(),
                }))
        }
        "completed_download_handling" => Ok(crate::health_detectors::evaluate_current(
            db, client, domain,
        )
        .await?
        .map(|v| HealthIssue {
            identity: member.identity.clone(),
            severity: HealthSeverity::Warning,
            reason: v.reason.into(),
            message: v.message.into(),
            wiki_url: v.wiki_url.into(),
            compatibility_type: v.compatibility_type.into(),
        })),
        "download_client_communication" => Ok(crate::health_detectors::evaluate_communication(
            db, client, domain,
        )
        .await?
        .map(|v| HealthIssue {
            identity: member.identity.clone(),
            severity: v.severity,
            reason: v.reason.into(),
            message: v.message.into(),
            wiki_url: v.wiki_url.into(),
            compatibility_type: v.compatibility_type.into(),
        })),
        "download_client_root_folder" => {
            download_roots::evaluate(db, client, domain)
                .await
                .map(|issue| {
                    issue.map(|v| HealthIssue {
                        identity: member.identity.clone(),
                        severity: HealthSeverity::Warning,
                        reason: v.reason.into(),
                        message: v.message.into(),
                        wiki_url: v.wiki_url.into(),
                        compatibility_type: v.compatibility_type.into(),
                    })
                })
        }
        _ => Err("check_failed"),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
