//! Durable whole-domain quality-definition reset, atomic with the shared reset logic in `qualities.rs`.
use super::*;
#[derive(Clone, Copy, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct QualityResetInput {
    pub media_type: MediaDomain,
    pub reset_titles: bool,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct QualityResetCommand {
    pub id: Uuid,
    pub media_type: MediaDomain,
    pub reset_titles: bool,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub definitions_reset: Option<i64>,
}
const COLUMNS: &str = "id,media_type,reset_titles,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,definitions_reset";
fn row(r: libsql::Row) -> Result<QualityResetCommand> {
    Ok(QualityResetCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        media_type: MediaDomain::parse(&r.get::<String>(1)?).map_err(|_| bad())?,
        reset_titles: r.get::<i64>(2)? == 1,
        priority: if r.get::<i64>(3)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(4)?)?,
        attempts: r.get::<i64>(5)? as u8,
        next_attempt_at: r.get(6)?,
        created_at: r.get(7)?,
        started_at: r.get(8)?,
        completed_at: r.get(9)?,
        error_code: r.get(10)?,
        definitions_reset: r.get(11)?,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<QualityResetCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM quality_reset_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::NOT_FOUND,
            "quality_reset_command_not_found",
        ))?)
}
async fn retained(c: &Connection, id: Uuid) -> Result<Option<QualityResetCommand>> {
    match read(c, id).await {
        Ok(v) => Ok(Some(v)),
        Err(Error(StatusCode::NOT_FOUND, "quality_reset_command_not_found")) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route(
            "/api/v1/quality-definitions/reset-commands",
            get(list).post(create),
        )
        .route(
            "/api/v1/quality-definitions/reset-commands/{id}",
            get(detail),
        )
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
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "quality_reset_timeout").into_response(),
    }
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<QualityResetInput>, JsonRejection>,
) -> Result<(StatusCode, Json<QualityResetCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if let Some(r)=tx.query(&format!("SELECT {COLUMNS} FROM quality_reset_commands WHERE media_type=? AND status IN ('queued','running','retry_wait')"),[domain(input.media_type)]).await?.next().await?{
            let existing=row(r)?;
            if existing.reset_titles!=input.reset_titles{return Err(conflict())}
            return bounded(existing)
        }
        if command_capacity(&tx).await? >=MAX_COMMANDS{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"command_history_full"))}
        let id=Uuid::new_v4();let timestamp=now()?;
        tx.execute("INSERT INTO quality_reset_commands(id,media_type,reset_titles,priority,status,attempts,next_attempt_at,created_at)VALUES(?,?,?,?,'queued',0,?,?)",params![id.to_string(),domain(input.media_type),i64::from(input.reset_titles),input.priority.number(),timestamp,timestamp]).await?;
        bounded(read(&tx,id).await?)
    }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<QualityResetCommand>>> {
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
        let total=tx.query(&format!("SELECT count(*) FROM quality_reset_commands {predicate}"),params![media,media,status,status]).await?.next().await?.ok_or_else(bad)?.get(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM quality_reset_commands {predicate} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=vec![];while let Some(r)=rows.next().await?{items.push(row(r)?)}bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<QualityResetCommand>> {
    q.map_err(|_| bad())?;
    bounded(
        read(
            &connection(&db).await?,
            Uuid::parse_str(&id).map_err(|_| bad())?,
        )
        .await?,
    )
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    c.execute("UPDATE quality_reset_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=?,completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<QualityResetCommand> {
    c.execute("UPDATE quality_reset_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    read(c, id).await
}
pub(super) async fn run(db: &Database, command: QualityResetCommand) -> Result<()> {
    let c = connection(db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let Some(current)=retained(&tx,command.id).await? else{return Ok(None)};
        if !matches!(current.status,CommandStatus::Running){return Ok(None)}
        let changed=crate::qualities::reset_definitions(&tx,domain(command.media_type),command.reset_titles).await?;
        tx.execute("UPDATE quality_reset_commands SET status='succeeded',completed_at=?,definitions_reset=? WHERE id=?",params![now()?,changed,command.id.to_string()]).await?;
        Ok(Some(read(&tx,command.id).await?))
    }.await;
    if let Some(done) = finish(tx, outcome).await? {
        eprintln!(
            "event=quality_reset_settled command_id={} media_type={} status={} definitions_reset={}",
            done.id,
            domain(done.media_type),
            done.status.text(),
            done.definitions_reset.unwrap_or(0)
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
    async fn enqueue(
        db: &Arc<Database>,
        media_type: MediaDomain,
        reset_titles: bool,
    ) -> Result<QualityResetCommand> {
        Ok(create(
            State(db.clone()),
            Ok(Query(Empty {})),
            Ok(Json(QualityResetInput {
                media_type,
                reset_titles,
                priority: CommandPriority::Normal,
            })),
        )
        .await?
        .1
        .0)
    }
    async fn actual_claim(db: &Database, id: Uuid) -> QualityResetCommand {
        let c = connection(db).await.unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        let command = claim(&tx, id, now().unwrap()).await.unwrap();
        tx.commit().await.unwrap();
        command
    }
    async fn seeded() -> (Arc<Database>, Scratch) {
        let path = std::env::temp_dir().join(format!("hrrdarr-quality-reset-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let db = Arc::new(Database::open_local(path.join("db")).await.unwrap());
        (db, Scratch(path))
    }
    async fn edited(c: &Connection, media: &str) {
        c.execute(
            "UPDATE quality_definitions SET title='Edited',min_size=1,max_size=999,preferred_size=500 WHERE media_type=?1",
            params![media],
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn run_resets_movie_sizes_and_optionally_titles_and_records_the_touched_count() {
        let (db, _scratch) = seeded().await;
        edited(&connection(&db).await.unwrap(), "movies").await;
        let total: i64 = connection(&db)
            .await
            .unwrap()
            .query(
                "SELECT count(*) FROM quality_definitions WHERE media_type='movies'",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        let command = enqueue(&db, MediaDomain::Movies, false).await.unwrap();
        let claimed = actual_claim(&db, command.id).await;
        run(&db, claimed).await.unwrap();
        let done = read(&connection(&db).await.unwrap(), command.id)
            .await
            .unwrap();
        assert!(matches!(done.status, CommandStatus::Succeeded));
        assert_eq!(done.definitions_reset, Some(total));
        let (title, min_size): (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='movies' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        assert_eq!(title, "Edited");
        assert_eq!(min_size, 0.);
        let command = enqueue(&db, MediaDomain::Movies, true).await.unwrap();
        let claimed = actual_claim(&db, command.id).await;
        run(&db, claimed).await.unwrap();
        let done = read(&connection(&db).await.unwrap(), command.id)
            .await
            .unwrap();
        assert_eq!(done.definitions_reset, Some(total));
        let title: String = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title FROM quality_definitions WHERE media_type='movies' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            r.get(0).unwrap()
        };
        assert_ne!(title, "Edited");
    }
    #[tokio::test]
    async fn run_on_tv_only_resets_titles_when_requested_and_leaves_sizes_alone() {
        let (db, _scratch) = seeded().await;
        edited(&connection(&db).await.unwrap(), "tv").await;
        let command = enqueue(&db, MediaDomain::Tv, false).await.unwrap();
        let claimed = actual_claim(&db, command.id).await;
        run(&db, claimed).await.unwrap();
        let done = read(&connection(&db).await.unwrap(), command.id)
            .await
            .unwrap();
        assert_eq!(done.definitions_reset, Some(0));
        let (title, min_size): (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='tv' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        assert_eq!(title, "Edited");
        assert_eq!(min_size, 1.);
        let command = enqueue(&db, MediaDomain::Tv, true).await.unwrap();
        let claimed = actual_claim(&db, command.id).await;
        run(&db, claimed).await.unwrap();
        let done = read(&connection(&db).await.unwrap(), command.id)
            .await
            .unwrap();
        assert!(done.definitions_reset.unwrap() > 0);
        let (title, min_size): (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='tv' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        assert_ne!(title, "Edited");
        assert_eq!(min_size, 1.);
    }
    #[tokio::test]
    async fn run_on_one_domain_leaves_the_other_domains_definitions_untouched() {
        let (db, _scratch) = seeded().await;
        edited(&connection(&db).await.unwrap(), "movies").await;
        edited(&connection(&db).await.unwrap(), "tv").await;
        let tv_before: (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='tv' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        let command = enqueue(&db, MediaDomain::Movies, true).await.unwrap();
        let claimed = actual_claim(&db, command.id).await;
        run(&db, claimed).await.unwrap();
        let done = read(&connection(&db).await.unwrap(), command.id)
            .await
            .unwrap();
        assert!(matches!(done.status, CommandStatus::Succeeded));
        let (movies_title, movies_min_size): (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='movies' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        assert_ne!(movies_title, "Edited");
        assert_eq!(movies_min_size, 0.);
        let tv_after: (String, f64) = {
            let r = connection(&db)
                .await
                .unwrap()
                .query(
                    "SELECT title,min_size FROM quality_definitions WHERE media_type='tv' LIMIT 1",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            (r.get(0).unwrap(), r.get(1).unwrap())
        };
        assert_eq!(tv_after, tv_before);
    }
    #[tokio::test]
    async fn second_active_command_for_same_media_type_is_idempotent_or_a_clean_conflict() {
        let (db, _scratch) = seeded().await;
        let first = enqueue(&db, MediaDomain::Tv, false).await.unwrap();
        let same = enqueue(&db, MediaDomain::Tv, false).await.unwrap();
        assert_eq!(first.id, same.id);
        let conflict = enqueue(&db, MediaDomain::Tv, true).await;
        assert!(matches!(
            conflict,
            Err(Error(StatusCode::CONFLICT, "command_conflict"))
        ));
        let other_domain = enqueue(&db, MediaDomain::Movies, true).await;
        assert!(other_domain.is_ok());
    }
    #[tokio::test]
    async fn worker_claim_and_interrupted_recovery_round_trip() {
        let _imports = crate::snapshots::IMPORT_TEST_LOCK.lock().await;
        let (mut db, scratch) = seeded().await;
        let dbpath = scratch.0.join("db");
        edited(&connection(&db).await.unwrap(), "movies").await;
        let command = enqueue(&db, MediaDomain::Movies, false).await.unwrap();
        let _claimed = actual_claim(&db, command.id).await;
        // Simulate interruption exactly between durable claim and atomic mutation.
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
        assert_eq!(done.attempts, 2);
        assert_eq!(done.definitions_reset, Some(30));
        let min_size: f64 = connection(&db)
            .await
            .unwrap()
            .query(
                "SELECT min_size FROM quality_definitions WHERE media_type='movies' LIMIT 1",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(min_size, 0.);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn real_http_create_conflict_list_and_detail_round_trip() {
        let (db, _scratch) = seeded().await;
        let app = super::super::router(db.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .post(format!("{base}/api/v1/quality-definitions/reset-commands"))
            .header("content-type", "application/json")
            .body(
                serde_json::json!({"media_type":"tv","reset_titles":false,"priority":"normal"})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        let created: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        let response = client
            .post(format!("{base}/api/v1/quality-definitions/reset-commands"))
            .header("content-type", "application/json")
            .body(
                serde_json::json!({"media_type":"tv","reset_titles":true,"priority":"normal"})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        let error: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(error["error"]["code"], "command_conflict");
        let response = client
            .get(format!(
                "{base}/api/v1/quality-definitions/reset-commands?media_type=tv"
            ))
            .send()
            .await
            .unwrap();
        let page: serde_json::Value =
            serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["id"], id);
        let response = client
            .get(format!(
                "{base}/api/v1/quality-definitions/reset-commands/{}",
                Uuid::new_v4()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        server.abort();
        let _ = server.await;
    }
}
