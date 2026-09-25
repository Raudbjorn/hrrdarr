//! Read-only, bounded catalog lookup. Selected adds fetch details again before opening a DB transaction.
use crate::providers::http::{HttpClient, HttpError, HttpRequestBody};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;
const TV_ORIGIN: &str = "https://skyhook.sonarr.tv/v1/tvdb/";
const MOVIE_ORIGIN: &str = "https://api.radarr.video/v1/";
const MAX_RESULTS: usize = 100;
const MAX_EPISODES: usize = 10000;
#[derive(Debug)]
pub enum MetadataError {
    InvalidInput,
    NotFound,
    Unavailable,
    Busy,
    RateLimited(Option<u32>),
    InvalidResponse,
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Metadata operation failed")
    }
}
impl std::error::Error for MetadataError {}
impl IntoResponse for MetadataError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::InvalidInput => (
                StatusCode::BAD_REQUEST,
                "invalid_metadata_query",
                "Use a title or a valid scoped catalog identifier",
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "metadata_not_found",
                "Catalog record was not found",
            ),
            Self::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "metadata_busy",
                "Metadata service is busy; retry later",
            ),
            Self::RateLimited(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "metadata_rate_limited",
                "Metadata service rate limit reached",
            ),
            Self::InvalidResponse => (
                StatusCode::BAD_GATEWAY,
                "invalid_metadata_response",
                "Metadata response is invalid or exceeds supported bounds",
            ),
            Self::Unavailable => (
                StatusCode::BAD_GATEWAY,
                "metadata_unavailable",
                "Metadata service request failed",
            ),
        };
        let mut response = (
            status,
            Json(crate::api::ApiErrorEnvelope::new(code, message)),
        )
            .into_response();
        if let Self::RateLimited(Some(seconds)) = self {
            if let Ok(value) = seconds.to_string().parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}
