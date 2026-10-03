//! Bounded, retention-driven pruning of terminal command history.
//!
//! Only `succeeded`/`failed`/`cancelled` rows are ever selected, and each table-domain keeps
//! its newest [`MIN_KEEP_PER_DOMAIN`] terminal rows regardless of age. Rows that replay,
//! offer or snapshot state still points at are excluded (see [`TABLES`]). Each batch is one
//! short IMMEDIATE transaction with no network or filesystem work, so it serialises with the
//! worker's own IMMEDIATE claim transactions and cannot delete a row mid-claim.
//!
//! The job is stateless and idempotent: it has no persisted run record, a restart simply
//! schedules one early run, and an interrupted batch rolls back as a unit.
use super::*;
use std::time::{Duration, Instant};

/// Terminal rows younger than this (by `completed_at`) are retained. It must exceed the
/// 30-minute search-offer lifetime; see the const assertion below.
pub const RETENTION_SECONDS: i64 = 14 * 24 * 60 * 60;
/// Newest terminal rows kept per (table, media domain) even when older than the retention
/// window, so a quiet domain never loses all of its history.
pub const MIN_KEEP_PER_DOMAIN: i64 = 100;
/// Rows deleted per transaction.
pub const BATCH_ROWS: i64 = 100;
/// Batches per table-domain per run; a larger backlog drains over several runs.
pub const MAX_BATCHES_PER_GROUP: u32 = 20;
/// Wall-clock budget per run, checked between batches (never wrapped around a transaction).
pub const RUN_BUDGET: Duration = Duration::from_secs(5);
/// Delay between runs when the backlog is drained.
pub const RUN_INTERVAL_SECONDS: i64 = 6 * 60 * 60;
/// Delay before the follow-up run when a run stopped on its batch or time bound.
pub const BACKLOG_INTERVAL_SECONDS: i64 = 60;
/// Delay before the first run after worker start.
pub const STARTUP_DELAY_SECONDS: i64 = 10;
/// Search offers stay grabbable for 1800 seconds after creation.
const SEARCH_OFFER_SECONDS: i64 = 1800;
const _: () = assert!(RETENTION_SECONDS > SEARCH_OFFER_SECONDS);
const TERMINAL: &str = "('succeeded','failed','cancelled')";
/// `rescan_commands` additionally has a terminal `skipped` status.
const TERMINAL_RESCAN: &str = "('succeeded','skipped','failed','cancelled')";
const DOMAINS: [&str; 2] = ["tv", "movies"];

struct Table {
    name: &'static str,
    /// The table's actual terminal status set; active statuses are never members.
    terminal: &'static str,
    /// Expression yielding 'tv' or 'movies' for alias `{a}`.
    domain: &'static str,
    /// Extra predicate over alias `t` that must hold for a row to be deletable. `?4` is `now`.
    protect: &'static str,
}
const MEDIA: &str = "{a}.media_type";
/// Per-table replay analysis (verified against migrations and readers of terminal rows):
/// - `commands`: the queue snapshot's `command_id` provenance row is kept.
/// - `rss_commands`: kept while any candidate still referencing it is unsettled; settled
///   candidates carry their own dedup/receipt state and tolerate `ON DELETE SET NULL`.
/// - `search_commands`: kept while any offer is unexpired or selected (the selection trigger
///   would abort anyway); expired unselected offers are deleted with the command.
/// - `manual_import_commands`: kept while its operation journal is neither preview nor
///   complete (interrupted import state).
/// - metadata/blocklist/quality/rescan rows have no inbound references and active-only
///   dedup indexes, so terminal rows are pure history.
const TABLES: [Table; 8] = [
    Table {
        name: "commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "NOT EXISTS(SELECT 1 FROM download_refresh_snapshots s WHERE s.command_id=t.id)",
    },
    Table {
        name: "metadata_refresh_commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "1",
    },
    Table {
        name: "blocklist_clear_commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "1",
    },
    Table {
        name: "rss_commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "NOT EXISTS(SELECT 1 FROM rss_candidates c WHERE c.command_id=t.id AND c.status IN ('pending','prepared','submitting','reconciling','needs_attention'))",
    },
    Table {
        name: "search_commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "NOT EXISTS(SELECT 1 FROM search_results s WHERE s.command_id=t.id AND (s.selected_candidate_id IS NOT NULL OR s.expires_at>?4))",
    },
    Table {
        name: "manual_import_commands",
        terminal: TERMINAL,
        domain: "COALESCE((SELECT CASE o.media_type WHEN 'movie' THEN 'movies' ELSE 'tv' END FROM operations o WHERE o.id={a}.operation_id),'tv')",
        protect: "NOT EXISTS(SELECT 1 FROM import_journal j WHERE j.operation_id=t.operation_id AND j.phase NOT IN ('preview','complete'))",
    },
    Table {
        name: "quality_reset_commands",
        terminal: TERMINAL,
        domain: MEDIA,
        protect: "1",
    },
    Table {
        name: "rescan_commands",
        terminal: TERMINAL_RESCAN,
        domain: MEDIA,
        protect: "1",
    },
];

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub retention_seconds: i64,
    pub min_keep: i64,
    pub batch_rows: i64,
    pub max_batches: u32,
    pub budget: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            retention_seconds: RETENTION_SECONDS,
            min_keep: MIN_KEEP_PER_DOMAIN,
            batch_rows: BATCH_ROWS,
            max_batches: MAX_BATCHES_PER_GROUP,
            budget: RUN_BUDGET,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupReport {
    pub table: &'static str,
    pub media_type: &'static str,
    pub deleted: u64,
    pub failed: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub groups: Vec<GroupReport>,
    /// A batch or time bound stopped at least one group before it ran out of candidates.
    pub more_pending: bool,
    pub failed_groups: u32,
}
impl Report {
    pub fn deleted(&self) -> u64 {
        self.groups.iter().map(|g| g.deleted).sum()
    }
    pub fn deleted_from(&self, table: &str, media_type: &str) -> u64 {
        self.groups
            .iter()
            .filter(|g| g.table == table && g.media_type == media_type)
            .map(|g| g.deleted)
            .sum()
    }
}

