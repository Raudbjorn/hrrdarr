//! Read-only, bounded catalog lookup. Selected adds fetch details again before opening a DB transaction.
use crate::providers::http::{HttpClient, HttpError, HttpRequestBody};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    pub network: Option<String>,
    pub original_country: Option<String>,
    pub status: Option<String>,
    pub genres: Option<Vec<String>>,
    pub original_language: Option<i64>,
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
    pub studio: Option<String>,
    pub genres: Option<Vec<String>>,
    pub keywords: Option<Vec<String>>,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
    pub runtime: Option<i64>,
    pub status: Option<String>,
    pub in_cinemas: Option<String>,
    pub digital_release: Option<String>,
    pub physical_release: Option<String>,
    pub secondary_year: Option<i64>,
    pub original_language: Option<i64>,
    // None is absent metadata; Some([]) is an explicitly empty title set.
    pub alternative_titles: Option<Vec<String>>,
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
/// Blank scalar facts are unknown; preserve supplied nonblank spelling.
pub(crate) fn canonical_scalar(value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| {
            if value.len() > 1024 || value.chars().any(char::is_control) {
                return Err(MetadataError::InvalidResponse);
            }
            Ok((!value.trim().is_empty()).then_some(value))
        })
        .transpose()
        .map(Option::flatten)
}
pub(crate) fn canonical_country(value: Option<String>) -> Result<Option<String>> {
    canonical_scalar(value)?
        .map(|value| {
            if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return Err(MetadataError::InvalidResponse);
            }
            Ok(value.to_ascii_uppercase())
        })
        .transpose()
}
/// Stable case-insensitive sets: sort lowercase keys and retain the smallest
/// original spelling for each key. Validate BEFORE deduplication (including bounds).
pub(crate) fn canonical_set(value: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    value
        .map(|values| {
            if values.len() > 128
                || serde_json::to_vec(&values)
                    .map_err(|_| MetadataError::InvalidResponse)?
                    .len()
                    > 65536
            {
                return Err(MetadataError::InvalidResponse);
            }
            let mut members = BTreeMap::<String, String>::new();
            for value in values {
                text(&value, 256, false)?;
                members
                    .entry(value.to_lowercase())
                    .and_modify(|prior| {
                        if value < *prior {
                            *prior = value.clone();
                        }
                    })
                    .or_insert(value);
            }
            Ok(members.into_values().collect())
        })
        .transpose()
}
fn canonical_datetime(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    let parsed = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
        .map_err(|_| MetadataError::InvalidResponse)?
        .and_utc();
    if crate::history::canonical_timestamp(&parsed).as_deref() != Some(value) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(parsed)
}
/// Derived at retrieval time, never from wire status or reconciliation's clock.
pub(crate) fn derive_movie_status(
    cinema: Option<&str>,
    digital: Option<&str>,
    physical: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<&'static str> {
    let cinema = cinema.map(canonical_datetime).transpose()?;
    let digital = digital.map(canonical_datetime).transpose()?;
    let physical = physical.map(canonical_datetime).transpose()?;
    if digital.is_some_and(|date| date <= now)
        || physical.is_some_and(|date| date <= now)
        || (digital.is_none()
            && physical.is_none()
            && cinema
                .is_some_and(|date| now.signed_duration_since(date) > chrono::Duration::days(90)))
    {
        Ok("released")
    } else if cinema.is_some_and(|date| date < now) {
        Ok("in_cinemas")
    } else {
        Ok("announced")
    }
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
    network: Option<String>,
    original_country: Option<String>,
    status: Option<String>,
    genres: Option<Vec<String>>,
    original_language: Option<String>,
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
        canonical_scalar(self.network.clone())?;
        canonical_scalar(self.status.clone())?;
        canonical_country(self.original_country.clone())?;
        canonical_set(self.genres.clone())?;
        tv_original_language(self.original_language.clone())?;
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
        // Wire timestamps normalize once; the complete DTO validator owns graph bounds.
        let season_ids = seasons
            .into_iter()
            .map(|season| season.season_number)
            .collect();
        let details = episodes
            .into_iter()
            .map(|episode| {
                Ok(EpisodeDetails {
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
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let original_language = tv_original_language(self.original_language.take())?;
        let network = canonical_scalar(self.network.take())?;
        let original_country = canonical_country(self.original_country.take())?;
        let status = canonical_scalar(self.status.take())?.map(|value| {
            match value.to_lowercase().as_str() {
                "ended" => "ended".into(),
                "upcoming" => "upcoming".into(),
                _ => "continuing".into(),
            }
        });
        let genres = canonical_set(self.genres.take())?;
        let summary = self.summary()?;
        SeriesDetails {
            network,
            original_country,
            status,
            genres,
            original_language,
            tvdb_id: summary.external_id,
            title: summary.title,
            year: summary.year,
            imdb_id: summary.imdb_id,
            seasons: season_ids,
            episodes: details,
        }
        .validated()
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    studio: Option<String>,
    genres: Option<Vec<String>>,
    keywords: Option<Vec<String>>,
    tmdb_id: i64,
    title: String,
    year: Option<i64>,
    imdb_id: Option<String>,
    runtime: Option<i64>,
    in_cinema: Option<String>,
    digital_release: Option<String>,
    physical_release: Option<String>,
    premier: Option<String>,
    original_language: Option<String>,
    alternative_titles: Option<Vec<AlternativeTitle>>,
}
#[derive(Deserialize)]
struct AlternativeTitle {
    title: String,
}
// SkyHook originalLanguage is an ISO code, not a display name or a movie language ID.
// IDs are the TV catalog; absence/unrecognized codes never imply English.
fn tv_original_language(value: Option<String>) -> Result<Option<i64>> {
    let Some(value) = value else { return Ok(None) };
    if value.trim().is_empty() {
        return Ok(None);
    }
    if value.len() > 32 || !value.bytes().all(|c| c.is_ascii_alphabetic() || c == b'-') {
        return Err(MetadataError::InvalidResponse);
    }
    let region = value.split_once('-').map(|(_, region)| region);
    if region.is_some_and(|r| r.len() != 2 || !r.bytes().all(|c| c.is_ascii_alphabetic())) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(crate::languages::iso_language_id(crate::api::MediaDomain::Tv, &value).map(i64::from))
}

fn original_language(value: Option<String>) -> Result<Option<i64>> {
    let Some(value) = value else { return Ok(None) };
    if !(2..=3).contains(&value.len()) || !value.bytes().all(|c| c.is_ascii_alphabetic()) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(crate::languages::iso_language_id(crate::api::MediaDomain::Movies, &value).map(i64::from))
}
impl Movie {
    fn details(self) -> Result<MovieDetails> {
        identity(self.tmdb_id)?;
        text(&self.title, 1024, false)?;
        let year = self.year.filter(|year| *year != 0);
        if year.is_some_and(|year| !(1..=9999).contains(&year)) {
            return Err(MetadataError::InvalidResponse);
        }
        let runtime = self.runtime;
        if runtime.is_some_and(|n| !(0..=10080).contains(&n)) {
            return Err(MetadataError::InvalidResponse);
        }
        let premier = timestamp(self.premier)?;
        let secondary_year = premier
            .as_ref()
            .and_then(|v| v.get(..4))
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| Some(*v) != year);
        let alternative_titles = self
            .alternative_titles
            .map(|titles| {
                if titles.len() > 64 {
                    return Err(MetadataError::InvalidResponse);
                }
                let mut result = BTreeSet::new();
                for item in titles {
                    text(&item.title, 1024, false)?;
                    result.insert(item.title);
                }
                Ok(result.into_iter().collect::<Vec<_>>())
            })
            .transpose()?;
        let in_cinemas = timestamp(self.in_cinema)?;
        let digital_release = timestamp(self.digital_release)?;
        let physical_release = timestamp(self.physical_release)?;
        let status = derive_movie_status(
            in_cinemas.as_deref(),
            digital_release.as_deref(),
            physical_release.as_deref(),
            chrono::Utc::now(),
        )?;
        Ok(MovieDetails {
            studio: canonical_scalar(self.studio)?,
            genres: canonical_set(self.genres)?,
            keywords: canonical_set(self.keywords)?,
            tmdb_id: self.tmdb_id,
            title: self.title,
            year,
            imdb_id: imdb(self.imdb_id)?,
            runtime,
            // Wire status is unrelated to the native home-release lifecycle.
            status: Some(status.into()),
            in_cinemas,
            digital_release,
            physical_release,
            secondary_year,
            original_language: original_language(self.original_language)?,
            alternative_titles,
        })
    }
}
fn validate_year(value: Option<i64>) -> Result<()> {
    if value.is_some_and(|year| !(1..=9999).contains(&year)) {
        return Err(MetadataError::InvalidResponse);
    }
    Ok(())
}
impl SeriesDetails {
    pub(crate) fn validated(mut self) -> Result<Self> {
        identity(self.tvdb_id)?;
        text(&self.title, 1024, false)?;
        validate_year(self.year)?;
        self.imdb_id = imdb(self.imdb_id)?;
        self.network = canonical_scalar(self.network)?;
        self.original_country = canonical_country(self.original_country)?;
        self.genres = canonical_set(self.genres)?;
        if self.status.as_deref().is_some_and(|status| {
            !matches!(status, "deleted" | "continuing" | "ended" | "upcoming")
        }) || self
            .original_language
            .is_some_and(|value| !(0..=52).contains(&value))
            || self.seasons.len() > 1000
            || self.episodes.len() > MAX_EPISODES
        {
            return Err(MetadataError::InvalidResponse);
        }
        let seasons = self.seasons.iter().copied().collect::<BTreeSet<_>>();
        if seasons.len() != self.seasons.len()
            || seasons
                .iter()
                .any(|season| !(0..=i32::MAX as i64).contains(season))
        {
            return Err(MetadataError::InvalidResponse);
        }
        self.seasons.sort_unstable();
        let mut ids = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        for episode in &self.episodes {
            identity(episode.tvdb_id)?;
            text(&episode.title, 1024, false)?;
            date(episode.air_date.clone())?;
            if let Some(value) = &episode.air_date_utc {
                canonical_datetime(value)?;
            }
            if !seasons.contains(&episode.season)
                || !(0..=i32::MAX as i64).contains(&episode.number)
                || !ids.insert(episode.tvdb_id)
                || !numbers.insert((episode.season, episode.number))
                || episode
                    .absolute_episode_number
                    .is_some_and(|v| !(0..=i32::MAX as i64).contains(&v))
                || episode
                    .runtime
                    .is_some_and(|v| !(0..=i32::MAX as i64).contains(&v))
            {
                return Err(MetadataError::InvalidResponse);
            }
            if let Some(value) = &episode.overview {
                if !value.is_empty() {
                    text(value, 32768, true)?;
                }
            }
            if let Some(value) = &episode.finale_type {
                if !value.is_empty() {
                    text(value, 128, false)?;
                }
            }
        }
        Ok(self)
    }
}
impl MovieDetails {
    pub(crate) fn validated(mut self) -> Result<Self> {
        identity(self.tmdb_id)?;
        text(&self.title, 1024, false)?;
        validate_year(self.year)?;
        validate_year(self.secondary_year)?;
        self.imdb_id = imdb(self.imdb_id)?;
        self.studio = canonical_scalar(self.studio)?;
        self.genres = canonical_set(self.genres)?;
        self.keywords = canonical_set(self.keywords)?;
        if self.runtime.is_some_and(|v| !(0..=10080).contains(&v))
            || self
                .original_language
                .is_some_and(|v| !(0..=57).contains(&v))
            || self.status.as_deref().is_some_and(|v| {
                !matches!(
                    v,
                    "deleted" | "tba" | "announced" | "in_cinemas" | "released"
                )
            })
        {
            return Err(MetadataError::InvalidResponse);
        }
        for value in [
            &self.in_cinemas,
            &self.digital_release,
            &self.physical_release,
        ]
        .into_iter()
        .flatten()
        {
            canonical_datetime(value)?;
        }
        if let Some(titles) = &self.alternative_titles {
            if titles.len() > 64 {
                return Err(MetadataError::InvalidResponse);
            }
            for title in titles {
                text(title, 1024, false)?;
            }
        }
        Ok(self)
    }
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

    #[test]
    fn autotag_wire_facts_validate_before_canonicalization() {
        let show = |extra: serde_json::Value| {
            let mut value = json!({"tvdbId":1,"title":"TV","seasons":[],"episodes":[]});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<Show>(value)
                .and_then(|wire| wire.details().map_err(serde::de::Error::custom))
        };
        let movie = |extra: serde_json::Value| {
            let mut value = json!({"tmdbId":1,"title":"Movie"});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<Movie>(value)
                .and_then(|wire| wire.details().map_err(serde::de::Error::custom))
        };
        let tv = show(json!({"network":"  RÚV  ","originalCountry":"isl","status":"Novel supplied value","genres":["drama","Drama","Áction","áction"]})).unwrap();
        assert_eq!(tv.network.as_deref(), Some("  RÚV  "));
        assert_eq!(tv.original_country.as_deref(), Some("ISL"));
        assert_eq!(tv.status.as_deref(), Some("continuing"));
        assert_eq!(tv.genres.unwrap(), ["Drama", "Áction"]);
        for (wire, expected) in [
            ("EnDeD", "ended"),
            ("UPCOMING", "upcoming"),
            ("deleted", "continuing"),
        ] {
            assert_eq!(
                show(json!({"status":wire})).unwrap().status.as_deref(),
                Some(expected)
            );
        }
        for extra in [
            json!({}),
            json!({"network":null,"originalCountry":null,"status":null,"genres":null}),
            json!({"network":"　 ","originalCountry":" ","status":" "}),
        ] {
            let value = show(extra).unwrap();
            assert!(
                value.network.is_none()
                    && value.original_country.is_none()
                    && value.status.is_none()
                    && value.genres.is_none()
            );
        }
        assert_eq!(show(json!({"genres":[]})).unwrap().genres, Some(vec![]));
        let film = movie(json!({"studio":" Studio ","genres":["b","A","a"],"keywords":["Ý","ý"],"runtime":0,"status":{"ignored":"wire"}})).unwrap();
        assert_eq!(film.studio.as_deref(), Some(" Studio "));
        assert_eq!(film.genres.unwrap(), ["A", "b"]);
        assert_eq!(film.keywords.unwrap(), ["Ý"]);
        assert_eq!(film.runtime, Some(0));
        assert_eq!(film.status.as_deref(), Some("announced"));
        assert_eq!(
            movie(json!({"runtime":10080})).unwrap().runtime,
            Some(10080)
        );
        assert_eq!(movie(json!({"runtime":null})).unwrap().runtime, None);
        assert_eq!(
            movie(json!({"originalLanguage":"zz"}))
                .unwrap()
                .original_language,
            None
        );
        assert_eq!(
            show(json!({"originalLanguage":"zz"}))
                .unwrap()
                .original_language,
            None
        );
        for bad in [json!(-1), json!(10081), json!(0.5), json!("0")] {
            assert!(movie(json!({"runtime":bad})).is_err());
        }
        for field in ["network", "originalCountry", "status", "originalLanguage"] {
            assert!(show(json!({field: 0})).is_err(), "{field}");
        }
        for field in ["studio", "originalLanguage"] {
            assert!(movie(json!({field: 0})).is_err(), "{field}");
        }
        for bad in [json!("us"), json!("USA "), json!("ÍSL"), json!("USAA")] {
            assert!(show(json!({"originalCountry":bad})).is_err());
        }
        for bad in [
            json!("\u{0}"),
            json!("\n"),
            json!("\u{85}"),
            json!("é".repeat(513)),
        ] {
            assert!(show(json!({"network":bad})).is_err());
            assert!(movie(json!({"studio":bad})).is_err());
        }
        for bad in [
            json!("drama"),
            json!([null]),
            json!([1]),
            json!([""]),
            json!(["　"]),
            json!(["ok", "bad\u{0}"]),
            json!(["bad\n"]),
            json!(["é".repeat(129)]),
            json!(vec!["x"; 129]),
            json!(vec!["\\".repeat(256); 128]),
        ] {
            assert!(show(json!({"genres":bad})).is_err(), "{bad}");
            assert!(movie(json!({"genres":bad})).is_err());
            assert!(movie(json!({"keywords":bad})).is_err());
        }
        assert!(
            show(json!({"network":"é".repeat(512),"genres":vec!["é".repeat(128);128]})).is_ok()
        );
        assert_eq!(
            canonical_set(Some(vec!["b".into(), "A".into(), "a".into()])).unwrap(),
            canonical_set(Some(vec!["a".into(), "A".into(), "b".into()])).unwrap()
        );
        // Lookup summaries discard facts only after supplied facts have validated.
        assert!(
            serde_json::from_value::<Show>(json!({"tvdbId":1,"title":"TV","genres":[null]}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<Show>(json!({"tvdbId":1,"title":"TV","genres":[""]}))
                .unwrap()
                .summary()
                .is_err()
        );
    }

    #[test]
    fn movie_lifecycle_has_exact_fixed_clock_boundaries() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .to_utc();
        let instant = |offset| {
            crate::history::canonical_timestamp(&(now + chrono::Duration::seconds(offset))).unwrap()
        };
        for (offset, expected) in [
            (-1, "in_cinemas"),
            (0, "announced"),
            (1, "announced"),
            (-90 * 86400, "in_cinemas"),
            (-90 * 86400 - 1, "released"),
        ] {
            assert_eq!(
                derive_movie_status(Some(&instant(offset)), None, None, now).unwrap(),
                expected
            );
        }
        for (offset, expected) in [(-1, "released"), (0, "released"), (1, "announced")] {
            assert_eq!(
                derive_movie_status(None, Some(&instant(offset)), None, now).unwrap(),
                expected
            );
            assert_eq!(
                derive_movie_status(None, None, Some(&instant(offset)), now).unwrap(),
                expected
            );
        }
        assert_eq!(
            derive_movie_status(Some(&instant(-100 * 86400)), Some(&instant(1)), None, now)
                .unwrap(),
            "in_cinemas"
        );
        assert_eq!(
            derive_movie_status(None, Some(&instant(1)), Some(&instant(0)), now).unwrap(),
            "released"
        );
        assert_eq!(
            derive_movie_status(None, None, None, now).unwrap(),
            "announced"
        );
        assert!(derive_movie_status(Some("invalid"), Some(&instant(-1)), None, now).is_err());
    }

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
