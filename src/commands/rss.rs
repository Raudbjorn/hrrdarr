//! Durable RSS decisions and guarded qBittorrent submission receipts.
use super::*;
use crate::providers::{RefreshClient, indexer, qbittorrent};
use crate::search::{Disposition, ReleaseTarget, SearchContext};

#[derive(Clone, Copy, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssTarget {
    pub media_type: MediaDomain,
    pub indexer_id: Uuid,
    pub indexer_revision: i64,
    pub client_id: Uuid,
    pub client_revision: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssInput {
    pub target: RssTarget,
    pub priority: CommandPriority,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssCommand {
    pub id: Uuid,
    pub target: RssTarget,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub attempts: u8,
    pub next_attempt_at: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub error_code: Option<String>,
    pub fetched: u32,
    pub evaluated: u32,
    pub rejected: u32,
    pub pending: u32,
    pub observed: u32,
    pub uncertain: u32,
    pub fetch_complete: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssCandidate {
    pub origin: super::search::CandidateOrigin,
    pub id: Uuid,
    pub command_id: Option<Uuid>,
    pub source: RssTarget,
    pub title: String,
    pub target: Option<ReleaseTarget>,
    pub status: String,
    pub reasons: Vec<String>,
    pub not_before: Option<i64>,
    pub attempts: u8,
    pub created_at: i64,
    pub updated_at: i64,
    pub error_code: Option<String>,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssScheduleInput {
    #[ts(optional)]
    pub revision: Option<i64>,
    pub target: RssTarget,
    pub interval_seconds: u32,
    pub enabled: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct RssSchedule {
    pub id: Uuid,
    pub target: RssTarget,
    pub revision: i64,
    pub interval_seconds: u32,
    pub enabled: bool,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
    pub error_code: Option<String>,
    pub created_at: i64,
}
const COLUMNS: &str = "id,media_type,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at,started_at,completed_at,error_code,fetched,evaluated,(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status IN ('rejected','cancelled')),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status='pending'),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status='observed'),(SELECT count(*) FROM rss_candidates rc WHERE rc.command_id=rss_commands.id AND rc.status IN ('submitting','reconciling','needs_attention')),fetch_complete";
fn target_row(r: &libsql::Row, start: i32) -> Result<RssTarget> {
    Ok(RssTarget {
        media_type: MediaDomain::parse(&r.get::<String>(start)?).map_err(|_| bad())?,
        indexer_id: Uuid::parse_str(&r.get::<String>(start + 1)?).map_err(|_| bad())?,
        indexer_revision: r.get(start + 2)?,
        client_id: Uuid::parse_str(&r.get::<String>(start + 3)?).map_err(|_| bad())?,
        client_revision: r.get(start + 4)?,
    })
}
fn row(r: libsql::Row) -> Result<RssCommand> {
    Ok(RssCommand {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        target: target_row(&r, 1)?,
        priority: if r.get::<i64>(6)? == 1 {
            CommandPriority::High
        } else {
            CommandPriority::Normal
        },
        status: CommandStatus::parse(&r.get::<String>(7)?)?,
        attempts: r.get::<i64>(8)? as u8,
        next_attempt_at: r.get(9)?,
        created_at: r.get(10)?,
        started_at: r.get(11)?,
        completed_at: r.get(12)?,
        error_code: r.get(13)?,
        fetched: r.get::<i64>(14)? as u32,
        evaluated: r.get::<i64>(15)? as u32,
        rejected: r.get::<i64>(16)? as u32,
        pending: r.get::<i64>(17)? as u32,
        observed: r.get::<i64>(18)? as u32,
        uncertain: r.get::<i64>(19)? as u32,
        fetch_complete: r.get::<i64>(20)? == 1,
    })
}
async fn read(c: &Connection, id: Uuid) -> Result<RssCommand> {
    row(c
        .query(
            &format!("SELECT {COLUMNS} FROM rss_commands WHERE id=?"),
            [id.to_string()],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(StatusCode::NOT_FOUND, "rss_command_not_found"))?)
}
pub(super) async fn valid_target(c: &Connection, t: RssTarget) -> Result<bool> {
    if t.indexer_revision <= 0
        || t.client_revision <= 0
        || t.indexer_revision > MAX_REVISION
        || t.client_revision > MAX_REVISION
    {
        return Ok(false);
    }
    Ok(c.query("SELECT EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.enabled=1 AND p.implementation IN ('torznab','newznab') AND s.media_type=?) AND EXISTS(SELECT 1 FROM providers p JOIN provider_scopes s ON s.provider_id=p.id WHERE p.id=? AND p.revision=? AND p.enabled=1 AND p.implementation='qbittorrent' AND s.media_type=?)",params![t.indexer_id.to_string(),t.indexer_revision,domain(t.media_type),t.client_id.to_string(),t.client_revision,domain(t.media_type)]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1)
}
pub(super) fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/rss/commands", get(list).post(create))
        .route("/api/v1/rss/commands/{id}", get(detail).delete(delete))
        .route("/api/v1/rss/commands/{id}/cancel", post(cancel))
        .route("/api/v1/rss/candidates", get(candidates))
        .route(
            "/api/v1/rss/candidates/{id}",
            axum::routing::delete(delete_candidate),
        )
        .route("/api/v1/rss/schedules", get(schedules).post(save_schedule))
        .route(
            "/api/v1/rss/schedules/{id}",
            axum::routing::delete(delete_schedule),
        )
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(deadline))
        .with_state(db)
}
async fn deadline(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => Error(StatusCode::SERVICE_UNAVAILABLE, "rss_timeout").into_response(),
    }
}
async fn enqueue(c: &Connection, input: RssInput, timestamp: i64) -> Result<RssCommand> {
    let t = input.target;
    if !valid_target(c, t).await? {
        return Err(Error(StatusCode::CONFLICT, "provider_changed"));
    }
    if let Some(r)=c.query(&format!("SELECT {COLUMNS} FROM rss_commands WHERE indexer_id=? AND client_id=? AND media_type=? AND status IN ('queued','running','retry_wait')"),params![t.indexer_id.to_string(),t.client_id.to_string(),domain(t.media_type)]).await?.next().await?{return row(r)}
    if command_capacity(c).await? >= MAX_COMMANDS {
        return Err(Error(StatusCode::TOO_MANY_REQUESTS, "command_history_full"));
    }
    let id = Uuid::new_v4();
    c.execute("INSERT INTO rss_commands(id,name,media_type,indexer_id,indexer_revision,client_id,client_revision,priority,status,attempts,next_attempt_at,created_at)VALUES(?,'rss_sync',?,?,?,?,?,?,'queued',0,?,?)",params![id.to_string(),domain(t.media_type),t.indexer_id.to_string(),t.indexer_revision,t.client_id.to_string(),t.client_revision,input.priority.number(),timestamp,timestamp]).await?;
    read(c, id).await
}
async fn create(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RssInput>, JsonRejection>,
) -> Result<(StatusCode, Json<RssCommand>)> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome = async { bounded(enqueue(&tx, input, now()?).await?) }.await;
    Ok((StatusCode::ACCEPTED, finish(tx, outcome).await?))
}
async fn list(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<CommandQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RssCommand>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0 || q.limit > 100 || q.offset > 10000 {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let media = q.media_type.map(domain);
    let status = q.status.map(CommandStatus::text);
    let outcome=async{
        let filter="(? IS NULL OR media_type=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM rss_commands WHERE {filter}"),params![media,media,status,status]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT {COLUMNS} FROM rss_commands WHERE {filter} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,status,status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut items=Vec::new();while let Some(r)=rows.next().await?{items.push(row(r)?)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn detail(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<RssCommand>> {
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
) -> Result<Json<RssCommand>> {
    q.map_err(|_| bad())?;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let timestamp=now()?;
        tx.execute("UPDATE rss_candidates SET status='cancelled',private_payload=NULL,updated_at=?,not_before=NULL,error_code=NULL WHERE command_id=? AND status IN ('pending','prepared')",params![timestamp,id.to_string()]).await?;
        tx.execute("UPDATE rss_commands SET status='cancelled',completed_at=?,error_code=NULL WHERE id=? AND status IN ('queued','running','retry_wait')",params![timestamp,id.to_string()]).await?;
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
        tx.execute("DELETE FROM rss_commands WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
pub(super) fn payload_context(id: Uuid, t: RssTarget) -> String {
    format!(
        "{id}/{}/{}/{}/{}/{}",
        t.indexer_id,
        t.indexer_revision,
        t.client_id,
        t.client_revision,
        domain(t.media_type)
    )
}
pub(super) fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
fn singleton(target: &Option<ReleaseTarget>) -> bool {
    matches!(target, Some(ReleaseTarget::Movies { .. }))
        || matches!(target,Some(ReleaseTarget::Tv{episode_ids,..}) if episode_ids.len()==1)
}
async fn capture_feed(db: &Database, client: &RefreshClient, command: &RssCommand) -> Result<()> {
    let t = command.target;
    let mut releases = Vec::new();
    let mut query_index = 0;
    let mut offset = 0;
    let mut release_bytes = 0usize;
    for page_number in 0..10 {
        let page = client
            .raw_search(
                &t.indexer_id.to_string(),
                t.indexer_revision,
                &indexer::IndexerSearch::Rss {
                    media_type: t.media_type,
                    offset,
                    query_index,
                    limit: 100,
                },
            )
            .await;
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                if let Some(seconds) = error.retry_after_seconds {
                    connection(db).await?.execute("UPDATE rss_commands SET next_attempt_at=? WHERE id=? AND status='running'",params![now()?+i64::from(seconds.clamp(1,86400)),command.id.to_string()]).await?;
                }
                return Err(Error(StatusCode::BAD_GATEWAY, error.code));
            }
        };
        if !page.warnings.is_empty() {
            return Err(Error(StatusCode::BAD_GATEWAY, "invalid_release"));
        }
        if releases.len() + page.items.len() > 1000 {
            return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "refresh_limit"));
        }
        for release in page.items {
            let bytes = indexer::encode_private(release)
                .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
            release_bytes = release_bytes.checked_add(bytes.len()).ok_or_else(bad)?;
            if release_bytes > 4 * 1024 * 1024 {
                return Err(Error(StatusCode::PAYLOAD_TOO_LARGE, "refresh_limit"));
            }
            releases.push(bytes);
        }
        let Some(next) = page.next_query else { break };
        if page_number == 9
            || next.query_index < query_index
            || (next.query_index == query_index && next.offset <= offset)
        {
            return Err(Error(StatusCode::BAD_GATEWAY, "refresh_limit"));
        }
        query_index = next.query_index;
        offset = next.offset;
    }
    let c = connection(db).await?;
    let timestamp = now()?;
    let mut captured = Vec::new();
    for mut encoded in releases {
        let release = indexer::decode_private(&encoded)
            .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
        encoded.fill(0);
        let mut decision =
            crate::search::evaluate(&c, t.media_type, &release, SearchContext::Rss, timestamp)
                .await
                .map_err(|e| Error(StatusCode::INTERNAL_SERVER_ERROR, e.0))?;
        if !matches!(decision.disposition, Disposition::Reject) && !singleton(&decision.target) {
            decision.disposition = Disposition::Reject;
            decision.reasons.push("unsupported_target".into());
            decision.target = None;
        }
        let fingerprint = digest(
            release
                .guid
                .as_deref()
                .filter(|v| !v.is_empty())
                .unwrap_or(&release.download_url)
                .as_bytes(),
        );
        let title = release
            .metadata
            .title
            .clone()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "(untitled release)".into());
        if title.len() > 1024 {
            return Err(Error(StatusCode::BAD_GATEWAY, "invalid_release"));
        }
        if matches!(decision.disposition, Disposition::Reject) {
            decision.target = None;
        }
        let id = Uuid::new_v4();
        let payload = if matches!(decision.disposition, Disposition::Reject) {
            None
        } else {
            let mut bytes = indexer::encode_private(release)
                .map_err(|_| Error(StatusCode::BAD_GATEWAY, "invalid_release"))?;
            let result = client.seal_release(&payload_context(id, t), &bytes);
            bytes.fill(0);
            Some(result.map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?)
        };
        captured.push((id, fingerprint, title, decision, payload));
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if !matches!(read(&tx,command.id).await?.status,CommandStatus::Running){return Ok(())}
        if !valid_target(&tx,t).await?{return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        let count=captured.len() as i64;
        for (id,fingerprint,title,decision,payload) in captured {
            if let Some(existing)=tx.query("SELECT id,status FROM rss_candidates WHERE indexer_id=? AND indexer_revision=? AND client_id=? AND client_revision=? AND media_type=? AND fingerprint=?",params![t.indexer_id.to_string(),t.indexer_revision,t.client_id.to_string(),t.client_revision,domain(t.media_type),fingerprint.clone()]).await?.next().await?{
                let existing_id:String=existing.get(0)?;let status:String=existing.get(1)?;
                if matches!(status.as_str(),"rejected"|"cancelled") {tx.execute("DELETE FROM rss_candidates WHERE id=?",[existing_id]).await?;}else{continue}
            }
            if tx.query("SELECT count(*)>=1024 OR COALESCE(sum(length(private_payload)),0)+(SELECT COALESCE(sum(length(private_payload)),0) FROM search_results)+?>16777216 FROM rss_candidates",[payload.as_ref().map_or(0,Vec::len) as i64]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?==1{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"candidate_limit"))}
            let (series,movie,episode)=match decision.target{Some(ReleaseTarget::Tv{series_id,episode_ids}) if episode_ids.len()==1=>(Some(series_id),None,episode_ids.first().copied()),Some(ReleaseTarget::Movies{movie_id})=>(None,Some(movie_id),None),_=>(None,None,None)};
            let status=if matches!(decision.disposition,Disposition::Reject){"rejected"}else{"pending"};
            let reasons=serde_json::to_string(&decision.reasons).map_err(|_|bad())?;
            tx.execute("INSERT INTO rss_candidates(id,command_id,indexer_id,indexer_revision,client_id,client_revision,media_type,fingerprint,title,private_payload,series_id,movie_id,status,decision_reasons_json,not_before,attempts,created_at,updated_at)VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,0,?,?)",params![id.to_string(),command.id.to_string(),t.indexer_id.to_string(),t.indexer_revision,t.client_id.to_string(),t.client_revision,domain(t.media_type),fingerprint,title,payload,series,movie,status,reasons,decision.not_before,timestamp,timestamp]).await?;
            if let Some(episode)=episode{tx.execute("INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id)VALUES(?,?,?)",params![id.to_string(),series,episode]).await?;}
        }
        tx.execute("UPDATE rss_commands SET fetch_complete=1,fetched=?,evaluated=? WHERE id=?",params![count,count,command.id.to_string()]).await?;Ok(())
    }.await;
    finish(tx, outcome).await
}
pub(super) async fn recover(c: &Connection, timestamp: i64, code: &str) -> Result<()> {
    // A persisted dispatch boundary is irrevocable: never reconstruct a payload or POST again.
    c.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,not_before=?,updated_at=?,error_code='submission_unknown' WHERE status='submitting'",params![timestamp,timestamp]).await?;
    c.execute("UPDATE rss_commands SET status=CASE WHEN attempts<3 THEN 'retry_wait' ELSE 'failed' END,next_attempt_at=max(next_attempt_at,?),completed_at=CASE WHEN attempts<3 THEN NULL ELSE ? END,error_code=? WHERE status='running'",params![timestamp,timestamp,code]).await?;
    Ok(())
}
pub(super) async fn claim(c: &Connection, id: Uuid, timestamp: i64) -> Result<RssCommand> {
    let current = read(c, id).await?;
    if !matches!(current.status, CommandStatus::Running) {
        c.execute("UPDATE rss_commands SET status='running',attempts=attempts+1,started_at=?,error_code=NULL WHERE id=?",params![timestamp,id.to_string()]).await?;
    }
    read(c, id).await
}
async fn settle_error(db: &Database, command: &RssCommand, code: &str) -> Result<()> {
    let code = match code {
        "provider_changed"
        | "provider_unavailable"
        | "refresh_timeout"
        | "refresh_limit"
        | "refresh_failed"
        | "key_unavailable"
        | "invalid_release"
        | "candidate_limit" => code,
        _ => "storage_error",
    };
    let retry = matches!(code, "refresh_timeout" | "refresh_failed" | "storage_error")
        && command.attempts < 3;
    let c = connection(db).await?;
    let timestamp = now()?;
    c.execute("UPDATE rss_commands SET status=?,next_attempt_at=max(next_attempt_at,?),completed_at=?,error_code=? WHERE id=? AND status='running'",params![if retry{"retry_wait"}else{"failed"},timestamp+30,if retry{None}else{Some(timestamp)},code,command.id.to_string()]).await?;
    Ok(())
}
struct CandidateWork {
    public: RssCandidate,
    payload: Option<Vec<u8>>,
    identity: Option<qbittorrent::SubmissionIdentity>,
}
async fn candidate(c: &Connection, id: Uuid) -> Result<CandidateWork> {
    let row=c.query("SELECT command_id,media_type,indexer_id,indexer_revision,client_id,client_revision,title,series_id,movie_id,status,decision_reasons_json,not_before,attempts,created_at,updated_at,error_code,private_payload,submission_identity_json FROM rss_candidates WHERE id=?",[id.to_string()]).await?.next().await?.ok_or(Error(StatusCode::NOT_FOUND,"rss_candidate_not_found"))?;
    let source = target_row(&row, 1)?;
    let series: Option<i64> = row.get(7)?;
    let movie: Option<i64> = row.get(8)?;
    let mut public = RssCandidate {
        origin: super::search::origin(c, id).await?,
        id,
        command_id: row
            .get::<Option<String>>(0)?
            .map(|v| Uuid::parse_str(&v).map_err(|_| bad()))
            .transpose()?,
        source,
        title: row.get(6)?,
        target: movie.map(|movie_id| ReleaseTarget::Movies { movie_id }),
        status: row.get(9)?,
        reasons: serde_json::from_str(&row.get::<String>(10)?).map_err(|_| bad())?,
        not_before: row.get(11)?,
        attempts: row.get::<i64>(12)? as u8,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        error_code: row.get(15)?,
    };
    let payload = row.get(16)?;
    let identity = row
        .get::<Option<String>>(17)?
        .map(|v| serde_json::from_str(&v).map_err(|_| bad()))
        .transpose()?;
    if let Some(series_id) = series {
        let mut rows=c.query("SELECT episode_id FROM rss_candidate_episodes WHERE candidate_id=? ORDER BY episode_id",[id.to_string()]).await?;
        let mut ids = Vec::new();
        while let Some(r) = rows.next().await? {
            ids.push(r.get(0)?)
        }
        if !ids.is_empty() {
            public.target = Some(ReleaseTarget::Tv {
                series_id,
                episode_ids: ids,
            });
        }
    }
    Ok(CandidateWork {
        public,
        payload,
        identity,
    })
}
pub(super) async fn public_candidate(c: &Connection, id: Uuid) -> Result<RssCandidate> {
    Ok(candidate(c, id).await?.public)
}
async fn candidate_state(
    c: &Connection,
    id: Uuid,
    status: &str,
    code: Option<&str>,
    not_before: Option<i64>,
) -> Result<()> {
    let allowed = match status {
        "rejected" | "cancelled" => "'pending','prepared'",
        "observed" => "'submitting'",
        "needs_attention" => "'submitting','reconciling'",
        _ => return Err(bad()),
    };
    c.execute(&format!("UPDATE rss_candidates SET status=?,private_payload=CASE WHEN ? IN ('rejected','cancelled') THEN NULL ELSE private_payload END,error_code=?,not_before=?,updated_at=? WHERE id=? AND status IN ({allowed})"),params![status,status,code,not_before,now()?,id.to_string()]).await?;
    Ok(())
}

async fn reconcile(db: &Database, client: &RefreshClient, work: CandidateWork) -> Result<()> {
    let p = work.public;
    let c = connection(db).await?;
    let identity = work.identity.ok_or_else(bad)?;
    let result = client
        .reconcile_download(
            &p.source.client_id.to_string(),
            p.source.client_revision,
            &identity,
        )
        .await;
    match result {
        Ok(qbittorrent::Reconciliation::Observed { .. }) => {
            candidate_state(
                &c,
                p.id,
                "needs_attention",
                Some("presence_unconfirmed"),
                None,
            )
            .await
        }
        Ok(qbittorrent::Reconciliation::NotObserved) if p.attempts >= 3 => {
            candidate_state(&c, p.id, "needs_attention", Some("not_observed"), None).await
        }
        Err(ref error) if !error.retryable || p.attempts >= 3 => {
            candidate_state(&c, p.id, "needs_attention", Some(error.code), None).await
        }
        result => {
            let delay = result
                .err()
                .and_then(|e| e.retry_after_seconds)
                .unwrap_or(30)
                .clamp(30, 86400);
            c.execute("UPDATE rss_candidates SET status='reconciling',attempts=attempts+1,error_code='submission_unknown',not_before=?,updated_at=? WHERE id=?",params![now()?+i64::from(delay),now()?,p.id.to_string()]).await?;
            Ok(())
        }
    }
}
async fn process_candidate(
    db: &Database,
    client: &RefreshClient,
    work: CandidateWork,
) -> Result<()> {
    if work.public.status == "reconciling" {
        return reconcile(db, client, work).await;
    }
    let p = work.public;
    let c = connection(db).await?;
    if !valid_target(&c, p.source).await? {
        return candidate_state(&c, p.id, "rejected", Some("provider_changed"), None).await;
    }
    let mut bytes = client
        .open_release(
            &payload_context(p.id, p.source),
            work.payload.as_deref().ok_or_else(bad)?,
        )
        .map_err(|e| Error(StatusCode::SERVICE_UNAVAILABLE, e.code))?;
    let decoded = indexer::decode_private(&bytes);
    bytes.fill(0);
    let release = decoded.map_err(|_| bad())?;
    let decision = crate::search::evaluate(
        &c,
        p.source.media_type,
        &release,
        super::search::authority(&c, p.id).await?,
        now()?,
    )
    .await
    .map_err(|e| Error(StatusCode::INTERNAL_SERVER_ERROR, e.0))?;
    if !singleton(&decision.target) || matches!(decision.disposition, Disposition::Reject) {
        let reasons = if singleton(&decision.target) {
            decision.reasons
        } else {
            vec!["unsupported_target".into()]
        };
        c.execute("UPDATE rss_candidates SET status='rejected',private_payload=NULL,decision_reasons_json=?,not_before=NULL,updated_at=? WHERE id=?",params![serde_json::to_string(&reasons).map_err(|_|bad())?,now()?,p.id.to_string()]).await?;
        return Ok(());
    }
    if matches!(decision.disposition, Disposition::Delay) {
        if p.status == "prepared" {
            return candidate_state(&c, p.id, "rejected", Some("target_changed"), None).await;
        }
        c.execute("UPDATE rss_candidates SET decision_reasons_json=?,not_before=?,updated_at=? WHERE id=?",params![serde_json::to_string(&decision.reasons).map_err(|_|bad())?,decision.not_before,now()?,p.id.to_string()]).await?;
        return Ok(());
    }
    if p.target != decision.target {
        return candidate_state(&c, p.id, "rejected", Some("target_changed"), None).await;
    }
    let target = decision.target.ok_or_else(bad)?;
    let (media_target, series, movie, episode) = match &target {
        ReleaseTarget::Tv {
            series_id,
            episode_ids,
        } => (
            crate::db::MediaTarget::Episode(episode_ids[0]),
            Some(*series_id),
            None,
            Some(episode_ids[0]),
        ),
        ReleaseTarget::Movies { movie_id } => (
            crate::db::MediaTarget::Movie(*movie_id),
            None,
            Some(*movie_id),
            None,
        ),
    };
    let source = match &release.facts.torrent {
        Some(facts) => match &facts.magnet_url {
            Some(value) => qbittorrent::AddSource::Magnet(value.clone()),
            None => qbittorrent::AddSource::Url(release.download_url.clone()),
        },
        None => {
            return candidate_state(&c, p.id, "rejected", Some("unsupported_target"), None).await;
        }
    };
    let mut options = qbittorrent::QbitOptions::default();
    if let Some(facts) = &release.facts.torrent {
        options.ratio_limit = facts.minimum_ratio;
        options.seeding_minutes = facts
            .minimum_seed_seconds
            .and_then(|v| v.checked_add(59))
            .and_then(|v| i64::try_from(v / 60).ok());
    }
    let prepared = match client
        .prepare_download(
            &p.source.client_id.to_string(),
            p.source.client_revision,
            media_target,
            source,
            false,
            options,
        )
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => return candidate_state(&c, p.id, "rejected", Some(error.code), None).await,
    };
    let identity = serde_json::to_string(prepared.identity()).map_err(|_| bad())?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        let current=candidate(&tx,p.id).await?;
        if current.public.status!=p.status{return Ok(false)}
        if !valid_target(&tx,p.source).await?{candidate_state(&tx,p.id,"rejected",Some("provider_changed"),None).await?;return Ok(false)}
        // Recheck all current local decision facts after preparation's network reads.
        let latest=crate::search::evaluate(&tx,p.source.media_type,&release,super::search::authority(&tx,p.id).await?,now()?).await.map_err(|e|Error(StatusCode::INTERNAL_SERVER_ERROR,e.0))?;
        if !matches!(latest.disposition,Disposition::Accept) || latest.target.as_ref()!=Some(&target) {
            candidate_state(&tx,p.id,"rejected",Some("target_changed"),None).await?;return Ok(false)
        }
        if p.status=="pending" {
            let collision=if let Some(movie)=movie {
                tx.query("SELECT 1 FROM rss_candidates c WHERE movie_id=? AND id!=? AND status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![movie,p.id.to_string()]).await?.next().await?.is_some()
            }else{
                tx.query("SELECT 1 FROM rss_candidate_episodes e JOIN rss_candidates c ON c.id=e.candidate_id WHERE e.episode_id=? AND c.id!=? AND c.status IN ('prepared','submitting','reconciling','observed','needs_attention') AND NOT EXISTS(SELECT 1 FROM rss_candidate_imports i JOIN import_journal j ON j.operation_id=i.operation_id JOIN import_history h ON h.operation_id=j.operation_id WHERE i.candidate_id=c.id AND j.phase='complete') LIMIT 1",params![episode,p.id.to_string()]).await?.next().await?.is_some()
            };
            if collision{candidate_state(&tx,p.id,"rejected",Some("target_conflict"),None).await?;return Ok(false)}
            tx.execute("DELETE FROM rss_candidate_episodes WHERE candidate_id=?",[p.id.to_string()]).await?;
            tx.execute("UPDATE rss_candidates SET series_id=?,movie_id=?,updated_at=? WHERE id=?",params![series,movie,now()?,p.id.to_string()]).await?;
            if let Some(episode)=episode{tx.execute("INSERT INTO rss_candidate_episodes(candidate_id,series_id,episode_id)VALUES(?,?,?)",params![p.id.to_string(),series,episode]).await?;}
            tx.execute("UPDATE rss_candidates SET status='prepared',submission_identity_json=?,not_before=NULL,decision_reasons_json='[]',updated_at=? WHERE id=?",params![identity.clone(),now()?,p.id.to_string()]).await?;
            return Ok(false)
        }
        if serde_json::to_string(current.identity.as_ref().ok_or_else(bad)?).map_err(|_|bad())?!=identity {
            candidate_state(&tx,p.id,"rejected",Some("target_changed"),None).await?;return Ok(false)
        }
        for hash in prepared.identity().hashes(){
            if tx.query("SELECT 1 FROM rss_hash_claims WHERE client_id=? AND hash=?",params![p.source.client_id.to_string(),hash.clone()]).await?.next().await?.is_some(){candidate_state(&tx,p.id,"rejected",Some("hash_conflict"),None).await?;return Ok(false)}
        }
        for hash in prepared.identity().hashes(){tx.execute("INSERT INTO rss_hash_claims(client_id,hash,candidate_id)VALUES(?,?,?)",params![p.source.client_id.to_string(),hash.clone(),p.id.to_string()]).await?;}
        let parsed=latest.parsed.as_ref().ok_or_else(bad)?;
        let mut evidence=crate::custom_formats::release(&release,parsed,matches!(p.source.media_type,MediaDomain::Tv));
        crate::custom_formats::populate_quality(&tx,&mut evidence,parsed.quality_name.as_deref(),matches!(p.source.media_type,MediaDomain::Tv)).await.map_err(|e|Error(StatusCode::INTERNAL_SERVER_ERROR,e.0))?;
        let evidence=serde_json::to_string(&evidence).map_err(|_|bad())?;
        if evidence.len()>16384{return Err(Error(StatusCode::CONFLICT,"invalid_release"))}
        tx.execute("UPDATE rss_candidates SET status='submitting',private_payload=NULL,comparison_facts_json=?,updated_at=? WHERE id=?",params![evidence,now()?,p.id.to_string()]).await?;Ok(true)
    }.await;
    if !finish(tx, outcome).await? {
        return Ok(());
    }
    // No cancellation or storage failure beyond this durable boundary can authorize another POST.
    match client
        .submit_download(
            &p.source.client_id.to_string(),
            p.source.client_revision,
            prepared,
        )
        .await
    {
        Ok(qbittorrent::AddOutcome::Observed { hash }) => {
            c.execute("UPDATE rss_candidates SET status='observed',observed_hash=?,error_code=NULL,not_before=NULL,updated_at=? WHERE id=? AND status='submitting'",params![hash,now()?,p.id.to_string()]).await?;
            Ok(())
        }
        Ok(qbittorrent::AddOutcome::AlreadyPresent { .. }) => {
            candidate_state(
                &c,
                p.id,
                "needs_attention",
                Some("preexisting_download"),
                None,
            )
            .await
        }
        result => {
            let delay = result
                .err()
                .and_then(|e| e.retry_after_seconds)
                .unwrap_or(30)
                .clamp(30, 86400);
            c.execute("UPDATE rss_candidates SET status='reconciling',attempts=1,error_code='submission_unknown',not_before=?,updated_at=? WHERE id=?",params![now()?+i64::from(delay),now()?,p.id.to_string()]).await?;
            Ok(())
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssCandidateQuery {
    #[ts(optional)]
    pub media_type: Option<MediaDomain>,
    #[ts(optional)]
    pub command_id: Option<Uuid>,
    #[ts(optional)]
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    #[ts(as = "Option<u16>", optional)]
    pub limit: u16,
    #[serde(default)]
    #[ts(as = "Option<u32>", optional)]
    pub offset: u32,
}
async fn candidates(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<RssCandidateQuery>, QueryRejection>,
) -> Result<Json<ApiPage<RssCandidate>>> {
    let q = q.map_err(|_| bad())?.0;
    if q.limit == 0
        || q.limit > 100
        || q.offset > 10000
        || q.status.as_ref().is_some_and(|s| {
            !matches!(
                s.as_str(),
                "pending"
                    | "rejected"
                    | "prepared"
                    | "submitting"
                    | "reconciling"
                    | "observed"
                    | "needs_attention"
                    | "cancelled"
            )
        })
    {
        return Err(bad());
    }
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::ReadOnly)
        .await?;
    let outcome=async{
        let media=q.media_type.map(domain);let command=q.command_id.map(|v|v.to_string());
        let filter="(? IS NULL OR media_type=?) AND (? IS NULL OR command_id=?) AND (? IS NULL OR status=?)";
        let total=tx.query(&format!("SELECT count(*) FROM rss_candidates WHERE {filter}"),params![media,media,command.clone(),command.clone(),q.status.clone(),q.status.clone()]).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)?;
        let mut rows=tx.query(&format!("SELECT id FROM rss_candidates WHERE {filter} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?"),params![media,media,command.clone(),command,q.status.clone(),q.status,i64::from(q.limit),i64::from(q.offset)]).await?;
        let mut ids=Vec::new();while let Some(r)=rows.next().await?{ids.push(Uuid::parse_str(&r.get::<String>(0)?).map_err(|_|bad())?)}drop(rows);
        let mut items=Vec::new();for id in ids{items.push(candidate(&tx,id).await?.public)}
        bounded(ApiPage{items,total,limit:q.limit,offset:q.offset})
    }.await;
    finish(tx, outcome).await
}
async fn delete_candidate(
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
        if !matches!(
            candidate(&tx, id).await?.public.status.as_str(),
            "rejected" | "cancelled"
        ) {
            return Err(conflict());
        }
        if !matches!(
            super::search::origin(&tx, id).await?,
            super::search::CandidateOrigin::Rss
        ) {
            return Err(conflict());
        }
        tx.execute("DELETE FROM rss_candidates WHERE id=?", [id.to_string()])
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    finish(tx, outcome).await
}
const SCHEDULE_COLUMNS: &str = "id,media_type,indexer_id,indexer_revision,client_id,client_revision,revision,interval_seconds,enabled,next_run_at,last_run_at,error_code,created_at";
fn schedule_row(r: libsql::Row) -> Result<RssSchedule> {
    Ok(RssSchedule {
        id: Uuid::parse_str(&r.get::<String>(0)?).map_err(|_| bad())?,
        target: target_row(&r, 1)?,
        revision: r.get(6)?,
        interval_seconds: r.get::<i64>(7)? as u32,
        enabled: r.get::<i64>(8)? == 1,
        next_run_at: r.get(9)?,
        last_run_at: r.get(10)?,
        error_code: r.get(11)?,
        created_at: r.get(12)?,
    })
}
async fn schedules(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
) -> Result<Json<Vec<RssSchedule>>> {
    q.map_err(|_| bad())?;
    let c = connection(&db).await?;
    let mut rows = c
        .query(
            &format!(
                "SELECT {SCHEDULE_COLUMNS} FROM rss_schedules ORDER BY created_at,id LIMIT 64"
            ),
            (),
        )
        .await?;
    let mut items = Vec::new();
    while let Some(r) = rows.next().await? {
        items.push(schedule_row(r)?)
    }
    bounded(items)
}
async fn save_schedule(
    State(db): State<Arc<Database>>,
    q: std::result::Result<Query<Empty>, QueryRejection>,
    input: std::result::Result<Json<RssScheduleInput>, JsonRejection>,
) -> Result<Json<RssSchedule>> {
    q.map_err(|_| bad())?;
    let input = input.map_err(|_| bad())?.0;
    if !(60..=86400).contains(&input.interval_seconds) {
        return Err(bad());
    }
    let t = input.target;
    let c = connection(&db).await?;
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if !valid_target(&tx,t).await?{return Err(Error(StatusCode::CONFLICT,"provider_changed"))}
        let prior=tx.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE indexer_id=? AND client_id=? AND media_type=?"),params![t.indexer_id.to_string(),t.client_id.to_string(),domain(t.media_type)]).await?.next().await?.map(schedule_row).transpose()?;
        let timestamp=now()?;
        let id=match prior {
            Some(prior)=>{
                if input.revision!=Some(prior.revision)||prior.revision>=MAX_REVISION{return Err(conflict())}
                tx.execute("UPDATE rss_schedules SET indexer_revision=?,client_revision=?,revision=revision+1,interval_seconds=?,enabled=?,next_run_at=?,error_code=NULL WHERE id=?",params![t.indexer_revision,t.client_revision,i64::from(input.interval_seconds),i64::from(input.enabled),timestamp+i64::from(input.interval_seconds),prior.id.to_string()]).await?;prior.id
            },None=>{
                if input.revision.is_some(){return Err(conflict())}
                if tx.query("SELECT count(*) FROM rss_schedules",()).await?.next().await?.ok_or_else(bad)?.get::<i64>(0)? >=64{return Err(Error(StatusCode::TOO_MANY_REQUESTS,"rss_schedule_limit"))}
                let id=Uuid::new_v4();tx.execute("INSERT INTO rss_schedules(id,media_type,indexer_id,indexer_revision,client_id,client_revision,interval_seconds,enabled,next_run_at,created_at)VALUES(?,?,?,?,?,?,?,?,?,?)",params![id.to_string(),domain(t.media_type),t.indexer_id.to_string(),t.indexer_revision,t.client_id.to_string(),t.client_revision,i64::from(input.interval_seconds),i64::from(input.enabled),timestamp+i64::from(input.interval_seconds),timestamp]).await?;id
            }
        };
        bounded(schedule_row(tx.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE id=?"),[id.to_string()]).await?.next().await?.ok_or_else(bad)?)?)
    }.await;
    finish(tx, outcome).await
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RssScheduleDelete {
    pub revision: i64,
}
async fn delete_schedule(
    State(db): State<Arc<Database>>,
    Path(id): Path<String>,
    q: std::result::Result<Query<RssScheduleDelete>, QueryRejection>,
) -> Result<StatusCode> {
    let q = q.map_err(|_| bad())?.0;
    let id = Uuid::parse_str(&id).map_err(|_| bad())?;
    let c = connection(&db).await?;
    if c.execute(
        "DELETE FROM rss_schedules WHERE id=? AND revision=?",
        params![id.to_string(), q.revision],
    )
    .await?
        != 1
    {
        return Err(conflict());
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn schedule_due(c: &Connection, timestamp: i64) -> Result<()> {
    let mut rows=c.query(&format!("SELECT {SCHEDULE_COLUMNS} FROM rss_schedules WHERE enabled=1 AND next_run_at<=? ORDER BY next_run_at,id LIMIT 64"),[timestamp]).await?;
    let mut due = Vec::new();
    while let Some(r) = rows.next().await? {
        due.push(schedule_row(r)?)
    }
    drop(rows);
    for schedule in due {
        let error = match enqueue(
            c,
            RssInput {
                target: schedule.target,
                priority: CommandPriority::Normal,
            },
            timestamp,
        )
        .await
        {
            Ok(_) => None,
            Err(Error(
                _,
                code @ ("command_history_full" | "command_conflict" | "provider_changed"),
            )) => Some(code),
            Err(e) => return Err(e),
        };
        c.execute("UPDATE rss_schedules SET next_run_at=?,last_run_at=CASE WHEN ? IS NULL THEN ? ELSE last_run_at END,error_code=?,enabled=CASE WHEN ?='provider_changed' THEN 0 ELSE enabled END WHERE id=?",params![timestamp+i64::from(schedule.interval_seconds),error,timestamp,error,error,schedule.id.to_string()]).await?;
    }
    Ok(())
}
pub(super) async fn run(db: &Database, client: &RefreshClient, command: RssCommand) -> Result<()> {
    if !command.fetch_complete {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(35),
            capture_feed(db, client, &command),
        )
        .await;
        return match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => settle_error(db, &command, error.1).await,
            Err(_) => settle_error(db, &command, "refresh_timeout").await,
        };
    }
    let c = connection(db).await?;
    let id=c.query("SELECT id FROM rss_candidates WHERE command_id=? AND (status='prepared' OR (status='pending' AND (not_before IS NULL OR not_before<=?))) ORDER BY created_at,id LIMIT 1",params![command.id.to_string(),now()?]).await?.next().await?.map(|r|r.get::<String>(0)).transpose()?;
    if let Some(id) = id {
        let id = Uuid::parse_str(&id).map_err(|_| bad())?;
        if let Err(error) = process_candidate(db, client, candidate(&c, id).await?).await {
            let status: String = c
                .query(
                    "SELECT status FROM rss_candidates WHERE id=?",
                    [id.to_string()],
                )
                .await?
                .next()
                .await?
                .ok_or_else(bad)?
                .get(0)?;
            if matches!(status.as_str(), "submitting" | "reconciling") {
                return Err(error);
            }
            return settle_error(db, &command, error.1).await;
        }
    }
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let outcome=async{
        if !matches!(read(&tx,command.id).await?.status,CommandStatus::Running){return Ok(())}
        let counts=tx.query("SELECT sum(status IN ('rejected','cancelled')),sum(status='pending'),sum(status='observed'),sum(status IN ('submitting','reconciling','needs_attention')),sum(status='prepared' OR (status='pending' AND (not_before IS NULL OR not_before<=?))) FROM rss_candidates WHERE command_id=?",params![now()?,command.id.to_string()]).await?.next().await?.ok_or_else(bad)?;
        let rejected=counts.get::<Option<i64>>(0)?.unwrap_or(0);let pending=counts.get::<Option<i64>>(1)?.unwrap_or(0);let observed=counts.get::<Option<i64>>(2)?.unwrap_or(0);let uncertain=counts.get::<Option<i64>>(3)?.unwrap_or(0);let immediate=counts.get::<Option<i64>>(4)?.unwrap_or(0);
        tx.execute("UPDATE rss_commands SET rejected=?,pending=?,observed=?,uncertain=?,next_attempt_at=? WHERE id=?",params![rejected,pending,observed,uncertain,now()?+1,command.id.to_string()]).await?;
        if immediate==0 {
            tx.execute("UPDATE rss_commands SET status=?,completed_at=?,error_code=? WHERE id=?",params![if uncertain>0{"failed"}else{"succeeded"},now()?,if uncertain>0{Some("submission_unknown")}else{None},command.id.to_string()]).await?;
        }
        Ok(())
    }.await;
    finish(tx, outcome).await
}
pub(super) async fn run_due(db: &Database, client: &RefreshClient, id: Uuid) -> Result<()> {
    let c = connection(db).await?;
    if let Err(error) = process_candidate(db, client, candidate(&c, id).await?).await {
        let code = match error.1 {
            "key_unavailable"
            | "target_changed"
            | "provider_changed"
            | "provider_unavailable"
            | "refresh_timeout"
            | "refresh_failed"
            | "invalid_release" => error.1,
            _ => "storage_error",
        };
        eprintln!("event=rss_candidate_error candidate_id={id} code={code}");
        let current: String = c
            .query(
                "SELECT status FROM rss_candidates WHERE id=?",
                [id.to_string()],
            )
            .await?
            .next()
            .await?
            .ok_or_else(bad)?
            .get(0)?;
        if matches!(current.as_str(), "submitting" | "reconciling") {
            return Err(error);
        }
        candidate_state(&c, id, "rejected", Some(code), None).await?;
    }
    Ok(())
}
