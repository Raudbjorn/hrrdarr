//! Native release evaluation shared by interactive search and durable RSS.
mod decision;
pub(crate) mod downloaded;
pub mod parser;
use crate::api::MediaDomain;
use crate::{
    db::Database,
    providers::{
        RefreshClient,
        indexer::{IndexerSearch, TvNumbering, TvSearchMode},
    },
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
pub use decision::evaluate;
pub(crate) use decision::target_ranks;
use libsql::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "media_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReleaseTarget {
    Tv {
        series_id: i64,
        episode_ids: Vec<i64>,
    },
    Movies {
        movie_id: i64,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchContext {
    Rss,
    UserSearch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Accept,
    Reject,
    Delay,
}
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ReleaseDecision {
    pub target: Option<ReleaseTarget>,
    pub disposition: Disposition,
    pub reasons: Vec<String>,
    pub not_before: Option<i64>,
    pub quality_id: Option<i64>,
    pub parsed: Option<parser::ParsedRelease>,
    #[serde(default)]
    pub custom_formats: Option<crate::custom_formats::Score>,
}
impl ReleaseDecision {
    fn reject(code: &str) -> Self {
        Self {
            target: None,
            disposition: Disposition::Reject,
            reasons: vec![code.into()],
            not_before: None,
            quality_id: None,
            parsed: None,
            custom_formats: None,
        }
    }
    pub(crate) fn deny(&mut self, code: &str) {
        self.disposition = Disposition::Reject;
        self.reasons.push(code.into());
    }
}
#[derive(Debug)]
pub struct SearchError(pub &'static str);
impl IntoResponse for SearchError {
    fn into_response(self) -> Response {
        eprintln!("event=release_request_error code={}", self.0);
        (
            match self.0 {
                "release_storage_error" | "release_profile_error" => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                "release_search_timeout" | "release_policy_timeout" => StatusCode::GATEWAY_TIMEOUT,
                "release_target_missing" => StatusCode::NOT_FOUND,
                code if code.starts_with("provider_") || code.starts_with("refresh_") => {
                    StatusCode::BAD_GATEWAY
                }
                _ => StatusCode::BAD_REQUEST,
            },
            Json(crate::api::ApiErrorEnvelope::new(
                self.0,
                "Release operation could not be completed",
            )),
        )
            .into_response()
    }
}
impl From<libsql::Error> for SearchError {
    fn from(_: libsql::Error) -> Self {
        Self("release_storage_error")
    }
}
pub type Result<T> = std::result::Result<T, SearchError>;
pub fn domain(media: MediaDomain) -> &'static str {
    match media {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ReleasePolicy {
    pub torrent_delay_minutes: u32,
    pub usenet_delay_minutes: u32,
    pub availability_delay_days: i32,
}
pub(crate) fn timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc().timestamp())
        })
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|t| t.and_utc().timestamp())
        })
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSearchInput {
    pub provider_id: uuid::Uuid,
    pub provider_revision: i64,
    pub target: crate::db::MediaTarget,
    pub offset: u32,
    pub query_index: u32,
    pub limit: u32,
}
#[derive(Serialize, ts_rs::TS)]
pub struct EvaluatedRelease {
    pub metadata: crate::providers::indexer::ReleaseMetadata,
    pub decision: ReleaseDecision,
}
#[derive(Serialize, ts_rs::TS)]
pub struct ReleaseSearchPage {
    pub items: Vec<EvaluatedRelease>,
    pub next_query: Option<crate::providers::indexer::IndexerContinuation>,
}
#[derive(Clone)]
struct SearchState {
    db: Arc<Database>,
    client: RefreshClient,
}
pub fn router(db: Arc<Database>, client: RefreshClient) -> Router {
    let commands = crate::commands::search::router(db.clone(), client.clone());
    Router::new()
        .route("/api/v1/release-search", post(search))
        .route(
            "/api/v1/release-policies/{media_type}",
            get(get_policy).put(put_policy),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(SearchState { db, client })
        .merge(commands)
}
fn media(value: &str) -> Result<MediaDomain> {
    match value {
        "tv" => Ok(MediaDomain::Tv),
        "movies" => Ok(MediaDomain::Movies),
        _ => Err(SearchError("invalid_media_type")),
    }
}
async fn get_policy(
    State(s): State<SearchState>,
    Path(value): Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::BTreeMap<String, String>>,
) -> Result<Json<Option<ReleasePolicy>>> {
    if !query.is_empty() {
        return Err(SearchError("invalid_query"));
    }
    let m = media(&value)?;
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
    let c =
        s.db.connect()
            .await
            .map_err(|_| SearchError("release_storage_error"))?;
    let row=c.query("SELECT torrent_delay_minutes,usenet_delay_minutes,availability_delay_days FROM release_delay_policies WHERE media_type=?",[domain(m)]).await?.next().await?;
    Ok(Json(
        row.map(|r| {
            Ok::<_, SearchError>(ReleasePolicy {
                torrent_delay_minutes: r.get::<i64>(0)? as u32,
                usenet_delay_minutes: r.get::<i64>(1)? as u32,
                availability_delay_days: r.get::<i64>(2)? as i32,
            })
        })
        .transpose()?,
    ))
    }).await.map_err(|_|SearchError("release_policy_timeout"))?
}
async fn put_policy(
    State(s): State<SearchState>,
    Path(value): Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::BTreeMap<String, String>>,
    body: std::result::Result<Json<ReleasePolicy>, JsonRejection>,
) -> Result<Json<ReleasePolicy>> {
    if !query.is_empty() {
        return Err(SearchError("invalid_query"));
    }
    let m = media(&value)?;
    let Json(p) = body.map_err(|_| SearchError("invalid_release_policy"))?;
    if p.torrent_delay_minutes > 10080
        || p.usenet_delay_minutes > 10080
        || !(-365..=365).contains(&p.availability_delay_days)
        || (matches!(m, MediaDomain::Tv) && p.availability_delay_days != 0)
    {
        return Err(SearchError("invalid_release_policy"));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let c =
            s.db.connect()
                .await
                .map_err(|_| SearchError("release_storage_error"))?;
        let tx = c
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await?;
        match crate::delay_profiles::write_legacy(&tx, m, &p).await {
            Ok(()) => tx.commit().await?,
            Err(_) => {
                tx.rollback().await?;
                return Err(SearchError("release_policy_update_failed"));
            }
        }
        Ok(Json(p))
    })
    .await
    .map_err(|_| SearchError("release_policy_timeout"))?
}
async fn search(
    State(s): State<SearchState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::BTreeMap<String, String>>,
    body: std::result::Result<Json<ReleaseSearchInput>, JsonRejection>,
) -> Result<Json<ReleaseSearchPage>> {
    if !query.is_empty() {
        return Err(SearchError("invalid_query"));
    }
    let Json(input) = body.map_err(|_| SearchError("invalid_release_search"))?;
    let target_id = match input.target {
        crate::db::MediaTarget::Episode(id) | crate::db::MediaTarget::Movie(id) => id,
    };
    if !(1..=9_007_199_254_740_991).contains(&target_id)
        || input.provider_revision < 1
        || input.limit == 0
        || input.limit > 100
        || input.offset > 10000
        || input.query_index > 128
    {
        return Err(SearchError("invalid_release_search"));
    }
    tokio::time::timeout(std::time::Duration::from_secs(40), async {
        let c =
            s.db.connect()
                .await
                .map_err(|_| SearchError("release_storage_error"))?;
        let (request, media) = target_request(
            &c,
            &input.target,
            input.offset,
            input.query_index,
            input.limit,
        )
        .await?;
        let page = s
            .client
            .raw_search(
                &input.provider_id.to_string(),
                input.provider_revision,
                &request,
            )
            .await
            .map_err(|e| SearchError(e.code))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| SearchError("clock_error"))?
            .as_secs() as i64;
        let mut items = Vec::new();
        for release in page.items {
            let mut decision =
                evaluate(&c, media, &release, SearchContext::UserSearch, now).await?;
            let matches = match (&input.target, &decision.target) {
                (
                    crate::db::MediaTarget::Episode(id),
                    Some(ReleaseTarget::Tv { episode_ids, .. }),
                ) => episode_ids.contains(id),
                (crate::db::MediaTarget::Movie(id), Some(ReleaseTarget::Movies { movie_id })) => {
                    id == movie_id
                }
                _ => false,
            };
            if !matches {
                decision.deny("requested_target_mismatch")
            }
            items.push(EvaluatedRelease {
                metadata: release.metadata,
                decision,
            });
        }
        let result = ReleaseSearchPage {
            items,
            next_query: page.next_query,
        };
        if serde_json::to_vec(&result)
            .map_err(|_| SearchError("release_storage_error"))?
            .len()
            > 1024 * 1024
        {
            return Err(SearchError("release_result_limit"));
        }
        Ok(Json(result))
    })
    .await
    .map_err(|_| SearchError("release_search_timeout"))?
}

