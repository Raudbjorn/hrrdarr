//! Domain-scoped proper/repack preference; writers share atomic pending reconsideration.
use crate::{api::MediaDomain, db::Database};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "RevisionPolicyMode")]
pub enum Mode {
    PreferAndUpgrade,
    DoNotUpgrade,
    DoNotPrefer,
}
impl Mode {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::PreferAndUpgrade => "prefer_and_upgrade",
            Self::DoNotUpgrade => "do_not_upgrade",
            Self::DoNotPrefer => "do_not_prefer",
        }
    }
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "prefer_and_upgrade" => Some(Self::PreferAndUpgrade),
            "do_not_upgrade" => Some(Self::DoNotUpgrade),
            "do_not_prefer" => Some(Self::DoNotPrefer),
            _ => None,
        }
    }
}
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[ts(rename = "RevisionPolicy")]
pub struct Policy {
    pub media_type: MediaDomain,
    pub mode: Mode,
    pub revision: i64,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "RevisionPolicyUpdate")]
pub struct Update {
    pub mode: Mode,
    pub revision: i64,
}
#[derive(Debug)]
pub struct Error(StatusCode, &'static str);
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(
                self.1,
                "Revision policy operation failed",
            )),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "revision_policy_storage_error",
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn domain(m: MediaDomain) -> &'static str {
    match m {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
pub async fn read(c: &Connection, m: MediaDomain) -> Result<Policy> {
    let r = c
        .query(
            "SELECT mode,revision FROM revision_policies WHERE media_type=?",
            [domain(m)],
        )
        .await?
        .next()
        .await?
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "revision_policy_missing",
        ))?;
    Ok(Policy {
        media_type: m,
        mode: Mode::parse(&r.get::<String>(0)?).ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "revision_policy_invalid",
        ))?,
        revision: r.get(1)?,
    })
}
/// Caller must own an immediate transaction. Never changes external-action ownership.
pub(crate) async fn set(
    c: &Connection,
    m: MediaDomain,
    mode: Mode,
    expected: i64,
    local: bool,
) -> Result<Policy> {
    if c.is_autocommit() {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "revision_policy_transaction_required",
        ));
    }
    let old = read(c, m).await?;
    if expected != old.revision || !(1..9_007_199_254_740_991).contains(&expected) {
        return Err(Error(StatusCode::CONFLICT, "revision_policy_conflict"));
    }
    c.execute("UPDATE revision_policies SET mode=?,revision=revision+1,locally_edited=CASE WHEN ? THEN 1 ELSE locally_edited END WHERE media_type=? AND revision=?",params![mode.text(),local,domain(m),expected]).await?;
    if mode != old.mode {
        c.execute("UPDATE rss_commands SET next_attempt_at=0 WHERE media_type=? AND next_attempt_at>0 AND status IN ('queued','retry_wait','running') AND EXISTS(SELECT 1 FROM rss_candidates r WHERE r.command_id=rss_commands.id AND r.status='pending' AND r.private_payload IS NOT NULL)",[domain(m)]).await?;
        c.execute(
            "UPDATE rss_candidates SET not_before=0 WHERE media_type=? AND status='pending' AND private_payload IS NOT NULL",
            [domain(m)],
        )
        .await?;
    }
    read(c, m).await
}
pub fn router(db: Arc<Database>) -> Router {
    let mut r = Router::new();
    for m in [MediaDomain::Tv, MediaDomain::Movies] {
        r = r.merge(
            Router::new()
                .route(
                    &format!("/api/v1/{}/revision-policy", domain(m)),
                    get(fetch).put(update),
                )
                .layer(Extension(m))
                .layer(DefaultBodyLimit::max(4096))
                .with_state(db.clone()),
        );
    }
    r
}
fn query(q: &BTreeMap<String, String>) -> Result<()> {
    if !q.is_empty() {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    Ok(())
}
async fn fetch(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Result<Json<Policy>> {
    query(&q)?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        Ok(Json(read(&c, m).await?))
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "revision_policy_timeout"))?
}
async fn update(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<MediaDomain>,
    Query(q): Query<BTreeMap<String, String>>,
    body: std::result::Result<Json<Update>, JsonRejection>,
) -> Result<Json<Policy>> {
    query(&q)?;
    let Json(input) =
        body.map_err(|_| Error(StatusCode::BAD_REQUEST, "invalid_revision_policy"))?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c = db.connect().await?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        let result = set(&tx, m, input.mode, input.revision, true).await;
        match result {
            Ok(p) => {
                tx.commit().await?;
                Ok(Json(p))
            }
            Err(e) => {
                tx.rollback().await?;
                Err(e)
            }
        }
    })
    .await
    .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "revision_policy_timeout"))?
}
