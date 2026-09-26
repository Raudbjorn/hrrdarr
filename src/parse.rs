//! Read-only "how would this release title parse and match" diagnostic. No writes, ever.
//!
//! Requirements shape only, read for a single `GET` action per domain from
//! `Sonarr.Api.V3/Parse/ParseController.cs` and `Radarr.Api.V3/Parse/ParseController.cs`
//! (both under `.do-not-commit/`, git-ignored, not committed). This module is an
//! independent implementation: it calls the same `crate::search::parser::parse`/`normalize`
//! primitives the real decision engine (`src/search/decision.rs`) and downloaded-file
//! matcher (`src/search/downloaded.rs`) already use, but its own matching/disambiguation
//! logic and response DTOs are original, not a port. See docs/parse-api.md for the
//! deliberate simplifications versus `decision.rs`.
use crate::db::Database;
use crate::search::parser::{self, Numbering, ParsedRelease};
use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const SCAN_LIMIT: i64 = 10_000;
const EPISODE_LIMIT: i64 = 1_000;

#[derive(Debug)]
struct Error(StatusCode, &'static str, &'static str);
type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(crate::api::ApiErrorEnvelope::new(self.1, self.2)),
        )
            .into_response()
    }
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database_error",
            "Parse diagnostic lookup failed",
        )
    }
}
fn bad(message: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn scan_limit() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "library_match_limit",
        "Too many library rows to scan for a diagnostic match",
    )
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ParseQuery {
    pub title: String,
}
fn title_param(query: std::result::Result<Query<ParseQuery>, QueryRejection>) -> Result<String> {
    let title = query
        .map_err(|_| bad("Query must include a title parameter"))?
        .0
        .title;
    if title.is_empty() {
        return Err(bad("title must not be empty"));
    }
    Ok(title)
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MatchedSeries {
    pub id: i64,
    pub title: String,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MatchedEpisode {
    pub id: i64,
    pub season: i64,
    pub number: i64,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TvParseResult {
    pub title: String,
    pub parsed: Option<ParsedRelease>,
    pub series: Option<MatchedSeries>,
    pub episodes: Vec<MatchedEpisode>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MatchedMovie {
    pub id: i64,
    pub title: String,
    pub year: Option<i64>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MovieParseResult {
    pub title: String,
    pub parsed: Option<ParsedRelease>,
    pub movie: Option<MatchedMovie>,
}

/// Full-scan normalized-title match, same bounded-scan idiom as `search::decision`'s
/// series match. Returns `None`, never an arbitrary pick, when zero or more than one
/// series shares the normalized title (no alias/alternate-title data exists to
/// disambiguate further; see docs/parse-api.md).
async fn match_series(conn: &Connection, normalized_title: &str) -> Result<Option<MatchedSeries>> {
    let mut rows = conn
        .query("SELECT id,title FROM series ORDER BY id LIMIT 10001", ())
        .await?;
    let mut matches = Vec::new();
    let mut scanned = 0i64;
    while let Some(row) = rows.next().await? {
        scanned += 1;
        if scanned > SCAN_LIMIT {
            return Err(scan_limit());
        }
        let title: String = row.get(1)?;
        if parser::normalize(&title) == normalized_title {
            matches.push(MatchedSeries {
                id: row.get(0)?,
                title,
            });
        }
    }
    Ok(if matches.len() == 1 {
        matches.pop()
    } else {
        None
    })
}
/// Resolves `Episodes`/`Season` numbering against one series; `Daily`/`Absolute`
/// numbering is intentionally not resolved (documented scope limit, not a schema gap
/// -- see docs/parse-api.md).
async fn match_episodes(
    conn: &Connection,
    series_id: i64,
    numbering: &Numbering,
) -> Result<Vec<MatchedEpisode>> {
    let season = match numbering {
        Numbering::Episodes { season, .. } | Numbering::Season { season } => *season,
        Numbering::Daily { .. } | Numbering::Absolute { .. } => return Ok(Vec::new()),
    };
    let wanted: Option<&[i64]> = match numbering {
        Numbering::Episodes { episodes, .. } => Some(episodes.as_slice()),
        _ => None,
    };
    let mut rows = conn
        .query(
            "SELECT id,season,number FROM episodes WHERE series_id=? AND season=? ORDER BY number LIMIT 1001",
            params![series_id, season],
        )
        .await?;
    let mut result = Vec::new();
    let mut scanned = 0i64;
    while let Some(row) = rows.next().await? {
        scanned += 1;
        if scanned > EPISODE_LIMIT {
            return Err(scan_limit());
        }
        let number: i64 = row.get(2)?;
        if wanted.is_none_or(|numbers| numbers.contains(&number)) {
            result.push(MatchedEpisode {
                id: row.get(0)?,
                season: row.get(1)?,
                number,
            });
        }
    }
    Ok(result)
}
/// Full-scan normalized-title match against `movies`/`movie_metadata`, same bounded-scan
/// idiom as above. A single title match is returned regardless of year (a deliberate
/// simplification versus `search::decision`'s stricter `year`/`secondary_year` check on
/// every candidate, see docs/parse-api.md). When more than one title matches, `parsed.year`
/// (always present on a successful movie parse) disambiguates only if it selects exactly
/// one of them; otherwise the match stays ambiguous and `None` is returned rather than an
/// arbitrary pick.
async fn match_movie(conn: &Connection, parsed: &ParsedRelease) -> Result<Option<MatchedMovie>> {
    let normalized_title = parser::normalize(&parsed.title);
    let mut rows = conn
        .query(
            "SELECT m.id,d.title,d.year FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id ORDER BY m.id LIMIT 10001",
            (),
        )
        .await?;
    let mut matches = Vec::new();
    let mut scanned = 0i64;
    while let Some(row) = rows.next().await? {
        scanned += 1;
        if scanned > SCAN_LIMIT {
            return Err(scan_limit());
        }
        let title: String = row.get(1)?;
        if parser::normalize(&title) == normalized_title {
            matches.push(MatchedMovie {
                id: row.get(0)?,
                title,
                year: row.get(2)?,
            });
        }
    }
    Ok(match matches.len() {
        0 => None,
        1 => matches.pop(),
        _ => {
            let mut by_year: Vec<_> = matches
                .into_iter()
                .filter(|m| m.year == parsed.year)
                .collect();
            if by_year.len() == 1 {
                by_year.pop()
            } else {
                None
            }
        }
    })
}

#[derive(Clone)]
struct Context {
    db: Arc<Database>,
}
pub fn router(db: Arc<Database>) -> Router {
    Router::new()
        .route("/api/v1/tv/parse", get(parse_tv))
        .route("/api/v1/movies/parse", get(parse_movies))
        .with_state(Context { db })
}
async fn parse_tv(
    State(ctx): State<Context>,
    query: std::result::Result<Query<ParseQuery>, QueryRejection>,
) -> Result<Json<TvParseResult>> {
    let title = title_param(query)?;
    let parsed = parser::parse(&title, true).ok();
    let mut series = None;
    let mut episodes = Vec::new();
    if let Some(parsed) = &parsed {
        let conn = ctx.db.connect().await?;
        series = match_series(&conn, &parser::normalize(&parsed.title)).await?;
        if let (Some(matched), Some(numbering)) = (&series, &parsed.numbering) {
            episodes = match_episodes(&conn, matched.id, numbering).await?;
        }
    }
    Ok(Json(TvParseResult {
        title,
        parsed,
        series,
        episodes,
    }))
}
async fn parse_movies(
    State(ctx): State<Context>,
    query: std::result::Result<Query<ParseQuery>, QueryRejection>,
) -> Result<Json<MovieParseResult>> {
    let title = title_param(query)?;
    let parsed = parser::parse(&title, false).ok();
    let movie = if let Some(parsed) = &parsed {
        let conn = ctx.db.connect().await?;
        match_movie(&conn, parsed).await?
    } else {
        None
    };
    Ok(Json(MovieParseResult {
        title,
        parsed,
        movie,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query as Q;
    use std::path::PathBuf;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("hrrdarr-parse-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    async fn ctx() -> (Sandbox, Context) {
        let sandbox = Sandbox::new();
        let db = Arc::new(Database::open_local(sandbox.0.join("db")).await.unwrap());
        (sandbox, Context { db })
    }

    #[tokio::test]
    async fn tv_parse_matches_series_and_episode() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Harbor','/tv/harbor');INSERT INTO seasons(series_id,number) VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,2,'Two');").await.unwrap();
        let result = parse_tv(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.S01E02.1080p.WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.parsed.is_some());
        let series = result.series.unwrap();
        assert_eq!(series.id, 1);
        assert_eq!(series.title, "Harbor");
        assert_eq!(result.episodes.len(), 1);
        assert_eq!(result.episodes[0].id, 1);
        assert_eq!(result.episodes[0].season, 1);
        assert_eq!(result.episodes[0].number, 2);
    }

    #[tokio::test]
    async fn tv_parse_succeeds_with_no_library_match() {
        let (_s, ctx) = ctx().await;
        let result = parse_tv(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.S01E02.1080p.WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.parsed.is_some());
        assert!(result.series.is_none());
        assert!(result.episodes.is_empty());
    }

    #[tokio::test]
    async fn tv_parse_ambiguous_series_title_resolves_to_no_match() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch(
            "INSERT INTO series(id,title,path) VALUES(1,'Harbor','/tv/a'),(2,'Harbor','/tv/b');",
        )
        .await
        .unwrap();
        let result = parse_tv(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.S01E02.1080p.WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.series.is_none(), "must not pick either arbitrarily");
        assert!(result.episodes.is_empty());
    }

    #[tokio::test]
    async fn tv_parse_daily_and_absolute_numbering_never_resolve_episodes() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch("INSERT INTO series(id,title,path) VALUES(1,'Harbor','/tv/harbor');INSERT INTO seasons(series_id,number) VALUES(1,1);INSERT INTO episodes(id,series_id,season,number,title) VALUES(1,1,1,23,'Twenty Three');").await.unwrap();
        for title in ["Harbor.2026.09.24.720p.HDTV", "Harbor - 023 1080p WEB-DL"] {
            let result = parse_tv(
                State(ctx.clone()),
                Ok(Q(ParseQuery {
                    title: title.into(),
                })),
            )
            .await
            .unwrap()
            .0;
            assert!(result.parsed.is_some(), "{title}");
            assert!(result.series.is_some(), "{title}");
            assert!(
                result.episodes.is_empty(),
                "{title}: daily/absolute numbering is out of scope for this diagnostic endpoint"
            );
        }
    }

    #[tokio::test]
    async fn tv_parse_failure_returns_title_only() {
        let (_s, ctx) = ctx().await;
        let result = parse_tv(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor 1080p WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result.title, "Harbor 1080p WEB-DL");
        assert!(result.parsed.is_none());
        assert!(result.series.is_none());
        assert!(result.episodes.is_empty());
    }

    #[tokio::test]
    async fn tv_empty_title_is_a_clean_400() {
        let (_s, ctx) = ctx().await;
        let err = parse_tv(
            State(ctx),
            Ok(Q(ParseQuery {
                title: String::new(),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn movie_parse_matches_by_title_and_year() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch("INSERT INTO movie_metadata(id,title,year) VALUES(1,'Harbor',1982);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/harbor');").await.unwrap();
        let result = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.1982.1080p.Bluray".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.parsed.is_some());
        let movie = result.movie.unwrap();
        assert_eq!(movie.id, 1);
        assert_eq!(movie.year, Some(1982));
    }

    #[tokio::test]
    async fn movie_parse_succeeds_with_no_library_match() {
        let (_s, ctx) = ctx().await;
        let result = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.1982.1080p.Bluray".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(result.parsed.is_some());
        assert!(result.movie.is_none());
    }

    #[tokio::test]
    async fn movie_year_disambiguates_same_title_candidates() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch("INSERT INTO movie_metadata(id,title,year) VALUES(1,'Harbor',1982),(2,'Harbor',2011);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/harbor-1982'),(2,2,'/movies/harbor-2011');").await.unwrap();
        let old = parse_movies(
            State(ctx.clone()),
            Ok(Q(ParseQuery {
                title: "Harbor.1982.1080p.Bluray".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(old.movie.unwrap().id, 1);
        let new = parse_movies(
            State(ctx.clone()),
            Ok(Q(ParseQuery {
                title: "Harbor.2011.Extended.2160p.WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(new.movie.unwrap().id, 2);
        // A parsed year matching neither candidate stays an unresolved (not arbitrary) match.
        let neither = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.1999.1080p.Bluray".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(neither.movie.is_none());
    }

    #[tokio::test]
    async fn movie_same_title_and_year_stays_ambiguous() {
        let (_s, ctx) = ctx().await;
        let c = ctx.db.connect().await.unwrap();
        c.execute_batch("INSERT INTO movie_metadata(id,title,year) VALUES(1,'Harbor',1982),(2,'Harbor',1982);INSERT INTO movies(id,metadata_id,path) VALUES(1,1,'/movies/harbor-a'),(2,2,'/movies/harbor-b');").await.unwrap();
        let result = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.1982.1080p.Bluray".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert!(
            result.movie.is_none(),
            "must not pick either duplicate arbitrarily"
        );
    }

    #[tokio::test]
    async fn movie_parse_failure_returns_title_only() {
        let (_s, ctx) = ctx().await;
        let result = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: "Harbor.S01E02.1080p.WEB-DL".into(),
            })),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result.title, "Harbor.S01E02.1080p.WEB-DL");
        assert!(result.parsed.is_none());
        assert!(result.movie.is_none());
    }

    #[tokio::test]
    async fn movie_empty_title_is_a_clean_400() {
        let (_s, ctx) = ctx().await;
        let err = parse_movies(
            State(ctx),
            Ok(Q(ParseQuery {
                title: String::new(),
            })),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_title_query_param_is_a_clean_400() {
        let (_s, ctx) = ctx().await;
        let rejection = Q::<ParseQuery>::try_from_uri(&"/x".parse().unwrap()).unwrap_err();
        let err = parse_tv(State(ctx), Err(rejection)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn hostile_oversized_and_control_character_titles_parse_cleanly_to_no_match() {
        let (_s, ctx) = ctx().await;
        for title in [
            "x".repeat(2000),
            format!("Harbor.S01E02.1080p.WEB-DL{}", '\u{0}'),
        ] {
            let result = parse_tv(
                State(ctx.clone()),
                Ok(Q(ParseQuery {
                    title: title.clone(),
                })),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(result.title, title);
            assert!(result.parsed.is_none(), "{title:?}");
            assert!(result.series.is_none());
            assert!(result.episodes.is_empty());
        }
    }
}
