//! Concrete catalog refresh commands. Remote reads precede one atomic local reconciliation.
use super::*;
use crate::{
    library::refresh,
    metadata::{MetadataClient, MetadataError},
};

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "media_type", rename_all = "lowercase", deny_unknown_fields)]
pub enum MetadataRefreshTarget {
    Tv { series_id: i64 },
    Movies { movie_id: i64 },
}
impl MetadataRefreshTarget {
    fn writer(self) -> refresh::Target {
        match self {
            Self::Tv { series_id } => refresh::Target::Tv { series_id },
            Self::Movies { movie_id } => refresh::Target::Movies { movie_id },
        }
    }
    fn parts(self) -> (&'static str, Option<i64>, Option<i64>, &'static str) {
        match self {
            Self::Tv { series_id } => ("tv", Some(series_id), None, "refresh_series"),
            Self::Movies { movie_id } => ("movies", None, Some(movie_id), "refresh_movie"),
        }
    }
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum MetadataCommandName {
    RefreshSeries,
    RefreshMovie,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MetadataCommandInput {
    pub target: MetadataRefreshTarget,
    pub priority: CommandPriority,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct MetadataCommandQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[ts(optional)]
    pub series_id: Option<i64>,
    #[ts(optional)]
    pub movie_id: Option<i64>,
    #[ts(optional)]
    pub status: Option<CommandStatus>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
#[derive(Serialize, ts_rs::TS)]
pub struct MetadataCommand {
    pub id: Uuid,
    pub name: MetadataCommandName,
    pub target: MetadataRefreshTarget,
    pub external_id: i64,
    #[serde(skip)]
    #[ts(skip)]
    metadata_id: Option<i64>,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub records_updated: u16,
}
impl MetadataCommand {
    fn captured(&self) -> refresh::CapturedTarget {
        refresh::CapturedTarget {
            target: self.target.writer(),
            external_id: self.external_id,
            metadata_id: self.metadata_id,
        }
    }
}
const COLUMNS: &str = "id,name,media_type,series_id,movie_id,external_id,metadata_id,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,records_updated";
// Used only with the concrete command table, never ambiguous library integer IDs.
const CURRENT: &str = "((media_type='tv' AND EXISTS(SELECT 1 FROM series s WHERE s.id=metadata_refresh_commands.series_id AND s.tvdb_id=metadata_refresh_commands.external_id)) OR (media_type='movies' AND EXISTS(SELECT 1 FROM movies m JOIN movie_metadata mm ON mm.id=m.metadata_id WHERE m.id=metadata_refresh_commands.movie_id AND m.metadata_id=metadata_refresh_commands.metadata_id AND mm.tmdb_id=metadata_refresh_commands.external_id)))";
fn row(r: libsql::Row) -> Result<MetadataCommand> {
    let target = match r.get::<String>(2)?.as_str() {
        "tv" => MetadataRefreshTarget::Tv {
            series_id: r.get(3)?,
        },
        "movies" => MetadataRefreshTarget::Movies {
            movie_id: r.get(4)?,
        },
        _ => return Err(bad()),
    };
    Ok(MetadataCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: match r.get::<String>(1)?.as_str() {
            "refresh_series" => MetadataCommandName::RefreshSeries,
            "refresh_movie" => MetadataCommandName::RefreshMovie,
            _ => return Err(bad()),
        },
        target,
        external_id: r.get(5)?,
        metadata_id: r.get(6)?,
        priority: if r.get::<i64>(7)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(8)?)?,
        attempts: r.get::<i64>(9)? as u8,
        next_attempt_at: r.get(10)?,
        created_at: r.get(11)?,
        started_at: r.get(12)?,
        completed_at: r.get(13)?,
        error_code: r.get(14)?,
        records_updated: r.get::<i64>(15)? as u16,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<MetadataCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM metadata_refresh_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "metadata_command_not_found"))?)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<MetadataCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "metadata_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
fn capture_error(error: refresh::Error) -> Error {
    match error {
        refresh::Error::TargetChanged => Error(StatusCode::CONFLICT, "target_changed"),
        refresh::Error::Conflict => Error(StatusCode::CONFLICT, "metadata_conflict"),
        refresh::Error::Storage => {
            eprintln!("event=metadata_command_storage_error code=storage_error");
            Error(StatusCode::INTERNAL_SERVER_ERROR, "command_storage_error")
        }
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/metadata-refresh/commands", get(list).post(create))
        .route(
            "/api/v1/metadata-refresh/commands/{id}",
            get(detail).delete(delete),
        )
        .route(
            "/api/v1/metadata-refresh/commands/{id}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(db)
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<MetadataCommandInput>, JsonRejection>,
) -> Result<(StatusCode, Json<MetadataCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let (media, series, movie, name) = input.target.parts();
    if !(1..=MAX_REVISION).contains(&series.or(movie).ok_or_else(bad)?) {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let captured=refresh::capture(&tx,input.target.writer()).await.map_err(capture_error)?;
        if let Some(r)=tx.query(&format!("SELECT {COLUMNS} FROM metadata_refresh_commands WHERE media_type=? AND series_id IS ? AND movie_id IS ? AND status IN ('queued','running','retry_wait')"),params![media,series,movie]).await?.next().await? {
            let existing=row(r)?;
            if existing.captured()!=captured {return Err(conflict())}
            return bounded(existing)
        }
        if tx.query("SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)",()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)? >=MAX_COMMANDS {return Err(Error(StatusCode::TOO_MANY_REQUESTS,"command_history_full"))}
        let id=Uuid::new_v4();let timestamp=now()?;
        tx.execute("INSERT INTO metadata_refresh_commands(id,name,media_type,series_id,movie_id,external_id,metadata_id,priority,status,attempts,next_attempt_at,created_at,records_updated)VALUES(?,?,?,?,?,?,?,?,'queued',0,?,?,0)",params![id.to_string(),name,media,series,movie,captured.external_id,captured.metadata_id,input.priority.number(),timestamp,timestamp]).await?;
        bounded(read(&tx,id).await?)
    }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<MetadataCommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<MetadataCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    if q.series_id.is_some() && (q.movie_id.is_some() || q.media_type != Some(MediaDomain::Tv))
        || q.movie_id.is_some() && q.media_type != Some(MediaDomain::Movies)
        || q.series_id
            .into_iter()
            .chain(q.movie_id)
            .any(|id| !(1..=MAX_REVISION).contains(&id))
    {
        return Err(bad());
    }
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async {
        let predicate="WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?) AND (? IS NULL OR series_id=?) AND (? IS NULL OR movie_id=?)";
        let total=tx.query(&format!("SELECT count(*) FROM metadata_refresh_commands {predicate}"),params![media,media,status,status,q.series_id,q.series_id,q.movie_id,q.movie_id]).await?.next().await?.ok_or_else(bad)?.get(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM metadata_refresh_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,q.series_id,q.series_id,q.movie_id,q.movie_id,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=vec![];while let Some(r)=rows.next().await?{items.push(row(r)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<MetadataCommand>> {
    q.map_err(|_| bad())?;
    bounded(
        read(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
        )
        .await?,
    )
}
async fn cancel(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<MetadataCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        tx.execute("UPDATE metadata_refresh_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','running','retry_wait')",params![now()?,id.to_string()]).await?;
        bounded(read(&tx,id).await?)
    }.await;
    finish(tx, outcome).await
}
async fn delete(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<StatusCode> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if matches!(
            read(&tx, id).await?.status,
            CommandStatus::Queued | CommandStatus::Running | CommandStatus::RetryWait
        ) {
            return Err(conflict());
        }
        tx.execute(
            "DELETE FROM metadata_refresh_commands WHERE id=?",
            [id.to_string()],
        )
        .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE metadata_refresh_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
pub(super) async fn sweep(c: &Connection, timestamp: i64) -> Result<()> {
    c.execute(&format!("UPDATE metadata_refresh_commands SET status='failed',completed_at=?,error_code='target_changed' WHERE status IN ('queued','retry_wait') AND NOT {CURRENT}"),[timestamp]).await?;
    Ok(())
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<MetadataCommand> {
    c.execute("UPDATE metadata_refresh_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
struct Failure {
    code: &'static str,
    retryable: bool,
    retry_after: Option<u32>,
}
fn remote_failure(error: MetadataError) -> Failure {
    let (code, retryable, retry_after) = match error {
        MetadataError::NotFound => ("metadata_not_found", false, None),
        MetadataError::InvalidInput | MetadataError::InvalidResponse => {
            ("invalid_metadata_response", false, None)
        }
        MetadataError::Unavailable => ("metadata_unavailable", true, None),
        MetadataError::Busy => ("metadata_busy", true, None),
        MetadataError::RateLimited(delay) => ("metadata_rate_limited", true, delay),
    };
    Failure {
        code,
        retryable,
        retry_after,
    }
}
async fn failure(c: &Connection, command: &MetadataCommand, error: Failure) -> Result<()> {
    let timestamp = now()?;
    let retry = error.retryable && command.attempts < 3;
    let delay = ((1i64 << command.attempts) + i64::from(Uuid::new_v4().as_bytes()[0] % 3))
        .max(i64::from(error.retry_after.unwrap_or(0).min(86400)));
    c.execute("UPDATE metadata_refresh_commands SET status=?,next_attempt_at=?,completed_at=?,error_code=? WHERE id=?",params![if retry{"retry_wait"}else{"failed"},timestamp+delay,if retry{None}else{Some(timestamp)},error.code,command.id.to_string()]).await?;
    Ok(())
}
pub(super) async fn run(
    db: &Arc<Database>,
    client: &MetadataClient,
    command: MetadataCommand,
) -> Result<()> {
    let probe = async {
        match command.target {
            MetadataRefreshTarget::Tv { .. } => client
                .series(command.external_id)
                .await
                .map(refresh::Details::Series),
            MetadataRefreshTarget::Movies { .. } => client
                .movie(command.external_id)
                .await
                .map(refresh::Details::Movie),
        }
    };
    tokio::pin!(probe);
    let mut cancellation = tokio::time::interval(std::time::Duration::from_millis(250));
    cancellation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            result=&mut probe=>break result.map_err(remote_failure),
            _=cancellation.tick()=>{
                let Some(current)=retained(&connection(db).await?,command.id).await? else{return Ok(())};
                if !matches!(current.status,CommandStatus::Running){return Ok(())}
            }
        }
    };
    publish(db, &command, result).await
}
async fn publish(
    db: &Database,
    command: &MetadataCommand,
    result: std::result::Result<refresh::Details, Failure>,
) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async {
        let Some(current)=retained(&tx,command.id).await? else{return Ok(None)};
        if !matches!(current.status,CommandStatus::Running){return Ok(None)}
        let current_target=refresh::capture(&tx,command.target.writer()).await;
        let result=match current_target {
            Ok(value) if value==command.captured()=>result,
            Ok(_)|Err(refresh::Error::TargetChanged)|Err(refresh::Error::Conflict)=>Err(Failure{code:"target_changed",retryable:false,retry_after:None}),
            Err(refresh::Error::Storage)=>return Err(Error(StatusCode::INTERNAL_SERVER_ERROR,"command_storage_error")),
        };
        match result {
            Ok(details)=>{
                let count=refresh::apply(&tx,&command.captured(),details).await.map_err(capture_error)?;
                tx.execute("UPDATE metadata_refresh_commands SET status='succeeded',completed_at=?,records_updated=? WHERE id=?",params![now()?,i64::from(count),command.id.to_string()]).await?;
            },
            Err(error)=>failure(&tx,command,error).await?,
        }
        Ok(Some(read(&tx,command.id).await?))
    }.await;
    // A writer error can follow writes. Roll back every fact before settling its static failure.
    let settled = match finish(tx, outcome).await {
        Err(Error(_, code @ ("metadata_conflict" | "target_changed"))) => {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await?;
            let outcome = async {
                let Some(current) = retained(&tx, command.id).await? else {
                    return Ok(None);
                };
                if !matches!(current.status, CommandStatus::Running) {
                    return Ok(None);
                }
                failure(
                    &tx,
                    command,
                    Failure {
                        code,
                        retryable: false,
                        retry_after: None,
                    },
                )
                .await?;
                Ok(Some(read(&tx, command.id).await?))
            }
            .await;
            finish(tx, outcome).await?
        }
        other => other?,
    };
    if let Some(settled) = settled {
        eprintln!(
            "event=metadata_command_settled command_id={} media_type={} status={} error_code={}",
            settled.id,
            settled.target.parts().0,
            settled.status.text(),
            settled.error_code.as_deref().unwrap_or("none")
        );
    }
    Ok(())
}