pub(crate) async fn target_request(
    c: &Connection,
    target: &crate::db::MediaTarget,
    offset: u32,
    query_index: u32,
    limit: u32,
) -> Result<(IndexerSearch, MediaDomain)> {
    Ok(match target {
        crate::db::MediaTarget::Episode(id) => {
            let r=c.query("SELECT s.title,s.tvdb_id,e.season,e.number,l.series_type,e.air_date,e.absolute_episode_number,l.use_scene_numbering FROM episodes e JOIN series s ON s.id=e.series_id LEFT JOIN library_settings l ON l.series_id=s.id WHERE e.id=?",[*id]).await?.next().await?.ok_or(SearchError("release_target_missing"))?;
            if r.get::<Option<i64>>(7)? == Some(1) {
                return Err(SearchError("scene_search_unsupported"));
            }
            let standard = TvNumbering::Episode {
                season: u32::try_from(r.get::<i64>(2)?)
                    .map_err(|_| SearchError("invalid_episode_number"))?,
                episode: u32::try_from(r.get::<i64>(3)?)
                    .map_err(|_| SearchError("invalid_episode_number"))?,
            };
            let numbering = match r.get::<Option<String>>(4)?.as_deref() {
                Some("standard") => standard,
                Some("daily") => TvNumbering::Daily {
                    date: r
                        .get::<Option<String>>(5)?
                        .ok_or(SearchError("daily_date_unknown"))?,
                },
                Some("anime") => TvNumbering::Anime {
                    absolute_episode: u32::try_from(
                        r.get::<Option<i64>>(6)?
                            .ok_or(SearchError("absolute_number_unknown"))?,
                    )
                    .map_err(|_| SearchError("invalid_episode_number"))?,
                    season: None,
                    episode: None,
                },
                _ => return Err(SearchError("series_type_unconfigured")),
            };
            (
                IndexerSearch::Tv {
                    title: r.get(0)?,
                    aliases: vec![],
                    tvdb_id: r.get::<Option<i64>>(1)?.and_then(|v| u32::try_from(v).ok()),
                    tvmaze_id: None,
                    rage_id: None,
                    imdb_id: None,
                    tmdb_id: None,
                    numbering,
                    search_mode: TvSearchMode::Default,
                    offset: offset,
                    query_index: query_index,
                    limit: limit,
                },
                MediaDomain::Tv,
            )
        }
        crate::db::MediaTarget::Movie(id) => {
            let r=c.query("SELECT d.title,d.year,d.tmdb_id,d.imdb_id,d.id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",[*id]).await?.next().await?.ok_or(SearchError("release_target_missing"))?;
            let mut aliases = Vec::new();
            let mut rows=c.query("SELECT title FROM movie_alternative_titles WHERE metadata_id=? ORDER BY title LIMIT 65",[r.get::<i64>(4)?]).await?;
            while let Some(a) = rows.next().await? {
                aliases.push(a.get(0)?)
            }
            if aliases.len() > 64 {
                return Err(SearchError("movie_alias_limit"));
            }
            (
                IndexerSearch::Movie {
                    title: r.get(0)?,
                    aliases,
                    year: r.get::<Option<i64>>(1)?.and_then(|v| u16::try_from(v).ok()),
                    tmdb_id: r.get::<Option<i64>>(2)?.and_then(|v| u32::try_from(v).ok()),
                    imdb_id: r.get(3)?,
                    offset: offset,
                    query_index: query_index,
                    limit: limit,
                },
                MediaDomain::Movies,
            )
        }
    })
}
