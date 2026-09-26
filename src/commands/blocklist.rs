//! Explicit execution-time domain purge, atomic with tombstones and command settlement.
use super::*;
#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BlocklistClearTarget {
    pub media_type: MediaDomain,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BlocklistClearInput {
    pub target: BlocklistClearTarget,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum BlocklistClearName {
    ClearBlocklist,
}
#[derive(Serialize, ts_rs::TS)]
pub struct BlocklistClearCommand {
    pub id: Uuid,
    pub name: BlocklistClearName,
    pub target: BlocklistClearTarget,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub records_removed: i64,
}
const COLUMNS: &str = "id,media_type,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,records_removed";
fn row(r: libsql::Row) -> Result<BlocklistClearCommand> {
    Ok(BlocklistClearCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        name: BlocklistClearName::ClearBlocklist,
        target: BlocklistClearTarget {
            media_type: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| bad())?,
        },
        priority: if r.get::<i64>(2)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(3)?)?,
        attempts: r.get::<i64>(4)? as u8,
        next_attempt_at: r.get(5)?,
        created_at: r.get(6)?,
        started_at: r.get(7)?,
        completed_at: r.get(8)?,
        error_code: r.get(9)?,
        records_removed: r.get(10)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<BlocklistClearCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM blocklist_clear_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::NOT_FOUND,
            "blocklist_clear_command_not_found",
        ))?)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<BlocklistClearCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "blocklist_clear_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/blocklist/clear-commands", get(list).post(create))
        .route(
            "/api/v1/blocklist/clear-commands/{id}",
            get(detail).delete(delete),
        )
        .route("/api/v1/blocklist/clear-commands/{id}/cancel", post(cancel))
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(request_deadline))
        .with_state(db)
}
async fn request_deadline(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "blocklist_clear_timeout").into_response(),
    }
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<BlocklistClearInput>, JsonRejection>,
) -> Result<(StatusCode, Json<BlocklistClearCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if let Some(r)=tx.query(&format!("SELECT {COLUMNS} FROM blocklist_clear_commands WHERE media_type=? AND status IN ('queued','running','retry_wait')"),[domain(input.target.media_type)]).await?.next().await?{return bounded(row(r)?)}
        if tx.query("SELECT (SELECT count(*) FROM commands)+(SELECT count(*) FROM metadata_refresh_commands)+(SELECT count(*) FROM blocklist_clear_commands)+(SELECT count(*) FROM rss_commands)+(SELECT count(*) FROM search_commands)+(SELECT count(*) FROM manual_import_commands)",()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)? >=MAX_COMMANDS{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"command_history_full"))}
        let id=Uuid::new_v4();let timestamp=now()?;
        tx.execute("INSERT INTO blocklist_clear_commands(id,name,media_type,priority,status,attempts,next_attempt_at,created_at,records_removed)VALUES(?,'clear_blocklist',?,?,'queued',0,?,?,0)",params![id.to_string(),domain(input.target.media_type),input.priority.number(),timestamp,timestamp]).await?;
        bounded(read(&tx,id).await?)
    }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<BlocklistClearCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if !(1..=100).contains(&q.limit) || q.offset > MAX_COMMANDS as u32 {
        return Err(bad());
    }
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let predicate="WHERE (? IS NULL OR media_type=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM blocklist_clear_commands {predicate}"),params![media,media,status,status]).await?.next().await?.ok_or_else(bad)?.get(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM blocklist_clear_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=vec![];while let Some(r)=rows.next().await?{items.push(row(r)?)}bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<BlocklistClearCommand>> {
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
) -> Result<Json<BlocklistClearCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        tx.execute("UPDATE blocklist_clear_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','running','retry_wait')",params![now()?,id.to_string()]).await?;bounded(read(&tx,id).await?)
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
            "DELETE FROM blocklist_clear_commands WHERE id=?",
            [id.to_string()],
        )
        .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE blocklist_clear_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