impl From<HttpError> for MetadataError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Busy => Self::Busy,
            HttpError::RateLimited {
                retry_after_seconds,
            } => Self::RateLimited(retry_after_seconds),
            HttpError::ResponseTooLarge | HttpError::InvalidResponse => Self::InvalidResponse,
            _ => Self::Unavailable,
        }
    }
}
type Result<T> = std::result::Result<T, MetadataError>;
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct LookupResult {
    pub media_type: crate::api::MediaDomain,
    pub external_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct SeriesDetails {
    pub tvdb_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
    pub seasons: Vec<i64>,
    pub episodes: Vec<EpisodeDetails>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct EpisodeDetails {
    pub tvdb_id: i64,
    pub season: i64,
    pub number: i64,
    pub title: String,
    pub air_date: Option<String>,
    pub air_date_utc: Option<String>,
    pub absolute_episode_number: Option<i64>,
    pub runtime: Option<i64>,
    pub overview: Option<String>,
    pub finale_type: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct MovieDetails {
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
}
pub struct MetadataClient {
    http: HttpClient,
    tv: String,
    movies: String,
}
impl MetadataClient {
    pub fn new() -> Result<Self> {
        Self::with_origins(TV_ORIGIN, MOVIE_ORIGIN)
    }
    /// Injection is limited to the fixed service origins or a local fixture server.
    pub fn with_origins(tv: &str, movies: &str) -> Result<Self> {
        fn origin(value: &str, production: &str) -> Result<String> {
            if value == production {
                return Ok(value.into());
            }
            let url = url::Url::parse(value).map_err(|_| MetadataError::InvalidInput)?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.port().is_none_or(|port| port == 0)
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
                || value != url.as_str()
            {
                return Err(MetadataError::InvalidInput);
            }
            Ok(value.into())
        }
        Ok(Self {
            http: HttpClient::new()?,
            tv: origin(tv, TV_ORIGIN)?,
            movies: origin(movies, MOVIE_ORIGIN)?,
        })
    }
    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        tv: bool,
        path: &str,
        query: &[(String, String)],
    ) -> Result<T> {
        // One stable lane per service preserves cooldowns across requests without unbounded identities.
        let operation = self
            .http
            .operation(Uuid::from_u128(if tv { 1 } else { 2 }))?;
        let response = operation
            .request(
                &format!("{}{path}", if tv { &self.tv } else { &self.movies }),
                query,
                &[],
                HttpRequestBody::Empty,
            )
            .await?;
        match response.status {
            404 => return Err(MetadataError::NotFound),
            200 => (),
            _ => return Err(MetadataError::Unavailable),
        }
        serde_json::from_slice(&response.body).map_err(|_| MetadataError::InvalidResponse)
    }
    pub async fn series(&self, id: i64) -> Result<SeriesDetails> {
        input_id(id)?;
        let wire: Show = self.fetch(true, &format!("shows/en/{id}"), &[]).await?;
        if wire.tvdb_id != id {
            return Err(MetadataError::InvalidResponse);
        }
        wire.details()
    }
    pub async fn movie(&self, id: i64) -> Result<MovieDetails> {
        input_id(id)?;
        let wire: Movie = self.fetch(false, &format!("movie/{id}"), &[]).await?;
        if wire.tmdb_id != id {
            return Err(MetadataError::InvalidResponse);
        }
        wire.details()
    }
    pub async fn lookup_tv(&self, term: &str) -> Result<Vec<LookupResult>> {
        let term = query(term)?;
        if let Some((prefix, id)) = term.split_once(':') {
            if matches!(prefix.to_ascii_lowercase().as_str(), "tvdb" | "tvdbid") {
                let id = parse_id(id)?;
                return match self.series(id).await {
                    Ok(show) => Ok(vec![LookupResult {
                        media_type: crate::api::MediaDomain::Tv,
                        external_id: show.tvdb_id,
                        title: show.title,
                        year: show.year,
                        imdb_id: show.imdb_id,
                    }]),
                    Err(MetadataError::NotFound) => Ok(vec![]),
                    Err(error) => Err(error),
                };
            }
            if matches!(
                prefix.to_ascii_lowercase().as_str(),
                "tmdb" | "tmdbid" | "imdb" | "imdbid"
            ) {
                return Err(MetadataError::InvalidInput);
            }
        }
        let results: Vec<Show> = match self
            .fetch(true, "search/en", &[("term".into(), term.into())])
            .await
        {
            Ok(results) => results,
            Err(MetadataError::NotFound) => return Ok(vec![]),
            Err(error) => return Err(error),
        };
        validate_results(
            results
                .into_iter()
                .map(Show::summary)
                .collect::<Result<Vec<_>>>()?,
        )
    }
    pub async fn lookup_movie(&self, term: &str) -> Result<Vec<LookupResult>> {
        let term = query(term)?;
        if let Some((prefix, id)) = term.split_once(':') {
            match prefix.to_ascii_lowercase().as_str() {
                "tmdb" | "tmdbid" => {
                    return match self.movie(parse_id(id)?).await {
                        Ok(movie) => Ok(vec![movie.summary()]),
                        Err(MetadataError::NotFound) => Ok(vec![]),
                        Err(error) => Err(error),
                    };
                }
                "imdb" | "imdbid" => {
                    let id = id.trim();
                    if !valid_imdb(id) {
                        return Err(MetadataError::InvalidInput);
                    }
                    let result: Result<Movie> =
                        self.fetch(false, &format!("movie/imdb/{id}"), &[]).await;
                    return match result {
                        Ok(movie) => {
                            let movie = movie.details()?;
                            if movie.imdb_id.as_deref() != Some(id) {
                                return Err(MetadataError::InvalidResponse);
                            }
                            Ok(vec![movie.summary()])
                        }
                        Err(MetadataError::NotFound) => Ok(vec![]),
                        Err(error) => Err(error),
                    };
                }
                "tvdb" | "tvdbid" => return Err(MetadataError::InvalidInput),
                _ => (),
            }
        }
        let results: Vec<Movie> = match self
            .fetch(false, "search", &[("q".into(), term.into())])
            .await
        {
            Ok(results) => results,
            Err(MetadataError::NotFound) => return Ok(vec![]),
            Err(error) => return Err(error),
        };
        validate_results(
            results
                .into_iter()
                .map(|movie| movie.details().map(|movie| movie.summary()))
                .collect::<Result<Vec<_>>>()?,
        )
    }
}
fn query(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(MetadataError::InvalidInput);
    }
    Ok(value)
}
fn input_id(id: i64) -> Result<()> {
    if !(1..=i32::MAX as i64).contains(&id) {
        Err(MetadataError::InvalidInput)
    } else {
        Ok(())
    }
}
fn parse_id(value: &str) -> Result<i64> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(MetadataError::InvalidInput);
    }
    let id = value.parse().map_err(|_| MetadataError::InvalidInput)?;
    input_id(id)?;
    Ok(id)
}
fn identity(id: i64) -> Result<()> {
    input_id(id).map_err(|_| MetadataError::InvalidResponse)
}
fn text(value: &str, max: usize, multiline: bool) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(())
}
fn valid_imdb(value: &str) -> bool {
    value.strip_prefix("tt").is_some_and(|id| {
        (7..=10).contains(&id.len())
            && id.bytes().all(|b| b.is_ascii_digit())
            && id.bytes().any(|b| b != b'0')
    })
}
fn imdb(value: Option<String>) -> Result<Option<String>> {
    match value {
        Some(value) if value.is_empty() => Ok(None),
        Some(value) if valid_imdb(&value) => Ok(Some(value)),
        None => Ok(None),
        _ => Err(MetadataError::InvalidResponse),
    }
}
fn date(value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| {
            if value.len() != 10
                || value.starts_with("0000")
                || chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d").is_err()
            {
                return Err(MetadataError::InvalidResponse);
            }
            Ok(value)
        })
        .transpose()
}
fn timestamp(value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| {
            if value.len() > 40
                || value.split_once('.').is_some_and(|(_, fraction)| {
                    fraction.bytes().take_while(u8::is_ascii_digit).count() > 9
                })
            {
                return Err(MetadataError::InvalidResponse);
            }
            let parsed = chrono::DateTime::parse_from_rfc3339(&value)
                .map_err(|_| MetadataError::InvalidResponse)?;
            crate::history::canonical_timestamp(&parsed.to_utc())
                .ok_or(MetadataError::InvalidResponse)
        })
        .transpose()
}
fn validate_results(results: Vec<LookupResult>) -> Result<Vec<LookupResult>> {
    let mut ids = BTreeSet::new();
    if results.len() > MAX_RESULTS || results.iter().any(|result| !ids.insert(result.external_id)) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(results)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Show {
    tvdb_id: i64,
    title: String,
    first_aired: Option<String>,
    imdb_id: Option<String>,
    seasons: Option<Vec<Season>>,
    episodes: Option<Vec<Episode>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Season {
    season_number: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Episode {
    tvdb_id: i64,
    season_number: i64,
    episode_number: i64,
    title: String,
    air_date: Option<String>,
    air_date_utc: Option<String>,
    absolute_episode_number: Option<i64>,
    runtime: Option<i64>,
    overview: Option<String>,
    finale_type: Option<String>,
}
impl Show {
    fn summary(self) -> Result<LookupResult> {
        identity(self.tvdb_id)?;
        text(&self.title, 1024, false)?;
        let first = date(self.first_aired)?;
        let year = first
            .as_deref()
            .map(|value| {
                value[..4]
                    .parse::<i64>()
                    .map_err(|_| MetadataError::InvalidResponse)
            })
            .transpose()?;
        Ok(LookupResult {
            media_type: crate::api::MediaDomain::Tv,
            external_id: self.tvdb_id,
            title: self.title,
            year,
            imdb_id: imdb(self.imdb_id)?,
        })
    }
    fn details(mut self) -> Result<SeriesDetails> {
        let seasons = self.seasons.take().ok_or(MetadataError::InvalidResponse)?;
        let episodes = self.episodes.take().ok_or(MetadataError::InvalidResponse)?;
        if seasons.len() > 1000 || episodes.len() > MAX_EPISODES {
            return Err(MetadataError::InvalidResponse);
        }
        let mut season_ids = BTreeSet::new();
        for season in seasons {
            if !(0..=i32::MAX as i64).contains(&season.season_number)
                || !season_ids.insert(season.season_number)
            {
                return Err(MetadataError::InvalidResponse);
            }
        }
        let mut ids = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        let mut details = Vec::with_capacity(episodes.len());
        for episode in episodes {
            identity(episode.tvdb_id)?;
            text(&episode.title, 1024, false)?;
            if !season_ids.contains(&episode.season_number)
                || !(0..=i32::MAX as i64).contains(&episode.episode_number)
                || !ids.insert(episode.tvdb_id)
                || !numbers.insert((episode.season_number, episode.episode_number))
                || episode
                    .absolute_episode_number
                    .is_some_and(|value| !(0..=i32::MAX as i64).contains(&value))
                || episode
                    .runtime
                    .is_some_and(|value| !(0..=i32::MAX as i64).contains(&value))
            {
                return Err(MetadataError::InvalidResponse);
            }
            if let Some(overview) = &episode.overview {
                if !overview.is_empty() {
                    text(overview, 32768, true)?;
                }
            }
            if let Some(finale) = &episode.finale_type {
                if !finale.is_empty() {
                    text(finale, 128, false)?;
                }
            }
            details.push(EpisodeDetails {
                tvdb_id: episode.tvdb_id,
                season: episode.season_number,
                number: episode.episode_number,
                title: episode.title,
                air_date: date(episode.air_date)?,
                air_date_utc: timestamp(episode.air_date_utc)?,
                absolute_episode_number: episode.absolute_episode_number,
                runtime: episode.runtime,
                overview: episode.overview,
                finale_type: episode.finale_type,
            });
        }
        let summary = self.summary()?;
        Ok(SeriesDetails {
            tvdb_id: summary.external_id,
            title: summary.title,
            year: summary.year,
            imdb_id: summary.imdb_id,
            seasons: season_ids.into_iter().collect(),
            episodes: details,
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    tmdb_id: i64,
    title: String,
    year: Option<i64>,
    imdb_id: Option<String>,
}
impl Movie {
    fn details(self) -> Result<MovieDetails> {
        identity(self.tmdb_id)?;
        text(&self.title, 1024, false)?;
        let year = self.year.filter(|year| *year != 0);
        if year.is_some_and(|year| !(1..=9999).contains(&year)) {
            return Err(MetadataError::InvalidResponse);
        }
        Ok(MovieDetails {
            tmdb_id: self.tmdb_id,
            title: self.title,
            year,
            imdb_id: imdb(self.imdb_id)?,
        })
    }
}
impl MovieDetails {
    fn summary(self) -> LookupResult {
        LookupResult {
            media_type: crate::api::MediaDomain::Movies,
            external_id: self.tmdb_id,
            title: self.title,
            year: self.year,
            imdb_id: self.imdb_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::{Path, Query},
        routing::get,
    };
    use serde_json::json;
    use std::collections::HashMap;

    #[tokio::test]
    async fn catalog_transport_validation_and_bounds() {
        async fn show(Path(id): Path<i64>) -> Response {
            match id {
                404 => StatusCode::NOT_FOUND.into_response(),
                302 => (StatusCode::FOUND, [("location", "http://private.invalid/")]).into_response(),
                429 => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "60")], "PRIVATE_BODY").into_response(),
                500 => (StatusCode::INTERNAL_SERVER_ERROR, "PRIVATE_BODY").into_response(),
                1000 => "x".repeat(crate::providers::http::MAX_BODY_BYTES + 1).into_response(),
                _ => Json(json!({"tvdbId": id, "title":"Catalog TV", "firstAired":"2024-02-29", "seasons":[{"seasonNumber":0}],"episodes":[{"tvdbId":20,"seasonNumber":0,"episodeNumber":1,"title":"Special","airDateUtc":"2024-02-29T21:00:00.123400+01:00"}]})).into_response(),
            }
        }
        let router = Router::new()
            .route("/shows/en/{id}", get(show))
            .route(
                "/search/en",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    if query.get("term").unwrap() == "absent" {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    assert_eq!(query.get("term").unwrap(), "A & B + C: Test");
                    Json(json!([{"tvdbId":1,"title":"Catalog TV"}])).into_response()
                }),
            )
            .route(
                "/search",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    if query.get("q").unwrap() == "absent" {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    assert_eq!(query.get("q").unwrap(), "A & B + C: Test");
                    Json(json!([{"tmdbId":1,"title":"Catalog Movie","year":0}])).into_response()
                }),
            )
            .route(
                "/movie/{id}",
                get(|Path(id): Path<i64>| async move {
                    Json(json!({"tmdbId":id,"title":"Movie","imdbId":"tt1234567"}))
                }),
            )
            .route(
                "/movie/imdb/{id}",
                get(|Path(id): Path<String>| async move {
                    Json(json!({"tmdbId":1,"title":"Movie","imdbId":id}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = MetadataClient::with_origins(&origin, &origin).unwrap();
        assert_eq!(
            client.lookup_tv("A & B + C: Test").await.unwrap()[0].year,
            None
        );
        assert_eq!(
            client.lookup_movie("A & B + C: Test").await.unwrap()[0].year,
            None
        );
        assert!(client.lookup_tv("absent").await.unwrap().is_empty());
        assert!(client.lookup_movie("absent").await.unwrap().is_empty());
        assert_eq!(
            client.lookup_tv("tvdb:1").await.unwrap()[0].media_type,
            crate::api::MediaDomain::Tv
        );
        assert_eq!(
            client.lookup_movie("tmdb:1").await.unwrap()[0].media_type,
            crate::api::MediaDomain::Movies
        );
        let series = client.series(1).await.unwrap();
        assert_eq!(series.year, Some(2024));
        assert_eq!(
            series.episodes[0].air_date_utc.as_deref(),
            Some("2024-02-29 20:00:00.1234")
        );
        assert_eq!(
            client.lookup_tv("tvdbid: 1 ").await.unwrap()[0].external_id,
            1
        );
        assert_eq!(
            client.lookup_movie("tmdb:1").await.unwrap()[0].external_id,
            1
        );
        assert_eq!(
            client.lookup_movie("imdb:tt1234567").await.unwrap()[0].external_id,
            1
        );
        for term in [
            "tvdb:0",
            "tvdb:../1",
            "tvdb:-1",
            "tmdb:1",
            "imdb:tt1234567",
            "",
        ] {
            assert!(matches!(
                client.lookup_tv(term).await,
                Err(MetadataError::InvalidInput)
            ));
        }
        assert!(client.lookup_tv("tvdb:404").await.unwrap().is_empty());
        assert!(matches!(
            client.series(404).await,
            Err(MetadataError::NotFound)
        ));
        assert!(matches!(
            client.series(302).await,
            Err(MetadataError::Unavailable)
        ));
        assert!(matches!(
            client.series(500).await,
            Err(MetadataError::Unavailable)
        ));
        assert!(matches!(
            client.series(1000).await,
            Err(MetadataError::InvalidResponse)
        ));
        assert!(matches!(
            client.series(429).await,
            Err(MetadataError::RateLimited(Some(60)))
        ));
        assert!(matches!(
            client.series(1).await,
            Err(MetadataError::RateLimited(_))
        ));
        assert!(
            client.movie(1).await.is_ok(),
            "Movie lane is independent of TV cooldown"
        );
        let response = MetadataError::Unavailable.into_response();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("PRIVATE"));
        for origin in [
            "http://127.0.0.1:0/",
            "http://127.0.0.1:1/path",
            "http://127.0.0.1:1/?q=x",
            "http://user@127.0.0.1:1/",
            "http://localhost:123/",
            "https://private.invalid/",
        ] {
            assert!(MetadataClient::with_origins(origin, MOVIE_ORIGIN).is_err());
        }
        assert!(timestamp(Some("2024-01-01T00:00:00.1234567891Z".into())).is_err());
        assert!(date(Some("2023-02-29".into())).is_err());
        assert!(date(Some("0000-01-01".into())).is_err());
        assert!(
            validate_results(
                (0..101)
                    .map(|id| LookupResult {
                        media_type: crate::api::MediaDomain::Tv,
                        external_id: id,
                        title: "x".into(),
                        year: None,
                        imdb_id: None
                    })
                    .collect()
            )
            .is_err()
        );
        server.abort();
        let _ = server.await;
    }
}
