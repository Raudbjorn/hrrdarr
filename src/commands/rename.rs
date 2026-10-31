//! RenameFiles / RenameSeries / RenameMovie and the series/movie folder renames (cmd.011,
//! cmd.012). One durable, domain-typed command table. Execution plans from current stored facts
//! and naming configuration (the same code the previews use), journals each file group before
//! touching the disk, moves with atomic no-replace renames (media file plus sidecars), and
//! commits the path update, the event row and the counters in one transaction. A crash leaves a
//! `planned` journal row whose true state is read back from the filesystem and then rolled
//! forward or back; nothing is guessed.
use super::*;
use crate::{
    naming::{
        movie_preview,
        plan::{self, PlannedFile, PlannedFolder, RenameReason, RenameStatus},
    },
    rename::{
        events::{self, RenameEvent, RenameEventKind, RenameOwner},
        fsops::{self, Ident, Kind, PairState, Sidecar},
    },
};
use std::collections::BTreeSet;

mod legacy;
pub use legacy::{
    LegacyAction, LegacyLayout, LegacyQuery, LegacyRecoveryInput, LegacyRecoveryReport,
    LegacyRefusal, LegacyRenameItem, LegacySidecar,
};

/// Owners per command and explicitly selected files per command.
const MAX_OWNERS: usize = 50;
const MAX_FILES: usize = 200;
/// Journaled items per run; the remainder is reported as `bounded` and a rerun continues.
const MAX_ITEMS: usize = 1000;
const MAX_DETAIL_ITEMS: i64 = 1000;
const BUSY_RETRY_SECONDS: i64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RenameName {
    RenameFiles,
    RenameSeries,
    RenameMovie,
    RenameSeriesFolder,
    RenameMovieFolder,
}
impl RenameName {
    fn text(self) -> &'static str {
        match self {
            Self::RenameFiles => "rename_files",
            Self::RenameSeries => "rename_series",
            Self::RenameMovie => "rename_movie",
            Self::RenameSeriesFolder => "rename_series_folder",
            Self::RenameMovieFolder => "rename_movie_folder",
        }
    }
    fn parse(v: &str) -> Result<Self> {
        Ok(match v {
            "rename_files" => Self::RenameFiles,
            "rename_series" => Self::RenameSeries,
            "rename_movie" => Self::RenameMovie,
            "rename_series_folder" => Self::RenameSeriesFolder,
            "rename_movie_folder" => Self::RenameMovieFolder,
            _ => return Err(bad()),
        })
    }
    fn folder(self) -> bool {
        matches!(self, Self::RenameSeriesFolder | Self::RenameMovieFolder)
    }
}
/// Typed target. TV targets carry `series_ids`, movie targets `movie_ids`; `file_ids` are
/// episode-file or movie-file IDs of the single owner and are required for `rename_files` only.
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RenameTarget {
    pub media_type: MediaDomain,
    #[serde(default)]
    pub series_ids: Vec<i64>,
    #[serde(default)]
    pub movie_ids: Vec<i64>,
    #[serde(default)]
    pub file_ids: Vec<i64>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RenameInput {
    pub name: RenameName,
    pub target: RenameTarget,
    /// Fence against configuration changes after the preview the user looked at.
    #[serde(default, deserialize_with = "super::present")]
    #[ts(optional)]
    pub expected_naming_revision: Option<i64>,
    pub priority: CommandPriority,
}
#[derive(Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum RenameItemState {
    Planned,
    Committed,
    Failed,
    RolledBack,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RenameItem {
    pub kind: RenameEventKind,
    pub series_id: Option<i64>,
    pub movie_id: Option<i64>,
    pub file_id: Option<i64>,
    pub old_path: String,
    pub new_path: String,
    pub state: RenameItemState,
    pub error_code: Option<String>,
    pub sidecar_count: i64,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RenameCommand {
    pub id: Uuid,
    pub name: RenameName,
    pub target: RenameTarget,
    pub expected_naming_revision: Option<i64>,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub cancel_requested: bool,
    pub items_renamed: i64,
    pub items_unchanged: i64,
    pub items_failed: i64,
    /// More changes existed than one run handles; run the command again to continue.
    pub bounded: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RenameCommandDetail {
    #[serde(flatten)]
    pub command: RenameCommand,
    pub items: Vec<RenameItem>,
}
const COLUMNS: &str = "id,name,media_type,expected_naming_revision,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,cancel_requested,items_renamed,items_unchanged,items_failed,bounded";
/// Sync column mapping; targets are filled by [`fill`] once the statement is released.
fn parse(r: &libsql::Row) -> Result<RenameCommand> {
    Ok(RenameCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: RenameName::parse(&r.get::<String>(1)?)?,
        target: RenameTarget {
            media_type: MediaDomain::parse(&r.get::<String>(2)?).map_err(|_| bad())?,
            series_ids: vec![],
            movie_ids: vec![],
            file_ids: vec![],
        },
        expected_naming_revision: r.get(3)?,
        priority: if r.get::<i64>(4)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(5)?)?,
        attempts: r.get::<i64>(6)? as u8,
        next_attempt_at: r.get(7)?,
        created_at: r.get(8)?,
        started_at: r.get(9)?,
        completed_at: r.get(10)?,
        error_code: r.get(11)?,
        cancel_requested: r.get::<i64>(12)? == 1,
        items_renamed: r.get(13)?,
        items_unchanged: r.get(14)?,
        items_failed: r.get(15)?,
        bounded: r.get::<i64>(16)? == 1,
    })
}
async fn fill(c: &Connection, command: &mut RenameCommand) -> Result<()> {
    let mut rows = c
        .query(
            "SELECT series_id,movie_id,file_id FROM rename_command_targets WHERE command_id=? ORDER BY ordinal",
            [command.id.to_string()],
        )
        .await?;
    let target = &mut command.target;
    while let Some(t) = rows.next().await? {
        if let Some(s) = t.get::<Option<i64>>(0)? {
            if !target.series_ids.contains(&s) {
                target.series_ids.push(s);
            }
        }
        if let Some(m) = t.get::<Option<i64>>(1)? {
            if !target.movie_ids.contains(&m) {
                target.movie_ids.push(m);
            }
        }
        if let Some(f) = t.get::<Option<i64>>(2)? {
            target.file_ids.push(f);
        }
    }
    Ok(())
}
async fn read(c: &Connection, id: Uuid) -> Result<RenameCommand> {
    let r = c
        .query(
            &format!("SELECT {COLUMNS} FROM rename_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "rename_command_not_found"))?;
    let mut command = parse(&r)?;
    fill(c, &mut command).await?;
    Ok(command)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<RenameCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "rename_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/rename/commands", get(list).post(create))
        .route("/api/v1/rename/commands/{id}", get(detail))
        .route("/api/v1/rename/commands/{id}/cancel", post(cancel))
        .route(
            "/api/v1/rename/legacy-recovery",
            get(legacy::inventory).post(legacy::recover),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn(request_deadline))
        .with_state(db)
}
async fn request_deadline(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(r) => r,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "rename_command_timeout").into_response(),
    }
}
fn distinct(ids: &[i64], max: usize) -> bool {
    !ids.is_empty()
        && ids.len() <= max
        && ids.iter().all(|i| (1..=MAX_REVISION).contains(i))
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}
async fn naming_revision(c: &Connection, media: MediaDomain) -> Result<i64> {
    Ok(c.query(
        "SELECT revision FROM naming_settings WHERE domain=?",
        [domain(media)],
    )
    .await?
    .next()
    .await?
    .ok_or_else(bad)?
    .get(0)?)
}
async fn admit(c: &Connection, input: &RenameInput, timestamp: i64) -> Result<RenameCommand> {
    let t = &input.target;
    let tv = t.media_type == MediaDomain::Tv;
    let (owners, other) = if tv {
        (&t.series_ids, &t.movie_ids)
    } else {
        (&t.movie_ids, &t.series_ids)
    };
    let files = input.name == RenameName::RenameFiles;
    let name_ok = match input.name {
        RenameName::RenameFiles => true,
        RenameName::RenameSeries | RenameName::RenameSeriesFolder => tv,
        RenameName::RenameMovie | RenameName::RenameMovieFolder => !tv,
    };
    if !name_ok
        || !other.is_empty()
        || !distinct(owners, if files { 1 } else { MAX_OWNERS })
        || (files && !distinct(&t.file_ids, MAX_FILES))
        || (!files && !t.file_ids.is_empty())
        || input
            .expected_naming_revision
            .is_some_and(|r| !(1..=MAX_REVISION).contains(&r))
    {
        return Err(bad());
    }
    let owner_table = if tv { "series" } else { "movies" };
    for id in owners {
        if c.query(&format!("SELECT 1 FROM {owner_table} WHERE id=?"), [*id])
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(Error(StatusCode::NOT_FOUND, "rename_target_not_found"));
        }
    }
    if files {
        let (table, column) = if tv {
            ("episode_files", "series_id")
        } else {
            ("movie_files", "movie_id")
        };
        for f in &t.file_ids {
            if c.query(
                &format!("SELECT 1 FROM {table} WHERE id=? AND {column}=?"),
                params![*f, owners[0]],
            )
            .await?
            .next()
            .await?
            .is_none()
            {
                return Err(Error(StatusCode::NOT_FOUND, "rename_target_not_found"));
            }
        }
    }
    if let Some(expected) = input.expected_naming_revision {
        if expected != naming_revision(c, t.media_type).await? {
            return Err(Error(StatusCode::CONFLICT, "naming_revision_changed"));
        }
    }
    let column = if tv { "series_id" } else { "movie_id" };
    for id in owners {
        if c.query(&format!("SELECT 1 FROM rename_command_targets t JOIN rename_commands r ON r.id=t.command_id WHERE r.status IN ('queued','running','retry_wait') AND t.{column}=?"), [*id]).await?.next().await?.is_some() {
            return Err(conflict());
        }
    }
    if command_capacity(c).await? >= MAX_COMMANDS {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
    }
    let id = Uuid::new_v4();
    c.execute("INSERT INTO rename_commands(id,name,media_type,expected_naming_revision,priority,status,attempts,next_attempt_at,created_at) VALUES(?,?,?,?,?,'queued',0,?,?)",
        params![id.to_string(), input.name.text(), domain(t.media_type), input.expected_naming_revision, input.priority.number(), timestamp, timestamp]).await?;
    let mut ordinal = 0_i64;
    if files {
        for f in &t.file_ids {
            c.execute("INSERT INTO rename_command_targets(command_id,ordinal,series_id,movie_id,file_id) VALUES(?,?,?,?,?)",
                params![id.to_string(), ordinal, if tv {Some(owners[0])} else {None}, if tv {None} else {Some(owners[0])}, *f]).await?;
            ordinal += 1;
        }
    } else {
        for o in owners {
            c.execute("INSERT INTO rename_command_targets(command_id,ordinal,series_id,movie_id) VALUES(?,?,?,?)",
                params![id.to_string(), ordinal, if tv {Some(*o)} else {None}, if tv {None} else {Some(*o)}]).await?;
            ordinal += 1;
        }
    }
    read(c, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RenameInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RenameCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async { bounded(admit(&tx, &input, now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RenameCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let total: i64 = c
        .query("SELECT count(*) FROM rename_commands WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?)", params![media, media, status, status])
        .await?.next().await?.ok_or_else(bad)?.get(0)?;
    let mut rows = c
        .query(&format!("SELECT {COLUMNS} FROM rename_commands WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?) ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),
            params![media, media, status, status, i64::from(q.limit), i64::from(q.offset)])
        .await?;
    let mut items = vec![];
    while let Some(r) = rows.next().await? {
        items.push(parse(&r)?);
    }
    drop(rows);
    for item in &mut items {
        fill(&c, item).await?;
    }
    bounded(ApiPage {
        items,
        total,
        limit: q.limit,
        offset: q.offset,
    })
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RenameCommandDetail>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let command = read(&c, id).await?;
    let mut rows = c
        .query("SELECT kind,series_id,movie_id,file_id,old_path,new_path,state,error_code,json_array_length(sidecars_json) FROM rename_items WHERE command_id=? ORDER BY id LIMIT ?", params![id.to_string(), MAX_DETAIL_ITEMS])
        .await?;
    let mut items = vec![];
    while let Some(r) = rows.next().await? {
        items.push(RenameItem {
            kind: if r.get::<String>(0)? == "folder" {
                RenameEventKind::Folder
            } else {
                RenameEventKind::File
            },
            series_id: r.get(1)?,
            movie_id: r.get(2)?,
            file_id: r.get(3)?,
            old_path: r.get(4)?,
            new_path: r.get(5)?,
            state: match r.get::<String>(6)?.as_str() {
                "planned" => RenameItemState::Planned,
                "committed" => RenameItemState::Committed,
                "failed" => RenameItemState::Failed,
                "rolled_back" => RenameItemState::RolledBack,
                _ => return Err(bad()),
            },
            error_code: r.get(7)?,
            sidecar_count: r.get(8)?,
        });
    }
    bounded(RenameCommandDetail { command, items })
}
async fn cancel(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RenameCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    // Queued work cancels at once; a running command stops before its next item (finished
    // renames are durable and stay; the item in flight completes or rolls back whole).
    let outcome = async {
        tx.execute("UPDATE rename_commands SET status='cancelled',completed_at=? WHERE id=? AND status IN ('queued','retry_wait')", params![now()?, id.to_string()]).await?;
        tx.execute("UPDATE rename_commands SET cancel_requested=1 WHERE id=? AND status='running'", [id.to_string()]).await?;
        bounded(read(&tx, id).await?)
    }.await;
    finish(tx, outcome).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE rename_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'", params![timestamp, timestamp, code]).await?;
    Ok(())
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<RenameCommand> {
    c.execute("UPDATE rename_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?", params![timestamp, id.to_string()]).await?;
    read(c, id).await
}

// ---- execution -------------------------------------------------------------------------------

struct Journal {
    id: i64,
    command_id: String,
    kind: Kind,
    series_id: Option<i64>,
    movie_id: Option<i64>,
    file_id: Option<i64>,
    old: String,
    new: String,
    ident: Ident,
    sidecars: Vec<Sidecar>,
    root: String,
}
impl Journal {
    fn pairs(&self) -> Vec<(String, String, Ident)> {
        let mut v = vec![(self.old.clone(), self.new.clone(), self.ident)];
        v.extend(
            self.sidecars
                .iter()
                .map(|s| (s.old.clone(), s.new.clone(), s.ident)),
        );
        v
    }
}
/// Moves every pair still at its source; returns the code of the first failure.
fn forward(j: &Journal) -> std::result::Result<(), &'static str> {
    for (i, (old, new, ident)) in j.pairs().into_iter().enumerate() {
        let kind = if i == 0 { j.kind } else { Kind::File };
        match fsops::pair_state(&old, &new, kind, ident) {
            Ok(PairState::Source) => {
                fsops::move_path(&j.root, &old, &new, kind, ident).map_err(|e| e.code())?
            }
            Ok(PairState::Moved) => {}
            Ok(PairState::Anomaly) => return Err("filesystem_conflict"),
            Err(e) => return Err(e.code()),
        }
    }
    Ok(())
}
/// Moves back everything that moved. Reports whether the original layout is fully restored.
fn rollback(j: &Journal) -> bool {
    let mut restored = true;
    for (i, (old, new, ident)) in j.pairs().into_iter().enumerate().rev() {
        let kind = if i == 0 { j.kind } else { Kind::File };
        match fsops::pair_state(&old, &new, kind, ident) {
            Ok(PairState::Source) => {}
            Ok(PairState::Moved) => {
                restored &= fsops::move_path(&j.root, &new, &old, kind, ident).is_ok();
            }
            _ => restored = false,
        }
    }
    restored
}
fn parse_journal(r: &libsql::Row) -> Result<Journal> {
    let kind = if r.get::<String>(2)? == "folder" {
        Kind::Dir
    } else {
        Kind::File
    };
    Ok(Journal {
        id: r.get(0)?,
        command_id: r.get(1)?,
        kind,
        series_id: r.get(3)?,
        movie_id: r.get(4)?,
        file_id: r.get(5)?,
        old: r.get(6)?,
        new: r.get(7)?,
        ident: serde_json::from_str(&r.get::<String>(8)?).map_err(|_| bad())?,
        sidecars: serde_json::from_str(&r.get::<String>(9)?).map_err(|_| bad())?,
        root: String::new(),
    })
}
/// Sets the root boundary: the owner's current folder, or a folder's parent. `false` when the
/// owner no longer exists.
async fn bind_root(c: &Connection, j: &mut Journal) -> Result<bool> {
    if j.kind == Kind::Dir {
        j.root = j
            .old
            .trim_end_matches('/')
            .rsplit_once('/')
            .map_or("/", |p| p.0)
            .to_owned();
        return Ok(true);
    }
    let (table, id) = match (j.series_id, j.movie_id) {
        (Some(s), _) => ("series", s),
        (_, Some(m)) => ("movies", m),
        _ => return Ok(false),
    };
    let Some(p) = c
        .query(&format!("SELECT path FROM {table} WHERE id=?"), [id])
        .await?
        .next()
        .await?
    else {
        return Ok(false);
    };
    j.root = p.get::<String>(0)?;
    Ok(true)
}
const JOURNAL_COLUMNS: &str =
    "id,command_id,kind,series_id,movie_id,file_id,old_path,new_path,identity_json,sidecars_json";
// A new command also recovers terminal predecessors for its owners, before replanning their paths.
const JOURNAL_SCOPE: &str = "state='planned' AND (?1 IS NULL OR command_id=?1 OR EXISTS(SELECT 1 FROM rename_command_targets t WHERE t.command_id=?1 AND (t.series_id=rename_items.series_id OR t.movie_id=rename_items.movie_id)))";
async fn settle_journal(
    c: &Connection,
    j: &Journal,
    state: &str,
    code: Option<&str>,
) -> Result<()> {
    c.execute(
        "UPDATE rename_items SET state=?,error_code=? WHERE id=? AND state='planned'",
        params![state, code, j.id],
    )
    .await?;
    Ok(())
}
/// Commit: DB paths, journal state, event row and counters together.
async fn commit(db: &Database, j: &Journal) -> std::result::Result<RenameEvent, &'static str> {
    let c = connection(db).await.map_err(|_| "storage_error")?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|_| "storage_error")?;
    let result: std::result::Result<RenameEvent, &'static str> = async {
        let tv = j.series_id.is_some();
        let owned = |e: libsql::Error| -> &'static str {
            let text = format!("{e:?}");
            if text.contains("UNIQUE") { "destination_in_use" } else { "storage_error" }
        };
        if j.kind == Kind::File {
            let table = if tv { "episode_files" } else { "movie_files" };
            let n = tx.execute(&format!("UPDATE {table} SET path=? WHERE id=? AND path=?"), params![j.new.clone(), j.file_id, j.old.clone()]).await.map_err(owned)?;
            if n != 1 {
                return Err("target_changed");
            }
        } else if tv {
            let id = j.series_id.unwrap_or(0);
            let n = tx.execute("UPDATE series SET path=? WHERE id=? AND path=?", params![j.new.clone(), id, j.old.clone()]).await.map_err(owned)?;
            if n != 1 { return Err("target_changed"); }
            tx.execute("UPDATE episode_files SET path=?1||substr(path,length(?2)+1) WHERE series_id=?3 AND substr(path,1,length(?2)+1)=?2||'/'", params![j.new.clone(), j.old.clone(), id]).await.map_err(owned)?;
        } else {
            let id = j.movie_id.unwrap_or(0);
            let n = tx.execute("UPDATE movies SET path=? WHERE id=? AND path=?", params![j.new.clone(), id, j.old.clone()]).await.map_err(owned)?;
            if n != 1 { return Err("target_changed"); }
            tx.execute("UPDATE movie_files SET path=?1||substr(path,length(?2)+1) WHERE movie_id=?3 AND substr(path,1,length(?2)+1)=?2||'/'", params![j.new.clone(), j.old.clone(), id]).await.map_err(owned)?;
        }
        let n = tx.execute("UPDATE rename_items SET state='committed',error_code=NULL WHERE id=? AND state='planned'", [j.id]).await.map_err(owned)?;
        if n != 1 { return Err("target_changed"); }
        let event_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO rename_events(id,kind,media_type,series_id,movie_id,file_id,command_id,old_path,new_path,sidecar_count) VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![event_id.clone(), if j.kind == Kind::Dir {"folder"} else {"file"}, if tv {"tv"} else {"movies"}, j.series_id, j.movie_id, j.file_id, j.command_id.clone(), j.old.clone(), j.new.clone(), j.sidecars.len() as i64]).await.map_err(owned)?;
        tx.execute("UPDATE rename_commands SET items_renamed=items_renamed+1 WHERE id=?", [j.command_id.clone()]).await.map_err(owned)?;
        let r = tx.query(&format!("SELECT {} FROM rename_events WHERE id=?", events::COLUMNS), [event_id]).await.map_err(owned)?.next().await.map_err(owned)?.ok_or("storage_error")?;
        Ok(RenameEvent {
            id: r.get(0).map_err(owned)?,
            event_type: "rename",
            kind: if j.kind == Kind::Dir { RenameEventKind::Folder } else { RenameEventKind::File },
            owner: if tv { RenameOwner::Tv(j.series_id.unwrap_or(0)) } else { RenameOwner::Movies(j.movie_id.unwrap_or(0)) },
            file_id: j.file_id,
            command_id: j.command_id.clone(),
            old_path: j.old.clone(),
            new_path: j.new.clone(),
            sidecar_count: j.sidecars.len() as i64,
            occurred_at: r.get(10).map_err(owned)?,
            dispatched: false,
        })
    }.await;
    match result {
        Ok(event) => {
            tx.commit().await.map_err(|_| "storage_error")?;
            Ok(event)
        }
        Err(code) => {
            let _ = tx.rollback().await;
            Err(code)
        }
    }
}
async fn fail_item(db: &Database, j: &Journal, restored: bool, code: &str) -> Result<()> {
    let c = connection(db).await?;
    if !restored {
        // Only a settled layout may lose its recovery handle or count as a failed item.
        settle_journal(&c, j, "planned", Some("rollback_incomplete")).await?;
        return Err(Error(StatusCode::CONFLICT, "rollback_incomplete"));
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let r = async {
        settle_journal(&tx, j, "rolled_back", Some(code)).await?;
        tx.execute(
            "UPDATE rename_commands SET items_failed=items_failed+1 WHERE id=?",
            [j.command_id.clone()],
        )
        .await?;
        Ok(())
    }
    .await;
    finish(tx, r).await
}
/// Finishes a journaled group, retaining the recovery handle if the layout cannot be restored.
async fn drive(db: &Arc<Database>, j: Journal) -> Result<bool> {
    let j = Arc::new(j);
    let jf = j.clone();
    let moved = tokio::task::spawn_blocking(move || forward(&jf))
        .await
        .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))?;
    if let Err(code) = moved {
        let jr = j.clone();
        let restored = tokio::task::spawn_blocking(move || rollback(&jr))
            .await
            .unwrap_or(false);
        fail_item(db, &j, restored, code).await?;
        return Ok(false);
    }
    match commit(db, &j).await {
        Ok(event) => {
            events::notify(&event);
            Ok(true)
        }
        Err("storage_error") => Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "command_storage_error",
        )),
        Err(code) => {
            let jr = j.clone();
            let restored = tokio::task::spawn_blocking(move || rollback(&jr))
                .await
                .unwrap_or(false);
            fail_item(db, &j, restored, code).await?;
            Ok(false)
        }
    }
}
/// Resolves planned journals globally, or for a command and its owners' prior commands.
pub(crate) async fn reconcile(db: &Arc<Database>, only: Option<&str>) -> Result<usize> {
    let c = connection(db).await?;
    let mut rows = c
        .query(&format!("SELECT {JOURNAL_COLUMNS} FROM rename_items WHERE {JOURNAL_SCOPE} ORDER BY id LIMIT 1000"), params![only])
        .await?;
    let mut parsed = vec![];
    while let Some(r) = rows.next().await? {
        parsed.push(parse_journal(&r)?);
    }
    drop(rows);
    let mut journals = vec![];
    for mut j in parsed {
        if bind_root(&c, &mut j).await? {
            journals.push(j);
        }
    }
    drop(c);
    let n = journals.len();
    for j in journals {
        // Anomalous pairs are rolled back by `drive` through the same forward/rollback logic.
        let id = j.id;
        match drive(db, j).await {
            Err(Error(_, "rollback_incomplete")) => {
                eprintln!("event=rename_recovery_pending journal_id={id} code=rollback_incomplete");
            }
            outcome => {
                outcome?;
            }
        }
    }
    Ok(n)
}
/// Worker-startup roll-forward/back, serialized with imports and deletions.
pub(crate) async fn startup(db: &Arc<Database>) -> Result<()> {
    let Ok(_guard) = crate::import::acquire_deletion_guard(db, "rename-startup") else {
        return Ok(());
    };
    reconcile(db, None).await.map(|_| ())
}
async fn settle(
    db: &Database,
    id: Uuid,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> Result<()> {
    connection(db).await?.execute(sql, values).await?;
    if let Some(done) = retained(&connection(db).await?, id).await? {
        eprintln!(
            "event=rename_settled command_id={} media_type={} status={} renamed={} unchanged={} failed={} bounded={}",
            done.id,
            domain(done.target.media_type),
            done.status.text(),
            done.items_renamed,
            done.items_unchanged,
            done.items_failed,
            done.bounded
        );
    }
    Ok(())
}
struct Work {
    series_id: Option<i64>,
    movie_id: Option<i64>,
    file_id: Option<i64>,
    kind: Kind,
    root: String,
    old: String,
    new: Option<String>,
    status: Option<RenameStatus>,
    reasons: Vec<RenameReason>,
}
fn reason_code(r: RenameReason) -> &'static str {
    match r {
        RenameReason::InvalidPath => "invalid_path",
        RenameReason::OutsideOwnerFolder => "outside_owner_folder",
        RenameReason::MissingExtension => "missing_extension",
        RenameReason::InvalidFileFacts => "invalid_file_facts",
        RenameReason::NamingRenderFailed => "naming_render_failed",
        RenameReason::MultiEpisodeStyleRequired => "multi_episode_style_required",
        RenameReason::NoLinkedEpisodes => "no_linked_episodes",
        RenameReason::InvalidEpisodeSet => "invalid_episode_set",
        RenameReason::FolderFormatUnavailable => "folder_format_unavailable",
        RenameReason::DestinationInUse => "destination_in_use",
        RenameReason::DuplicateDestination => "duplicate_destination",
        RenameReason::DestinationExists => "destination_exists",
        RenameReason::SourceMissing => "source_missing",
        RenameReason::PathUnsafe => "path_unsafe",
    }
}
fn from_file(p: PlannedFile) -> Work {
    Work {
        series_id: Some(p.series_id),
        movie_id: None,
        file_id: Some(p.file_id),
        kind: Kind::File,
        root: p.root,
        old: p.old_abs,
        new: p.new_abs,
        status: p.status,
        reasons: p.reasons,
    }
}
fn from_folder(p: PlannedFolder) -> Work {
    let root = p
        .old_abs
        .trim_end_matches('/')
        .rsplit_once('/')
        .map_or("/", |x| x.0)
        .to_owned();
    Work {
        series_id: p.series_id,
        movie_id: p.movie_id,
        file_id: None,
        kind: Kind::Dir,
        root,
        old: p.old_abs,
        new: p.new_abs,
        status: p.status,
        reasons: p.reasons,
    }
}
fn plan_error(e: movie_preview::Error) -> Error {
    match e.1 {
        "naming_configuration_invalid" => Error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "naming_configuration_invalid",
        ),
        "series_not_found" | "movie_not_found" => Error(StatusCode::NOT_FOUND, "target_missing"),
        _ => Error(StatusCode::SERVICE_UNAVAILABLE, "storage_error"),
    }
}
/// Plans every target of the command from current facts. Returns (work, unchanged).
async fn plan_work(db: &Arc<Database>, command: &RenameCommand) -> Result<(Vec<Work>, i64)> {
    let t = &command.target;
    let only: BTreeSet<i64> = t.file_ids.iter().copied().collect();
    let mut unchanged = 0;
    let mut work = vec![];
    match command.name {
        RenameName::RenameFiles | RenameName::RenameSeries if t.media_type == MediaDomain::Tv => {
            let r = plan::compute_tv(
                db,
                t.series_ids.clone(),
                None,
                (command.name == RenameName::RenameFiles).then_some(&only),
                false,
            )
            .await
            .map_err(plan_error)?;
            for f in r.files {
                if f.unchanged {
                    unchanged += 1
                } else {
                    work.push(from_file(f))
                }
            }
        }
        RenameName::RenameFiles | RenameName::RenameMovie => {
            let mut ids = t.movie_ids.clone();
            ids.sort_unstable();
            let r = movie_preview::compute(db, ids).await.map_err(plan_error)?;
            unchanged += r.unchanged_count as i64;
            let c = connection(db).await?;
            for item in r.items {
                if command.name == RenameName::RenameFiles && !only.contains(&item.movie_file_id) {
                    continue;
                }
                let row = c.query("SELECT m.path,f.path FROM movies m JOIN movie_files f ON f.movie_id=m.id WHERE m.id=? AND f.id=?", params![item.movie_id, item.movie_file_id]).await?.next().await?;
                let Some(row) = row else { continue };
                let (root, old) = (row.get::<String>(0)?, row.get::<String>(1)?);
                let unavailable = matches!(
                    item.status,
                    crate::naming::movie_preview::MovieRenamePreviewStatus::Unavailable
                );
                work.push(Work {
                    series_id: None,
                    movie_id: Some(item.movie_id),
                    file_id: Some(item.movie_file_id),
                    kind: Kind::File,
                    new: item
                        .new_path
                        .map(|n| format!("{}/{n}", root.trim_end_matches('/'))),
                    root,
                    old,
                    status: Some(if unavailable {
                        RenameStatus::Unavailable
                    } else {
                        RenameStatus::Change
                    }),
                    reasons: if unavailable {
                        vec![RenameReason::NamingRenderFailed]
                    } else {
                        vec![]
                    },
                });
            }
        }
        RenameName::RenameSeriesFolder | RenameName::RenameMovieFolder => {
            let ids = if t.media_type == MediaDomain::Tv {
                t.series_ids.clone()
            } else {
                t.movie_ids.clone()
            };
            let r = plan::compute_folders(db, t.media_type, ids, false)
                .await
                .map_err(plan_error)?;
            for f in r.folders {
                if f.unchanged {
                    unchanged += 1
                } else {
                    work.push(from_folder(f))
                }
            }
        }
        _ => return Err(bad()),
    }
    Ok((work, unchanged))
}
async fn record_failure(db: &Database, command_id: &str, w: &Work, code: &str) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let r = async {
        tx.execute("INSERT INTO rename_items(command_id,kind,series_id,movie_id,file_id,old_path,new_path,identity_json,state,error_code) VALUES(?,?,?,?,?,?,?,'{}','failed',?)",
            params![command_id, if w.kind == Kind::Dir {"folder"} else {"file"}, w.series_id, w.movie_id, w.file_id, w.old.clone(), w.new.clone().unwrap_or_else(|| w.old.clone()), code]).await?;
        tx.execute("UPDATE rename_commands SET items_failed=items_failed+1 WHERE id=?", [command_id]).await?;
        Ok(())
    }.await;
    finish(tx, r).await
}
enum Prepared {
    Ready(Ident, Vec<Sidecar>),
    Refused(&'static str),
}
fn prepare(w: &Work, tracked: &dyn Fn(&str) -> bool) -> Prepared {
    let new = w.new.as_deref().unwrap_or_default();
    match fsops::probe(&w.root, &w.old, new, w.kind) {
        fsops::Probe::Ready | fsops::Probe::CaseOnly => {}
        fsops::Probe::Exists => return Prepared::Refused("destination_exists"),
        fsops::Probe::SourceMissing => return Prepared::Refused("source_missing"),
        fsops::Probe::Unsafe => return Prepared::Refused("path_unsafe"),
    }
    let ident = match fsops::observe(&w.old, w.kind) {
        Ok(Some(i)) => i,
        Ok(None) => return Prepared::Refused("source_missing"),
        Err(e) => return Prepared::Refused(e.code()),
    };
    let sidecars = if w.kind == Kind::File {
        match fsops::discover_sidecars(&w.old, new, tracked) {
            Ok(s) => s,
            Err(e) => return Prepared::Refused(e.code()),
        }
    } else {
        vec![]
    };
    for s in &sidecars {
        if !matches!(
            fsops::probe(&w.root, &s.old, &s.new, Kind::File),
            fsops::Probe::Ready | fsops::Probe::CaseOnly
        ) {
            return Prepared::Refused("destination_exists");
        }
    }
    Prepared::Ready(ident, sidecars)
}
async fn tracked_paths(db: &Database, work: &[Work]) -> Result<Arc<BTreeSet<String>>> {
    let c = connection(db).await?;
    let mut set = BTreeSet::new();
    let mut rows = c
        .query(
            "SELECT path FROM episode_files UNION SELECT path FROM movie_files",
            (),
        )
        .await?;
    let _ = work;
    while let Some(r) = rows.next().await? {
        set.insert(r.get::<String>(0)?);
    }
    Ok(Arc::new(set))
}
pub(super) async fn run(db: &Arc<Database>, command: RenameCommand) -> Result<()> {
    let id = command.id;
    let sid = id.to_string();
    let running = "WHERE id=? AND status='running'";
    let c = connection(db).await?;
    let Some(current) = retained(&c, id).await? else {
        return Ok(());
    };
    if !matches!(current.status, CommandStatus::Running) {
        return Ok(());
    }
    let guard = match crate::import::acquire_deletion_guard(db, &sid) {
        Ok(g) => g,
        Err(_) => {
            return settle(db, id, &format!("UPDATE rename_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code='import_busy' {running}"), params![now()? + BUSY_RETRY_SECONDS, now()?, sid]).await;
        }
    };
    // Finish what an interrupted attempt left half done before planning anything new.
    reconcile(db, Some(&sid)).await?;
    if c.query(
        &format!("SELECT 1 FROM rename_items WHERE {JOURNAL_SCOPE} LIMIT 1"),
        [sid.clone()],
    )
    .await?
    .next()
    .await?
    .is_some()
    {
        return Err(Error(StatusCode::CONFLICT, "rollback_incomplete"));
    }
    let fail = |code: &'static str| {
        format!(
            "UPDATE rename_commands SET status='failed',completed_at=?,error_code='{code}' {running}"
        )
    };
    if let Some(expected) = command.expected_naming_revision {
        if expected != naming_revision(&c, command.target.media_type).await? {
            return settle(
                db,
                id,
                &fail("naming_revision_changed"),
                params![now()?, sid],
            )
            .await;
        }
    }
    drop(c);
    let (work, unchanged) = match plan_work(db, &command).await {
        Ok(v) => v,
        Err(Error(_, "naming_configuration_invalid")) => {
            return settle(
                db,
                id,
                &fail("naming_configuration_invalid"),
                params![now()?, sid],
            )
            .await;
        }
        Err(Error(_, "target_missing")) => {
            return settle(db, id, &fail("target_missing"), params![now()?, sid]).await;
        }
        Err(e) => return Err(e),
    };
    connection(db)
        .await?
        .execute(
            "UPDATE rename_commands SET items_unchanged=? WHERE id=?",
            params![unchanged, sid.clone()],
        )
        .await?;
    let tracked = tracked_paths(db, &work).await?;
    let mut bounded_run = false;
    for (index, w) in work.into_iter().enumerate() {
        if index >= MAX_ITEMS {
            bounded_run = true;
            break;
        }
        let c = connection(db).await?;
        let Some(live) = retained(&c, id).await? else {
            return Ok(());
        };
        if !matches!(live.status, CommandStatus::Running) {
            return Ok(());
        }
        if live.cancel_requested {
            drop(c);
            return settle(db, id, &format!("UPDATE rename_commands SET status='cancelled',completed_at=?,bounded=? {running}"), params![now()?, i64::from(bounded_run), sid]).await;
        }
        drop(c);
        if let Some(status) = w.status {
            if status != RenameStatus::Change {
                let code = w.reasons.first().copied().map_or("blocked", reason_code);
                record_failure(db, &sid, &w, code).await?;
                continue;
            }
        }
        let wt = Arc::new(w);
        let (p, tr) = (wt.clone(), tracked.clone());
        let prepared =
            tokio::task::spawn_blocking(move || prepare(&p, &|path: &str| tr.contains(path)))
                .await
                .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error"))?;
        let w = wt;
        let (ident, sidecars) = match prepared {
            Prepared::Refused(code) => {
                record_failure(db, &sid, &w, code).await?;
                continue;
            }
            Prepared::Ready(i, s) => (i, s),
        };
        // Intent is durable before any rename; the journal row is the recovery handle.
        let new = w.new.clone().unwrap_or_default();
        let journal_id = {
            let c = connection(db).await?;
            c.execute("INSERT INTO rename_items(command_id,kind,series_id,movie_id,file_id,old_path,new_path,identity_json,sidecars_json,state) VALUES(?,?,?,?,?,?,?,?,?,'planned')",
                params![sid.clone(), if w.kind == Kind::Dir {"folder"} else {"file"}, w.series_id, w.movie_id, w.file_id, w.old.clone(), new.clone(), serde_json::to_string(&ident).map_err(|_| bad())?, serde_json::to_string(&sidecars).map_err(|_| bad())?]).await?;
            c.last_insert_rowid()
        };
        drive(
            db,
            Journal {
                id: journal_id,
                command_id: sid.clone(),
                kind: w.kind,
                series_id: w.series_id,
                movie_id: w.movie_id,
                file_id: w.file_id,
                old: w.old.clone(),
                new,
                ident,
                sidecars,
                root: w.root.clone(),
            },
        )
        .await?;
    }
    drop(guard);
    let counts = connection(db)
        .await?
        .query(
            "SELECT items_renamed,items_failed FROM rename_commands WHERE id=?",
            [sid.clone()],
        )
        .await?
        .next()
        .await?
        .map(|r| (r.get::<i64>(0).unwrap_or(0), r.get::<i64>(1).unwrap_or(0)));
    match counts {
        Some((0, failed)) if failed > 0 => settle(db, id, &format!("UPDATE rename_commands SET status='failed',completed_at=?,error_code='items_failed',bounded=? {running}"), params![now()?, i64::from(bounded_run), sid]).await,
        _ => settle(db, id, &format!("UPDATE rename_commands SET status='succeeded',completed_at=?,bounded=? {running}"), params![now()?, i64::from(bounded_run), sid]).await,
    }
}