pub(super) async fn claim(
    c: &Connection,
    id: Uuid,
    timestamp: i64,
) -> Result<BlocklistClearCommand> {
    c.execute("UPDATE blocklist_clear_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
pub(super) async fn run(db: &Database, command: BlocklistClearCommand) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let Some(current)=retained(&tx,command.id).await? else{return Ok(None)};
        if !matches!(current.status,CommandStatus::Running){return Ok(None)}
        // No page/selection snapshot: purge the chosen domain as it exists at execution.
        // Existing DELETE triggers retain provenance tombstones and remove episode memberships.
        let removed=tx.execute("DELETE FROM blocklist_entries WHERE media_type=?",[domain(command.target.media_type)]).await?;
        let removed=i64::try_from(removed).ok().filter(|n|*n<=MAX_REVISION).ok_or(Error(StatusCode::INTERNAL_SERVER_ERROR,"command_storage_error"))?;
        tx.execute("UPDATE blocklist_clear_commands SET status='succeeded',completed_at=?,records_removed=? WHERE id=?",params![now()?,removed,command.id.to_string()]).await?;
        Ok(Some(read(&tx,command.id).await?))
    }.await;
    if let Some(done) = finish(tx, outcome).await? {
        eprintln!(
            "event=blocklist_clear_settled command_id={} media_type={} status={} records_removed={}",
            done.id,
            domain(done.target.media_type),
            done.status.text(),
            done.records_removed
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("scratch cleanup failed: {error}")
            }
        }
    }
    async fn imported(db: &Database, path: &std::path::Path, tv: bool) {
        let source = libsql::Builder::new_local(path).build().await.unwrap();
        let c = source.connect().unwrap();
        if tv {
            c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(233);CREATE TABLE Series(Id INTEGER,TvdbId INTEGER,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,Seasons TEXT);INSERT INTO Series VALUES(1,101,'Series',2020,'/owned-fixture/tv',1,'[{\"seasonNumber\":1,\"monitored\":true}]');CREATE TABLE Episodes(Id INTEGER,SeriesId INTEGER,SeasonNumber INTEGER,EpisodeNumber INTEGER,Title TEXT,Monitored INTEGER,EpisodeFileId INTEGER);INSERT INTO Episodes VALUES(1,1,1,1,'Episode',1,0);CREATE TABLE EpisodeFiles(Id INTEGER,SeriesId INTEGER,RelativePath TEXT);CREATE TABLE Blocklist(Id INTEGER,SeriesId INTEGER,EpisodeIds TEXT,Date TEXT,SourceTitle TEXT);INSERT INTO Blocklist VALUES(1,1,'[1]','2020-01-01T00:00:00Z','TV blocked');").await.unwrap();
        } else {
            c.execute_batch("CREATE TABLE VersionInfo(Version INTEGER);INSERT INTO VersionInfo VALUES(206);CREATE TABLE Movies(Id INTEGER,TmdbId INTEGER,ImdbId TEXT,Title TEXT,Year INTEGER,Path TEXT,Monitored INTEGER,MovieFileId INTEGER);INSERT INTO Movies VALUES(1,101,'tt101','Movie',2020,'/owned-fixture/movies',1,0);CREATE TABLE MovieFiles(Id INTEGER,MovieId INTEGER,RelativePath TEXT,Edition TEXT);CREATE TABLE Blocklist(Id INTEGER,MovieId INTEGER,Date TEXT,SourceTitle TEXT);INSERT INTO Blocklist VALUES(1,1,'2020-01-01T00:00:00Z','Movie blocked');").await.unwrap();
        }
        let root = path
            .parent()
            .unwrap()
            .join(if tv { "tv" } else { "movies" });
        c.execute(
            if tv {
                "UPDATE Series SET Path=?"
            } else {
                "UPDATE Movies SET Path=?"
            },
            [root.to_str().unwrap()],
        )
        .await
        .unwrap();
        drop(c);
        drop(source);
        let report = crate::snapshots::import(
            db,
            if tv {
                crate::snapshots::Application::Sonarr
            } else {
                crate::snapshots::Application::Radarr
            },
            std::fs::read(path).unwrap(),
            false,
        )
        .await
        .unwrap();
        assert!(report.applied);
        assert_eq!(report.conflicts, 0);
    }
    async fn enqueue(db: &Arc<Database>, media_type: MediaDomain) -> BlocklistClearCommand {
        create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(BlocklistClearInput {
                target: BlocklistClearTarget { media_type },
                priority: CommandPriority::Normal,
            })),
        )
        .await
        .unwrap()
        .1
        .0
    }
    async fn actual_claim(db: &Database, id: Uuid) -> BlocklistClearCommand {
        let c = connection(db).await.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let command = claim(&tx, id, now().unwrap()).await.unwrap();
        tx.commit().await.unwrap();
        command
    }
    #[tokio::test]
    async fn running_clear_claim_cancellation_and_reopen_preserve_both_domain_targets() {
        let _imports = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
        let path = std::env::temp_dir().join(format!("hrrdarr-clear-claim-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let _scratch = Scratch(path.clone());
        let dbpath = path.join("db");
        let mut db = Arc::new(Database::open_local(&dbpath).await.unwrap());
        imported(&db, &path.join("sonarr"), true).await;
        imported(&db, &path.join("radarr"), false).await;
        for media_type in [MediaDomain::Tv, MediaDomain::Movies] {
            let command = enqueue(&db, media_type).await;
            let claimed = actual_claim(&db, command.id).await;
            assert!(matches!(claimed.status, CommandStatus::Running));
            assert_eq!(claimed.attempts, 1);
            let _ = cancel(
                State(db.clone()),
                Path(command.id.to_string()),
                Ok(Query(Empty {})),
            )
            .await
            .unwrap();
            // Actual production claim + cancellation precedes execution; it must leave the whole domain intact.
            run(&db, claimed).await.unwrap();
            let c = connection(&db).await.unwrap();
            assert_eq!(
                c.query(
                    "SELECT count(*) FROM blocklist_entries WHERE media_type=?",
                    [domain(media_type)]
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
                1
            );
            drop(c);
            let command = enqueue(&db, media_type).await;
            let claimed = actual_claim(&db, command.id).await;
            assert_eq!(claimed.attempts, 1);
            drop(claimed);
            // Simulate interruption exactly between durable claim and atomic mutation, without SQL-fabricated running state.
            drop(db);
            db = Arc::new(Database::open_local(&dbpath).await.unwrap());
            let (_, client) = crate::providers::router_with_refresh(db.clone(), None);
            let runtime = super::super::start(db.clone(), client).await.unwrap();
            let done = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let current = read(&connection(&db).await.unwrap(), command.id)
                        .await
                        .unwrap();
                    if matches!(current.status, CommandStatus::Succeeded) {
                        break current;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await
                }
            })
            .await
            .unwrap();
            assert_eq!(done.id, command.id);
            assert_eq!(done.target.media_type, media_type);
            assert_eq!(done.attempts, 2);
            assert_eq!(done.records_removed, 1);
            runtime.shutdown().await;
            let c = connection(&db).await.unwrap();
            assert_eq!(
                c.query(
                    "SELECT count(*) FROM blocklist_entries WHERE media_type=?",
                    [domain(media_type)]
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
                0
            );
            if media_type == MediaDomain::Tv {
                assert_eq!(
                    c.query(
                        "SELECT count(*) FROM blocklist_entries WHERE media_type='movies'",
                        ()
                    )
                    .await
                    .unwrap()
                    .next()
                    .await
                    .unwrap()
                    .unwrap()
                    .get::<i64>(0)
                    .unwrap(),
                    1
                )
            }
        }
    }
}