fn select_sql(table: &Table) -> String {
    let inner_domain = table.domain.replace("{a}", "x");
    format!(
        "SELECT t.id FROM (SELECT x.id,x.created_at,x.completed_at,row_number() OVER (ORDER BY x.created_at DESC,x.id DESC) AS rn FROM {name} x WHERE x.status IN {terminal} AND {inner_domain}=?1) r JOIN {name} t ON t.id=r.id WHERE r.rn>?2 AND r.completed_at<?3 AND {protect} ORDER BY r.created_at,r.id LIMIT ?5",
        name = table.name,
        terminal = table.terminal,
        protect = table.protect,
    )
}

/// Deletes one batch in one transaction; returns rows deleted.
async fn batch(
    c: &Connection,
    table: &Table,
    domain: &str,
    policy: &Policy,
    now: i64,
) -> Result<u64> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let result = async {
        let mut rows = tx
            .query(
                &select_sql(table),
                params![
                    domain,
                    policy.min_keep,
                    now.saturating_sub(policy.retention_seconds),
                    now,
                    policy.batch_rows
                ],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<String>(0)?);
        }
        drop(rows);
        let mut deleted = 0u64;
        for id in ids {
            if table.name == "search_commands" {
                // The FK is RESTRICT; only unselected offers can be here (selected ones are
                // excluded by the selection predicate and by the database trigger).
                tx.execute(
                    "DELETE FROM search_results WHERE command_id=? AND selected_candidate_id IS NULL",
                    [id.clone()],
                )
                .await?;
            }
            // The status guard is re-asserted inside the deleting transaction.
            deleted += tx
                .execute(
                    &format!(
                        "DELETE FROM {} WHERE id=? AND status IN {}",
                        table.name, table.terminal
                    ),
                    [id],
                )
                .await?;
        }
        Ok(deleted)
    }
    .await;
    finish(tx, result).await
}

/// Runs one bounded pass over every table and domain. A failing group is reported and does
/// not stop the others. Rerunning deletes only what remains eligible.
pub async fn prune(db: &Database, policy: &Policy, now: i64) -> Result<Report> {
    let c = connection(db).await?;
    let started = Instant::now();
    let mut report = Report::default();
    for table in &TABLES {
        for domain in DOMAINS {
            let media_type = if domain == "tv" { "tv" } else { "movies" };
            let mut group = GroupReport {
                table: table.name,
                media_type,
                deleted: 0,
                failed: false,
            };
            let mut last_full = false;
            for _ in 0..policy.max_batches {
                if started.elapsed() >= policy.budget {
                    // Time bound reached before this group was proven drained.
                    report.more_pending = true;
                    last_full = false;
                    break;
                }
                match batch(&c, table, domain, policy, now).await {
                    Ok(n) => {
                        group.deleted += n;
                        last_full = n >= policy.batch_rows as u64;
                        if !last_full {
                            break;
                        }
                    }
                    Err(_) => {
                        group.failed = true;
                        report.failed_groups += 1;
                        last_full = false;
                        eprintln!(
                            "event=housekeeping_error table={} media_type={media_type} code=storage_error",
                            table.name
                        );
                        break;
                    }
                }
            }
            // The batch bound ended on a full batch: more rows may remain.
            if last_full {
                report.more_pending = true;
            }
            if group.deleted > 0 {
                eprintln!(
                    "event=housekeeping_pruned table={} media_type={media_type} rows={}",
                    table.name, group.deleted
                );
            }
            report.groups.push(group);
        }
    }
    Ok(report)
}

/// In-process recurrence state. Not persisted: pruning is idempotent, so a restart just
/// schedules an early run.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    next_run_at: i64,
}
impl Schedule {
    pub fn new(now: i64) -> Self {
        Self {
            next_run_at: now + STARTUP_DELAY_SECONDS,
        }
    }
    pub fn next_run_at(&self) -> i64 {
        self.next_run_at
    }
    pub fn due(&self, now: i64) -> bool {
        now >= self.next_run_at
    }
}

/// Runs the job if due and reschedules it; never returns an error to the worker loop.
pub async fn tick(
    db: &Database,
    schedule: &mut Schedule,
    policy: &Policy,
    now: i64,
) -> Option<Report> {
    if !schedule.due(now) {
        return None;
    }
    // Advance first so a failure cannot make the worker retry every second.
    schedule.next_run_at = now + RUN_INTERVAL_SECONDS;
    match prune(db, policy, now).await {
        Ok(report) => {
            if report.more_pending || report.failed_groups > 0 {
                schedule.next_run_at = now + BACKLOG_INTERVAL_SECONDS;
            }
            eprintln!(
                "event=housekeeping_run deleted={} more_pending={} failed_groups={}",
                report.deleted(),
                report.more_pending,
                report.failed_groups
            );
            Some(report)
        }
        Err(_) => {
            eprintln!("event=housekeeping_error code=storage_error");
            schedule.next_run_at = now + BACKLOG_INTERVAL_SECONDS;
            None
        }
    }
}
