//! Contract tests for bounded terminal command-history pruning (job.005).
//!
//! Fixtures insert rows directly in their final state. The admit and transition triggers of the
//! eight pool tables are dropped from the temporary fixture database only so rows can be seeded
//! without building every provider/series/operation prerequisite; the delete guards, FK
//! actions and the pruner's own SQL are the production ones.
use hrrdarr::{
    commands::{
        self,
        housekeeping::{self, Policy, Schedule},
    },
    db::Database,
    providers,
};
use libsql::{Connection, params};
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

const NOW: i64 = 2_000_000_000;
const TABLES: [&str; 8] = [
    "commands",
    "metadata_refresh_commands",
    "blocklist_clear_commands",
    "rss_commands",
    "search_commands",
    "manual_import_commands",
    "quality_reset_commands",
    "rescan_commands",
];
const DOMAINS: [&str; 2] = ["tv", "movies"];

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("scratch cleanup failed: {e}");
        }
    }
}
struct Fx {
    _scratch: Scratch,
    path: PathBuf,
    db: Arc<Database>,
    c: Connection,
}
async fn fixture() -> Fx {
    let dir = std::env::temp_dir().join(format!("hrrdarr-housekeeping-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("db");
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let c = db.connect().await.unwrap();
    let mut triggers = Vec::new();
    let mut rows = c
        .query(
            &format!(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name IN ({}) AND (name LIKE '%admit%' OR name LIKE '%transition%')",
                TABLES.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(",")
            ),
            (),
        )
        .await
        .unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        triggers.push(r.get::<String>(0).unwrap());
    }
    drop(rows);
    for t in triggers {
        c.execute(&format!("DROP TRIGGER {t}"), ()).await.unwrap();
    }
    // search_results_admit/immutable would require a running fetch; the selected-offer rows
    // below are inserted already selected.
    c.execute("DROP TRIGGER search_results_admit", ())
        .await
        .unwrap();
    c.execute_batch(
        "INSERT INTO series(id,title,path) VALUES(1,'S','/tv/s');
         INSERT INTO seasons(series_id,number) VALUES(1,1);
         INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,1,'E');
         INSERT INTO movie_metadata(id,title) VALUES(1,'M');
         INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/m');
         INSERT INTO operations(id,media_type,episode_id,source,mode,destination,status,message) VALUES('00000000-0000-4000-8000-000000000001','episode',1,'/s','copy','/d','done','');
         INSERT INTO operations(id,media_type,movie_id,source,mode,destination,status,message) VALUES('00000000-0000-4000-8000-000000000002','movie',1,'/s','copy','/d','done','');",
    )
    .await
    .unwrap();
    Fx {
        _scratch: Scratch(dir),
        path,
        db,
        c,
    }
}
fn op_for(domain: &str) -> &'static str {
    if domain == "tv" {
        "00000000-0000-4000-8000-000000000001"
    } else {
        "00000000-0000-4000-8000-000000000002"
    }
}
/// Inserts one row in its final status and returns its id. `completed` applies to terminal
/// rows; active rows get none.
async fn seed(
    c: &Connection,
    table: &str,
    domain: &str,
    created: i64,
    status: &str,
    completed: i64,
) -> String {
    let id = Uuid::new_v4().to_string();
    let active = matches!(status, "queued" | "running" | "retry_wait");
    let attempts = if status == "queued" { 0 } else { 1 };
    let started = if status == "queued" {
        "NULL".to_owned()
    } else {
        created.to_string()
    };
    let done = if active {
        "NULL".to_owned()
    } else {
        completed.to_string()
    };
    let error = if matches!(status, "failed" | "retry_wait") {
        "'interrupted'"
    } else {
        "NULL"
    };
    let series = i64::from(domain == "tv");
    let movie = i64::from(domain == "movies");
    let common = format!(
        "status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,id) VALUES('{status}',{attempts},0,{created},{started},{done},{error},'{id}'"
    );
    let sql = match table {
        "commands" => format!(
            "INSERT INTO commands(name,provider_id,media_type,provider_revision,items_observed,{common})"
        ).replace("VALUES('", &format!("VALUES('refresh_downloads','{}','{domain}',1,0,'", Uuid::new_v4())),
        "metadata_refresh_commands" => {
            let (name, s, m, md) = if domain == "tv" {
                ("refresh_series", "1", "NULL", "NULL")
            } else {
                ("refresh_movie", "NULL", "1", "1")
            };
            format!("INSERT INTO metadata_refresh_commands(name,media_type,series_id,movie_id,metadata_id,external_id,{common})")
                .replace("VALUES('", &format!("VALUES('{name}','{domain}',{s},{m},{md},{},'", created.max(1)))
        }
        "blocklist_clear_commands" => format!(
            "INSERT INTO blocklist_clear_commands(name,media_type,{common})"
        ).replace("VALUES('", &format!("VALUES('clear_blocklist','{domain}','")),
        "rss_commands" => format!(
            "INSERT INTO rss_commands(name,media_type,indexer_id,indexer_revision,client_id,client_revision,fetch_complete,{common})"
        ).replace("VALUES('", &format!("VALUES('rss_sync','{domain}','{}',1,'{}',1,{},'", Uuid::new_v4(), Uuid::new_v4(), i64::from(status == "succeeded"))),
        "search_commands" => format!(
            "INSERT INTO search_commands(mode,captured_target_json,media_type,requested_episode_id,requested_movie_id,indexer_id,indexer_revision,client_id,client_revision,fetch_complete,{common})"
        ).replace("VALUES('", &format!("VALUES('interactive','{{}}','{domain}',{},{},'{}',1,'{}',1,{},'", if domain=="tv" {"1"} else {"NULL"}, if domain=="tv" {"NULL"} else {"1"}, Uuid::new_v4(), Uuid::new_v4(), i64::from(status == "succeeded"))),
        "manual_import_commands" => format!(
            "INSERT INTO manual_import_commands(batch_id,operation_id,{common})"
        ).replace("VALUES('", &format!("VALUES('{}','{}','", Uuid::new_v4(), op_for(domain))),
        "quality_reset_commands" => format!(
            "INSERT INTO quality_reset_commands(media_type,reset_titles,definitions_reset,{common})"
        ).replace("VALUES('", &format!("VALUES('{domain}',0,{},'", if status == "succeeded" { "0" } else { "NULL" })),
        "rescan_commands" => {
            let extra = match status {
                "succeeded" => "0,0,NULL",
                "skipped" => "NULL,NULL,'root_empty'",
                _ => "NULL,NULL,NULL",
            };
            format!("INSERT INTO rescan_commands(media_type,series_id,movie_id,files_adopted,files_removed,skip_reason,{common})")
                .replace("VALUES('", &format!("VALUES('{domain}',{},{},{extra},'", if series == 1 {"1"} else {"NULL"}, if movie == 1 {"1"} else {"NULL"}))
        }
        other => panic!("unknown table {other}"),
    };
    c.execute(&sql, ())
        .await
        .unwrap_or_else(|e| panic!("{table} {status}: {e}\n{sql}"));
    id
}
async fn count(c: &Connection, table: &str, domain: &str) -> i64 {
    let predicate = if table == "manual_import_commands" {
        format!("operation_id='{}'", op_for(domain))
    } else {
        format!("media_type='{domain}'")
    };
    scalar(
        c,
        &format!("SELECT count(*) FROM {table} WHERE {predicate}"),
    )
    .await
}
async fn scalar(c: &Connection, sql: &str) -> i64 {
    c.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
async fn exists(c: &Connection, table: &str, id: &str) -> bool {
    c.query(&format!("SELECT 1 FROM {table} WHERE id=?"), [id])
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .is_some()
}
fn policy(retention: i64, keep: i64) -> Policy {
    Policy {
        retention_seconds: retention,
        min_keep: keep,
        batch_rows: 3,
        max_batches: 100,
        budget: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn prunes_only_old_terminal_rows_in_every_table_and_domain() {
    let f = fixture().await;
    let old = NOW - 100_000;
    let mut old_terminal = Vec::new();
    let mut keep = Vec::new();
    for table in TABLES {
        for (i, domain) in DOMAINS.into_iter().enumerate() {
            for (n, status) in ["succeeded", "failed", "cancelled"].into_iter().enumerate() {
                old_terminal.push((
                    table,
                    seed(&f.c, table, domain, old + n as i64, status, old + 10).await,
                ));
            }
            if table == "rescan_commands" {
                old_terminal.push((
                    table,
                    seed(&f.c, table, domain, old + 5, "skipped", old + 10).await,
                ));
            }
            // Recent terminal row, plus one active row per group (alternating status; the
            // active-scope unique indexes admit one active row per scope).
            keep.push((
                table,
                seed(&f.c, table, domain, NOW - 10, "cancelled", NOW - 5).await,
            ));
            let active = if i == 0 { "queued" } else { "running" };
            keep.push((table, seed(&f.c, table, domain, old - 1, active, 0).await));
        }
    }
    let report = housekeeping::prune(&f.db, &policy(1000, 0), NOW)
        .await
        .unwrap();
    assert_eq!(report.failed_groups, 0);
    assert!(!report.more_pending);
    for (table, id) in &old_terminal {
        assert!(
            !exists(&f.c, table, id).await,
            "{table} old terminal row survived"
        );
    }
    for (table, id) in &keep {
        assert!(
            exists(&f.c, table, id).await,
            "{table} recent/active row was deleted"
        );
    }
    assert_eq!(report.deleted(), old_terminal.len() as u64);
    assert_eq!(report.deleted_from("rescan_commands", "tv"), 4);
    assert_eq!(report.deleted_from("commands", "movies"), 3);
}

#[tokio::test]
async fn keep_floor_and_retention_window_are_honored_and_reruns_are_idempotent() {
    let f = fixture().await;
    let old = NOW - 100_000;
    let mut ids = Vec::new();
    for n in 0..6 {
        ids.push(
            seed(
                &f.c,
                "quality_reset_commands",
                "tv",
                old + n,
                "cancelled",
                old + n,
            )
            .await,
        );
    }
    // The floor counts newest rows, so the two oldest go and the newest four remain.
    let first = housekeeping::prune(&f.db, &policy(1000, 4), NOW)
        .await
        .unwrap();
    assert_eq!(first.deleted(), 2);
    assert!(!exists(&f.c, "quality_reset_commands", &ids[0]).await);
    assert!(!exists(&f.c, "quality_reset_commands", &ids[1]).await);
    for id in &ids[2..] {
        assert!(exists(&f.c, "quality_reset_commands", id).await);
    }
    let again = housekeeping::prune(&f.db, &policy(1000, 4), NOW)
        .await
        .unwrap();
    assert_eq!(again.deleted(), 0);
    assert_eq!(count(&f.c, "quality_reset_commands", "tv").await, 4);
    // A floor at or above the row count deletes nothing, however old.
    assert_eq!(
        housekeeping::prune(&f.db, &policy(0, 4), NOW)
            .await
            .unwrap()
            .deleted(),
        0
    );

    // Retention is strict on completed_at, not created_at: a row created long ago but completed
    // at the cutoff survives; one completed a second earlier is pruned.
    let at_cutoff = seed(&f.c, "rescan_commands", "tv", 1, "cancelled", NOW - 1000).await;
    let before_cutoff = seed(&f.c, "rescan_commands", "tv", 2, "cancelled", NOW - 1001).await;
    let recently_completed = seed(&f.c, "rescan_commands", "tv", 3, "cancelled", NOW - 1).await;
    housekeeping::prune(&f.db, &policy(1000, 0), NOW)
        .await
        .unwrap();
    assert!(exists(&f.c, "rescan_commands", &at_cutoff).await);
    assert!(!exists(&f.c, "rescan_commands", &before_cutoff).await);
    assert!(exists(&f.c, "rescan_commands", &recently_completed).await);
}

#[tokio::test]
async fn keep_floor_is_per_domain_and_equal_numeric_ids_do_not_interfere() {
    let f = fixture().await;
    let old = NOW - 100_000;
    // rescan tv series 1 and movie 1 (and metadata/manual/search with episode 1 / movie 1)
    // share numeric id 1. TV has 3 old rows, movies 2; a shared floor of 2 would delete 3.
    for table in [
        "rescan_commands",
        "metadata_refresh_commands",
        "manual_import_commands",
        "search_commands",
    ] {
        for n in 0..3 {
            seed(&f.c, table, "tv", old + n, "cancelled", old + n).await;
        }
        for n in 0..2 {
            seed(&f.c, table, "movies", old + n, "cancelled", old + n).await;
        }
        let report = housekeeping::prune(&f.db, &policy(1000, 2), NOW)
            .await
            .unwrap();
        assert_eq!(report.deleted_from(table, "tv"), 1, "{table}");
        assert_eq!(report.deleted_from(table, "movies"), 0, "{table}");
        assert_eq!(count(&f.c, table, "tv").await, 2, "{table}");
        assert_eq!(count(&f.c, table, "movies").await, 2, "{table}");
    }
}

#[tokio::test]
async fn replay_and_receipt_dependent_rows_are_protected() {
    let f = fixture().await;
    let c = &f.c;
    let old = NOW - 100_000;

    // commands: the queue snapshot's provenance row stays; an unreferenced twin is pruned.
    let provider = Uuid::new_v4().to_string();
    c.execute("INSERT INTO providers(id,implementation,name,enabled,priority,revision,settings_version,endpoint) VALUES(?,'qbittorrent','Client',1,1,1,1,'http://127.0.0.1:1/')", [provider.clone()]).await.unwrap();
    c.execute("INSERT INTO provider_scopes(provider_id,implementation,media_type,category,recent_priority,older_priority,initial_state,content_layout,sequential_order,first_last_first,add_tags) VALUES(?,'qbittorrent','tv','tv',0,1,'started','default',0,0,0)", [provider.clone()]).await.unwrap();
    let snap = Uuid::new_v4().to_string();
    c.execute("INSERT INTO commands(id,name,provider_id,media_type,provider_revision,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,items_observed) VALUES(?,'refresh_downloads',?,'tv',1,0,'succeeded',1,0,?,?,?,0)", params![snap.clone(), provider.clone(), old, old, old]).await.unwrap();
    c.execute("INSERT INTO download_refresh_snapshots(provider_id,media_type,provider_revision,observed_at,command_id,items_json) VALUES(?,'tv',1,?,?,'[]')", params![provider, old, snap.clone()]).await.unwrap();
    let twin = seed(c, "commands", "tv", old, "cancelled", old).await;

    // rss_commands: unsettled candidate protects; a settled candidate survives its command
    // (ON DELETE SET NULL); an unreferenced command is pruned. Movies mirrors TV with equal ids.
    let mut rss = Vec::new();
    for domain in DOMAINS {
        let protected = seed(c, "rss_commands", domain, old, "cancelled", old).await;
        let settled = seed(c, "rss_commands", domain, old + 1, "cancelled", old).await;
        let bare = seed(c, "rss_commands", domain, old + 2, "cancelled", old).await;
        for (command, status) in [(&protected, "pending"), (&settled, "cancelled")] {
            let (indexer, client): (String, String) = {
                let r = c
                    .query(
                        "SELECT indexer_id,client_id FROM rss_commands WHERE id=?",
                        [command.clone()],
                    )
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap();
                (r.get(0).unwrap(), r.get(1).unwrap())
            };
            let candidate = Uuid::new_v4().to_string();
            let (series, movie) = if domain == "tv" {
                ("1", "NULL")
            } else {
                ("NULL", "1")
            };
            c.execute(&format!("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,created_at,updated_at) VALUES(?,?,?,?,1,?,1,?,'Release',zeroblob(32),{series},{movie},'pending','[]',0,0)"), params![candidate.clone(), command.clone(), domain, indexer, client, "a".repeat(64)]).await.unwrap();
            if status == "cancelled" {
                c.execute(
                    "UPDATE rss_candidates SET status='cancelled',private_payload=NULL WHERE id=?",
                    [candidate],
                )
                .await
                .unwrap();
            }
        }
        rss.push((protected, settled, bare));
    }

    // search_commands: unexpired offer and selected offer protect; an expired unselected offer
    // is deleted together with its command.
    let offer = |command: String, created: i64, selected: Option<String>| {
        let payload = if selected.is_some() {
            None
        } else {
            Some(vec![0u8; 32])
        };
        (command, created, selected, payload)
    };
    let live = seed(c, "search_commands", "tv", old, "succeeded", old).await;
    let expired = seed(c, "search_commands", "tv", old + 1, "succeeded", old).await;
    let selected = seed(c, "search_commands", "movies", old + 2, "succeeded", old).await;
    // Selected offer: the winning candidate is itself retained.
    let winner = Uuid::new_v4().to_string();
    let winner_command = seed(c, "rss_commands", "movies", old + 3, "cancelled", old).await;
    let (idx2, cli2): (String, String) = {
        let r = c
            .query(
                "SELECT indexer_id,client_id FROM rss_commands WHERE id=?",
                [winner_command.clone()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        (r.get(0).unwrap(), r.get(1).unwrap())
    };
    c.execute("INSERT INTO rss_candidates(id,command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,fingerprint,title,private_payload,movie_id,status,decision_reasons_json,created_at,updated_at) VALUES(?,?,'movies',?,1,?,1,?,'Winner',zeroblob(32),1,'pending','[]',0,0)", params![winner.clone(), winner_command, idx2, cli2, "b".repeat(64)]).await.unwrap();
    for (n, (command, created, sel, payload)) in [
        offer(live.clone(), NOW - 600, None),
        offer(expired.clone(), old, None),
        offer(selected.clone(), old, Some(winner.clone())),
    ]
    .into_iter()
    .enumerate()
    {
        c.execute("INSERT INTO search_results(id,command_id,ordinal,fingerprint,title,metadata_json,decision_json,private_payload,created_at,expires_at,selected_candidate_id) VALUES(?,?,0,?,'Offer','{}','{}',?,?,?,?)",
            params![Uuid::new_v4().to_string(), command, format!("{n:064x}"), payload, created, created + 1800, sel]).await.unwrap();
    }

    // manual_import_commands: an interrupted import journal protects its command.
    let staged = seed(c, "manual_import_commands", "tv", old, "failed", old).await;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [op_for("tv")],
    )
    .await
    .unwrap();
    c.execute(
        "UPDATE import_journal SET stage_json='{}',phase='staged' WHERE operation_id=?",
        [op_for("tv")],
    )
    .await
    .unwrap();
    let free = seed(
        c,
        "manual_import_commands",
        "movies",
        old + 1,
        "failed",
        old,
    )
    .await;
    c.execute(
        "INSERT INTO import_journal(operation_id,plan_json,phase) VALUES(?,'{}','preview')",
        [op_for("movies")],
    )
    .await
    .unwrap();

    // The live offer's command is recent enough to also pass the age test elsewhere; make it
    // old by completion so only the unexpired offer protects it.
    c.execute(
        "UPDATE search_commands SET completed_at=? WHERE id=?",
        params![old, live.clone()],
    )
    .await
    .unwrap();
    let report = housekeeping::prune(&f.db, &policy(1000, 0), NOW)
        .await
        .unwrap();
    assert_eq!(report.failed_groups, 0, "{report:?}");

    assert!(
        exists(c, "commands", &snap).await,
        "snapshot provenance command deleted"
    );
    assert!(!exists(c, "commands", &twin).await);
    for (protected, settled, bare) in &rss {
        assert!(
            exists(c, "rss_commands", protected).await,
            "unsettled candidate's command deleted"
        );
        assert!(!exists(c, "rss_commands", settled).await);
        assert!(!exists(c, "rss_commands", bare).await);
    }
    // Settled candidate and the pending one remain; the settled one lost only its command link.
    assert_eq!(
        scalar(
            c,
            "SELECT count(*) FROM rss_candidates WHERE command_id IS NULL AND status='cancelled'"
        )
        .await,
        2
    );
    // Two protected-by-candidate commands (tv, movies) plus the search winner's own command.
    assert_eq!(
        scalar(
            c,
            "SELECT count(*) FROM rss_candidates WHERE command_id IS NOT NULL AND status='pending'"
        )
        .await,
        3
    );
    assert!(
        exists(c, "search_commands", &live).await,
        "unexpired offer's command deleted"
    );
    assert!(
        exists(c, "search_commands", &selected).await,
        "selected offer's command deleted"
    );
    assert!(!exists(c, "search_commands", &expired).await);
    assert_eq!(scalar(c, "SELECT count(*) FROM search_results").await, 2);
    assert!(
        exists(c, "manual_import_commands", &staged).await,
        "interrupted import's command deleted"
    );
    assert!(!exists(c, "manual_import_commands", &free).await);
    // Rerun is a no-op: protection is stable, not a one-shot.
    assert_eq!(
        housekeeping::prune(&f.db, &policy(1000, 0), NOW)
            .await
            .unwrap()
            .deleted(),
        0
    );
    // Once the offer has expired the protected command is no longer needed.
    let later = NOW + 1800;
    let report = housekeeping::prune(&f.db, &policy(1000, 0), later)
        .await
        .unwrap();
    assert!(!exists(c, "search_commands", &live).await);
    assert!(exists(c, "search_commands", &selected).await);
    assert!(report.deleted_from("search_commands", "tv") >= 1);
}

#[tokio::test]
async fn batches_are_bounded_by_rows_batches_and_wall_clock() {
    let f = fixture().await;
    let old = NOW - 100_000;
    for n in 0..7 {
        seed(
            &f.c,
            "blocklist_clear_commands",
            "tv",
            old + n,
            "cancelled",
            old,
        )
        .await;
    }
    let bounded = Policy {
        batch_rows: 3,
        max_batches: 2,
        ..policy(1000, 0)
    };
    let first = housekeeping::prune(&f.db, &bounded, NOW).await.unwrap();
    assert_eq!(first.deleted(), 6);
    assert!(
        first.more_pending,
        "a full final batch must report remaining work"
    );
    let second = housekeeping::prune(&f.db, &bounded, NOW).await.unwrap();
    assert_eq!(second.deleted(), 1);
    assert!(!second.more_pending);
    assert_eq!(
        housekeeping::prune(&f.db, &bounded, NOW)
            .await
            .unwrap()
            .deleted(),
        0
    );

    for n in 0..4 {
        seed(
            &f.c,
            "blocklist_clear_commands",
            "movies",
            old + n,
            "cancelled",
            old,
        )
        .await;
    }
    let expired = Policy {
        budget: Duration::ZERO,
        ..policy(1000, 0)
    };
    let none = housekeeping::prune(&f.db, &expired, NOW).await.unwrap();
    assert_eq!(none.deleted(), 0);
    assert!(none.more_pending);
    assert_eq!(count(&f.c, "blocklist_clear_commands", "movies").await, 4);
}

#[tokio::test]
async fn failed_batch_rolls_back_without_blocking_other_tables_and_rerun_recovers() {
    let f = fixture().await;
    let old = NOW - 100_000;
    let mut poisoned = Vec::new();
    for n in 0..3 {
        poisoned.push(seed(&f.c, "rescan_commands", "tv", old + n, "cancelled", old).await);
    }
    let other = seed(&f.c, "quality_reset_commands", "tv", old, "cancelled", old).await;
    f.c.execute(&format!("CREATE TRIGGER fixture_poison BEFORE DELETE ON rescan_commands WHEN OLD.id='{}' BEGIN SELECT RAISE(ABORT,'fixture'); END", poisoned[2]), ()).await.unwrap();
    let report = housekeeping::prune(&f.db, &policy(1000, 0), NOW)
        .await
        .unwrap();
    assert_eq!(report.failed_groups, 1);
    assert!(!exists(&f.c, "quality_reset_commands", &other).await);
    // The batch is atomic: rows deleted before the failing one were rolled back with it.
    assert_eq!(count(&f.c, "rescan_commands", "tv").await, 3);
    f.c.execute("DROP TRIGGER fixture_poison", ())
        .await
        .unwrap();
    let report = housekeeping::prune(&f.db, &policy(1000, 0), NOW)
        .await
        .unwrap();
    assert_eq!((report.failed_groups, report.deleted()), (0, 3));
}

#[tokio::test]
async fn interrupted_run_and_process_restart_resume_without_overdeleting() {
    let f = fixture().await;
    let old = NOW - 100_000;
    for n in 0..8 {
        seed(
            &f.c,
            "metadata_refresh_commands",
            "movies",
            old + n,
            "failed",
            old,
        )
        .await;
    }
    // Interrupted: the future is dropped between batches (as a shutdown would).
    let slice = Policy {
        batch_rows: 3,
        max_batches: 1,
        ..policy(1000, 2)
    };
    assert_eq!(
        housekeeping::prune(&f.db, &slice, NOW)
            .await
            .unwrap()
            .deleted(),
        3
    );
    let Fx {
        path,
        db,
        c,
        _scratch,
        ..
    } = f;
    drop(c);
    drop(db);
    // Restart: reopen and rerun with a fresh schedule; floor still honored, no over-delete.
    let db = Arc::new(Database::open_local(&path).await.unwrap());
    let c = db.connect().await.unwrap();
    assert_eq!(count(&c, "metadata_refresh_commands", "movies").await, 5);
    let report = housekeeping::prune(&db, &policy(1000, 2), NOW)
        .await
        .unwrap();
    assert_eq!(report.deleted(), 3);
    assert_eq!(count(&c, "metadata_refresh_commands", "movies").await, 2);
    drop(_scratch);
}

#[tokio::test]
async fn schedule_runs_once_per_interval_and_retries_backlog_sooner() {
    let f = fixture().await;
    let old = NOW - 100_000;
    let mut schedule = Schedule::new(NOW);
    assert_eq!(
        schedule.next_run_at(),
        NOW + housekeeping::STARTUP_DELAY_SECONDS
    );
    let p = policy(1000, 0);
    assert!(
        housekeeping::tick(
            &f.db,
            &mut schedule,
            &p,
            NOW + housekeeping::STARTUP_DELAY_SECONDS - 1
        )
        .await
        .is_none()
    );
    for n in 0..4 {
        seed(&f.c, "rescan_commands", "tv", old + n, "cancelled", old).await;
    }
    let due = NOW + housekeeping::STARTUP_DELAY_SECONDS;
    let bounded = Policy {
        batch_rows: 3,
        max_batches: 1,
        ..p
    };
    let report = housekeeping::tick(&f.db, &mut schedule, &bounded, due)
        .await
        .unwrap();
    assert_eq!(report.deleted(), 3);
    // Backlog remains: the follow-up is soon, and exactly one run happens per due tick.
    assert_eq!(
        schedule.next_run_at(),
        due + housekeeping::BACKLOG_INTERVAL_SECONDS
    );
    assert!(
        housekeeping::tick(&f.db, &mut schedule, &bounded, due + 1)
            .await
            .is_none()
    );
    let report = housekeeping::tick(
        &f.db,
        &mut schedule,
        &bounded,
        due + housekeeping::BACKLOG_INTERVAL_SECONDS,
    )
    .await
    .unwrap();
    assert_eq!(report.deleted(), 1);
    let drained = due + housekeeping::BACKLOG_INTERVAL_SECONDS;
    assert_eq!(
        schedule.next_run_at(),
        drained + housekeeping::RUN_INTERVAL_SECONDS
    );
    assert!(
        housekeeping::tick(&f.db, &mut schedule, &bounded, drained + 1)
            .await
            .is_none()
    );
    assert!(
        housekeeping::tick(
            &f.db,
            &mut schedule,
            &bounded,
            drained + housekeeping::RUN_INTERVAL_SECONDS
        )
        .await
        .is_some()
    );
}

/// A prune that has to wait behind another connection's IMMEDIATE (claim-style) transaction
/// observes the committed state: a still-active old row is never deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prune_waits_for_an_in_flight_claim_and_never_deletes_the_claimed_row() {
    let f = fixture().await;
    let old = NOW - 100_000;
    let claimed = seed(&f.c, "rescan_commands", "tv", old, "queued", 0).await;
    let done = seed(&f.c, "rescan_commands", "movies", old, "cancelled", old).await;
    let claimer = f.db.connect().await.unwrap();
    let tx = claimer
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .unwrap();
    tx.execute(
        "UPDATE rescan_commands SET status='running',attempts=1,started_at=? WHERE id=?",
        params![NOW, claimed.clone()],
    )
    .await
    .unwrap();
    let db = f.db.clone();
    let prune =
        tokio::spawn(async move { housekeeping::prune(&db, &policy(0, 0), NOW).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !prune.is_finished(),
        "prune must wait for the claim transaction"
    );
    tx.commit().await.unwrap();
    let report = prune.await.unwrap();
    assert_eq!(report.failed_groups, 0);
    assert!(exists(&f.c, "rescan_commands", &claimed).await);
    assert!(!exists(&f.c, "rescan_commands", &done).await);
}

/// Real worker claims concurrently with an aggressive prune loop: every seeded row is executed
/// to a terminal state before it can be deleted, and no prune batch ever fails on a guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_worker_claims_race_aggressive_pruning_without_losing_a_row() {
    let f = fixture().await;
    f.c.execute_batch("CREATE TABLE hk_audit(status TEXT NOT NULL); CREATE TRIGGER hk_audit_delete AFTER DELETE ON blocklist_clear_commands BEGIN INSERT INTO hk_audit(status) VALUES(OLD.status); END;").await.unwrap();
    const ROUNDS: i64 = 3;
    const ROWS: i64 = ROUNDS * 2;
    let (_router, client) = providers::router_with_refresh(f.db.clone(), None);
    let runtime = commands::start(f.db.clone(), client).await.unwrap();
    let aggressive = Policy {
        retention_seconds: 0,
        min_keep: 0,
        batch_rows: 1,
        max_batches: 10,
        budget: Duration::from_secs(5),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    // The active-scope index admits one active clear per domain, so feed one TV and one movie
    // row per round, old by creation time and unclaimed, while pruning runs continuously.
    let mut fed = 0;
    loop {
        if fed < ROUNDS && scalar(&f.c, "SELECT count(*) FROM blocklist_clear_commands WHERE status IN ('queued','running','retry_wait')").await == 0 {
            for domain in DOMAINS {
                seed(&f.c, "blocklist_clear_commands", domain, 1 + fed, "queued", 0).await;
            }
            fed += 1;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let report = housekeeping::prune(&f.db, &aggressive, now).await.unwrap();
        assert_eq!(report.failed_groups, 0);
        if scalar(&f.c, "SELECT count(*) FROM hk_audit").await == ROWS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not finish and prune all rows"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        scalar(
            &f.c,
            "SELECT count(*) FROM hk_audit WHERE status='succeeded'"
        )
        .await,
        ROWS,
        "a row was pruned before succeeding"
    );
    assert_eq!(
        scalar(&f.c, "SELECT count(*) FROM blocklist_clear_commands").await,
        0
    );
    runtime.shutdown().await;
}

/// The worker registers the recurring job itself: after the startup delay it prunes rows past
/// the default keep floor without any API call, and leaves the floor intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn started_worker_runs_the_recurring_job_once_and_keeps_the_floor() {
    let f = fixture().await;
    let total = housekeeping::MIN_KEEP_PER_DOMAIN + 5;
    f.c.execute("BEGIN", ()).await.unwrap();
    for n in 0..total {
        seed(
            &f.c,
            "quality_reset_commands",
            "tv",
            1000 + n,
            "cancelled",
            1000 + n,
        )
        .await;
    }
    f.c.execute("COMMIT", ()).await.unwrap();
    let (_router, client) = providers::router_with_refresh(f.db.clone(), None);
    let runtime = commands::start(f.db.clone(), client).await.unwrap();
    let deadline = std::time::Instant::now()
        + Duration::from_secs(housekeeping::STARTUP_DELAY_SECONDS as u64 + 15);
    while count(&f.c, "quality_reset_commands", "tv").await != housekeeping::MIN_KEEP_PER_DOMAIN {
        assert!(std::time::Instant::now() < deadline, "worker never pruned");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Wait past another worker tick: the next run is hours away, and the floor holds regardless.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        count(&f.c, "quality_reset_commands", "tv").await,
        housekeeping::MIN_KEEP_PER_DOMAIN
    );
    runtime.shutdown().await;
}
