//! Torznab/Newznab requests and bounded, namespace-aware XML responses.
use super::{IndexerAccess, ProviderSettings, http::HttpError};
use crate::api::MediaDomain;
use roxmltree::{Document, Node, ParsingOptions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const XML_BYTES: usize = 1024 * 1024;
const MAX_ITEMS: u32 = 500;
const MAX_QUERY_TEXT_BYTES: usize = 8192;
const MAX_QUERIES: usize = 70;
const NEWZNAB: &str = "http://www.newznab.com/DTD/2010/feeds/attributes/";
const TORZNAB: &str = "http://torznab.com/schemas/2015/feed";

#[derive(Debug)]
pub enum IndexerError {
    InvalidRequest,
    Unsupported,
    Authentication,
    RateLimited { retry_after_seconds: Option<u32> },
    InvalidResponse,
    Transport(HttpError),
}
type Result<T> = std::result::Result<T, IndexerError>;
impl From<HttpError> for IndexerError {
    fn from(value: HttpError) -> Self {
        Self::Transport(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum SearchEngine {
    Raw,
    Sphinx,
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct SearchCapability {
    pub available: bool,
    pub parameters: Vec<String>,
    pub aggregate_ids: bool,
    pub search_engine: SearchEngine,
}
impl SearchCapability {
    fn supports(&self, parameter: &str) -> bool {
        self.available && self.parameters.iter().any(|p| p == parameter)
    }
}
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct IndexerCategory {
    pub id: u32,
    pub parent_id: Option<u32>,
}
/// A selectable category; labels are untrusted remote text, bounded and secret-redacted.
#[derive(Clone, Serialize, ts_rs::TS)]
pub struct CategoryOption {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub label: String,
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct Capabilities {
    pub max_limit: u32,
    pub default_limit: u32,
    pub search: SearchCapability,
    pub tv: SearchCapability,
    pub movies: SearchCapability,
    pub categories: Vec<IndexerCategory>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct IndexerTest {
    pub capabilities: Capabilities,
    pub domains: Vec<MediaDomain>,
}

fn default_limit() -> u32 {
    100
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TvNumbering {
    Episode {
        season: u32,
        episode: u32,
    },
    Season {
        season: u32,
    },
    Daily {
        date: String,
    },
    DailySeason {
        year: u16,
    },
    Special {
        episode_title: String,
    },
    Anime {
        absolute_episode: u32,
        #[serde(default)]
        #[ts(optional=nullable)]
        season: Option<u32>,
        #[serde(default)]
        #[ts(optional=nullable)]
        episode: Option<u32>,
    },
    AnimeSeason {
        season: u32,
        #[serde(default)]
        #[ts(as = "Option<Vec<String>>", optional)]
        season_aliases: Vec<String>,
    },
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TvSearchMode {
    #[default]
    Default,
    Ids,
    Titles,
    Both,
}
#[derive(Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IndexerSearch {
    Rss {
        media_type: MediaDomain,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        offset: u32,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        query_index: u32,
        #[serde(default = "default_limit")]
        #[ts(as = "Option<u32>", optional)]
        limit: u32,
    },
    Tv {
        title: String,
        #[serde(default)]
        #[ts(as = "Option<Vec<String>>", optional)]
        aliases: Vec<String>,
        #[serde(default)]
        #[ts(optional = nullable)]
        tvdb_id: Option<u32>,
        #[serde(default)]
        #[ts(optional = nullable)]
        tvmaze_id: Option<u32>,
        #[serde(default)]
        #[ts(optional = nullable)]
        rage_id: Option<u32>,
        #[serde(default)]
        #[ts(optional = nullable)]
        imdb_id: Option<String>,
        #[serde(default)]
        #[ts(optional = nullable)]
        tmdb_id: Option<u32>,
        numbering: TvNumbering,
        #[serde(default)]
        #[ts(as = "Option<TvSearchMode>", optional)]
        search_mode: TvSearchMode,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        offset: u32,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        query_index: u32,
        #[serde(default = "default_limit")]
        #[ts(as = "Option<u32>", optional)]
        limit: u32,
    },
    Movie {
        title: String,
        #[serde(default)]
        #[ts(as = "Option<Vec<String>>", optional)]
        aliases: Vec<String>,
        #[serde(default)]
        #[ts(optional = nullable)]
        year: Option<u16>,
        #[serde(default)]
        #[ts(optional = nullable)]
        imdb_id: Option<String>,
        #[serde(default)]
        #[ts(optional = nullable)]
        tmdb_id: Option<u32>,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        offset: u32,
        #[serde(default)]
        #[ts(as = "Option<u32>", optional)]
        query_index: u32,
        #[serde(default = "default_limit")]
        #[ts(as = "Option<u32>", optional)]
        limit: u32,
    },
}
#[derive(Serialize, ts_rs::TS)]
pub struct ReleaseMetadata {
    pub title: Option<String>,
    pub size_bytes: Option<u64>,
    pub published_at: String,
    pub categories: Vec<u32>,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub peers: Option<u32>,
    pub languages: Vec<String>,
}
// Deliberately neither Serialize nor Debug: guid, locators and attributes can contain credentials.
pub struct Release {
    pub metadata: ReleaseMetadata,
    pub guid: Option<String>,
    pub facts: ReleaseFacts,
    pub download_url: String,
    pub attributes: BTreeMap<String, Vec<String>>,
}
pub enum ReleaseIdentifiers {
    Tv {
        tvdb_id: Option<u32>,
        tvmaze_id: Option<u32>,
        tvrage_id: Option<u32>,
        tmdb_id: Option<u32>,
        imdb_id: Option<String>,
    },
    Movie {
        tmdb_id: Option<u32>,
        imdb_id: Option<String>,
    },
}
pub struct TorrentFacts {
    pub info_hash: Option<String>,
    pub magnet_url: Option<String>,
    pub download_volume_factor: Option<f64>,
    pub upload_volume_factor: Option<f64>,
    pub minimum_ratio: Option<f64>,
    pub minimum_seed_seconds: Option<u64>,
    pub internal: Option<bool>,
}
pub struct ReleaseFacts {
    pub identifiers: ReleaseIdentifiers,
    pub scene: Option<bool>,
    pub nuked: Option<bool>,
    pub subtitles: Vec<String>,
    pub has_subtitles: Option<bool>,
    pub comments_url: Option<String>,
    pub info_url: Option<String>,
    pub torrent: Option<TorrentFacts>,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum IndexerItemWarningCode {
    InvalidItem,
}
#[derive(Serialize, ts_rs::TS)]
pub struct IndexerItemWarning {
    pub index: u32,
    pub code: IndexerItemWarningCode,
}
#[derive(Serialize, ts_rs::TS)]
pub struct IndexerContinuation {
    pub query_index: u32,
    pub offset: u32,
}
#[derive(Serialize, ts_rs::TS)]
pub struct IndexerPage {
    pub warnings: Vec<IndexerItemWarning>,
    pub query_index: u32,
    pub query_count: u32,
    pub next_query: Option<IndexerContinuation>,
    pub media_type: MediaDomain,
    pub items: Vec<ReleaseMetadata>,
    pub offset: u32,
    pub limit: u32,
    pub total: Option<u32>,
    pub next_offset: Option<u32>,
}
pub struct ReleasePage {
    pub warnings: Vec<IndexerItemWarning>,
    pub items: Vec<Release>,
    pub offset: u32,
    pub limit: u32,
    pub total: Option<u32>,
    pub next_offset: Option<u32>,
}

fn xml(input: &str) -> Result<Document<'_>> {
    if input.len() > XML_BYTES {
        return Err(IndexerError::InvalidResponse);
    }
    let doc = Document::parse_with_options(
        input,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 20_000,
            entity_resolver: None,
        },
    )
    .map_err(|_| IndexerError::InvalidResponse)?;
    let root = doc.root_element();
    if root.has_tag_name("error") {
        let description = root
            .attribute("description")
            .unwrap_or("")
            .to_ascii_lowercase();
        if description.contains("request limit reached") {
            return Err(IndexerError::RateLimited {
                retry_after_seconds: root.attribute("retry").and_then(|s| s.parse().ok()),
            });
        }
        return Err(
            match root.attribute("code").and_then(|s| s.parse::<u32>().ok()) {
                Some(100..=199) => IndexerError::Authentication,
                Some(500 | 429) => IndexerError::RateLimited {
                    retry_after_seconds: root.attribute("retry").and_then(|s| s.parse().ok()),
                },
                Some(203) => IndexerError::Unsupported,
                _ => IndexerError::InvalidResponse,
            },
        );
    }
    Ok(doc)
}
fn number(value: Option<&str>) -> Result<u32> {
    value
        .ok_or(IndexerError::InvalidResponse)?
        .parse()
        .map_err(|_| IndexerError::InvalidResponse)
}
fn category_id(value: Option<&str>) -> Result<u32> {
    let n = number(value)?;
    if n == 0 || n > i32::MAX as u32 {
        Err(IndexerError::InvalidResponse)
    } else {
        Ok(n)
    }
}
fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Result<Option<Node<'a, 'i>>> {
    let mut found = node.children().filter(|n| n.has_tag_name(name));
    let first = found.next();
    if found.next().is_some() {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(first)
}
// Validate all advertised IDs, including filtered roots, before projecting selectable options.
fn category_nodes<'a, 'i>(root: Node<'a, 'i>) -> Result<Vec<(u32, Option<u32>, Node<'a, 'i>)>> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(container) = child(root, "categories")? {
        for parent in container.children().filter(|n| n.has_tag_name("category")) {
            let id = category_id(parent.attribute("id"))?;
            for (node, parent_id) in std::iter::once((parent, None)).chain(
                parent
                    .children()
                    .filter(|n| n.has_tag_name("subcat"))
                    .map(|n| (n, Some(id))),
            ) {
                if result.len() == 4096 {
                    return Err(IndexerError::InvalidResponse);
                }
                let id = category_id(node.attribute("id"))?;
                if !seen.insert(id) {
                    return Err(IndexerError::InvalidResponse);
                }
                result.push((id, parent_id, node));
            }
        }
    }
    Ok(result)
}

/// Standard choices used only when discovery fails or the endpoint is not configured.
pub fn standard_categories(domain: MediaDomain) -> Vec<CategoryOption> {
    let (root, name, children): (u32, &str, &[(u32, &str)]) = match domain {
        MediaDomain::Tv => (
            5000,
            "TV",
            &[
                (5010, "WEB-DL"),
                (5020, "Foreign"),
                (5030, "SD"),
                (5040, "HD"),
                (5045, "UHD"),
                (5050, "Other"),
                (5060, "Sport"),
                (5070, "Anime"),
                (5080, "Documentary"),
            ],
        ),
        MediaDomain::Movies => (
            2000,
            "Movies",
            &[
                (2010, "Foreign"),
                (2020, "Other"),
                (2030, "SD"),
                (2040, "HD"),
                (2045, "UHD"),
                (2050, "BluRay"),
                (2060, "3D"),
            ],
        ),
    };
    std::iter::once(CategoryOption {
        id: root,
        parent_id: None,
        label: name.into(),
    })
    .chain(children.iter().map(|(id, label)| CategoryOption {
        id: *id,
        parent_id: Some(root),
        label: (*label).into(),
    }))
    .collect()
}

/// Caps-only discovery, independent of search/limit validation and without scoped parameters.
/// Empty or absent advertised categories are a successful empty result, never standard fallback.
pub async fn discover_categories(
    operation: &super::http::HttpOperation<'_>,
    endpoint: &str,
    access: &IndexerAccess<'_>,
    domain: MediaDomain,
) -> Result<Vec<CategoryOption>> {
    if !access.validate() {
        return Err(IndexerError::InvalidRequest);
    }
    let body = fetch(
        operation,
        endpoint,
        access.api_key,
        vec![("t".into(), "caps".into()), ("o".into(), "xml".into())],
    )
    .await?;
    let result = (|| {
        let doc = xml(&body)?;
        let root = doc.root_element();
        if !root.has_tag_name("caps") {
            return Err(IndexerError::InvalidResponse);
        }
        let ignored = [1000, 3000, 4000, 6000, 7000];
        let deferred = match domain {
            MediaDomain::Tv => 2000,
            MediaDomain::Movies => 5000,
        };
        let mut options = Vec::new();
        for (id, parent_id, node) in category_nodes(root)? {
            if ignored.contains(&parent_id.unwrap_or(id)) {
                continue;
            }
            let label = node
                .attribute("name")
                .ok_or(IndexerError::InvalidResponse)?;
            if !valid_text(label, 512) {
                return Err(IndexerError::InvalidResponse);
            }
            let private = access.api_key.into_iter().chain(
                access
                    .tv_parameters
                    .iter()
                    .chain(access.movie_parameters)
                    .map(|p| p.value.as_str()),
            );
            let label = if private.filter(|s| !s.is_empty()).any(|s| label.contains(s)) {
                "[redacted]".to_owned()
            } else {
                label.to_owned()
            };
            options.push(CategoryOption {
                id,
                parent_id,
                label,
            });
        }
        // Group each parent's ascending children directly after it. Opposite-domain roots go last.
        options.sort_by_key(|o| {
            let root = o.parent_id.unwrap_or(o.id);
            (root == deferred, root, o.parent_id.is_some(), o.id)
        });
        Ok(options)
    })();
    cooldown(operation, result)
}

pub fn parse_capabilities(input: &str) -> Result<Capabilities> {
    let doc = xml(input)?;
    let root = doc.root_element();
    if !root.has_tag_name("caps") {
        return Err(IndexerError::InvalidResponse);
    }
    let (max_limit, default_limit) = if let Some(limits) = child(root, "limits")? {
        let max = limits.attribute("max").and_then(|s| s.parse::<i32>().ok());
        let default = limits
            .attribute("default")
            .and_then(|s| s.parse::<i32>().ok());
        let (Some(max), Some(default)) = (max, default) else {
            return parse_capabilities("<caps/>");
        };
        if max <= 0 || default <= 0 || default > max {
            return Err(IndexerError::InvalidResponse);
        }
        (max as u32, default as u32)
    } else {
        (100, 100)
    };
    let searching = child(root, "searching")?;
    let capability = |name| -> Result<SearchCapability> {
        let n = searching
            .map(|parent| child(parent, name))
            .transpose()?
            .flatten();
        if searching.is_some() && n.is_none() {
            return Ok(SearchCapability {
                available: false,
                parameters: vec![],
                aggregate_ids: false,
                search_engine: SearchEngine::Sphinx,
            });
        }
        let available = match n.and_then(|n| n.attribute("available")) {
            Some("yes") => true,
            Some("no") => false,
            None if searching.is_none() => true,
            _ => return Err(IndexerError::InvalidResponse),
        };
        let explicit = n.and_then(|n| n.attribute("supportedParams"));
        let params = explicit.unwrap_or(match name {
            "tv-search" => "q,rid,season,ep",
            "movie-search" => "q,imdbid,imdbtitle,imdbyear",
            _ => "q",
        });
        if params.len() > 1024 {
            return Err(IndexerError::InvalidResponse);
        }
        let parameters = params
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| {
                matches!(
                    s.as_str(),
                    "q" | "title"
                        | "imdbtitle"
                        | "imdbyear"
                        | "rid"
                        | "tvdbid"
                        | "tvmazeid"
                        | "imdbid"
                        | "tmdbid"
                        | "season"
                        | "ep"
                        | "year"
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(SearchCapability {
            available,
            parameters,
            aggregate_ids: available && name != "search" && explicit.is_some(),
            search_engine: if available
                && n.and_then(|n| n.attribute("searchEngine")) == Some("raw")
            {
                SearchEngine::Raw
            } else {
                SearchEngine::Sphinx
            },
        })
    };
    let categories = category_nodes(root)?
        .into_iter()
        .map(|(id, parent_id, _)| IndexerCategory { id, parent_id })
        .collect();
    Ok(Capabilities {
        max_limit,
        default_limit,
        search: capability("search")?,
        tv: capability("tv-search")?,
        movies: capability("movie-search")?,
        categories,
    })
}

pub struct SearchPlan {
    pub media_type: MediaDomain,
    pub parameters: Vec<(String, String)>,
    pub offset: u32,
    pub limit: u32,
}
fn clean_title(title: &str) -> String {
    use icu_properties::{CodePointMapData, props::GeneralCategory};
    let title = title.trim();
    let title = if title
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("the "))
    {
        &title[4..]
    } else {
        title
    };
    let categories = CodePointMapData::<GeneralCategory>::new();
    let mut output = String::new();
    for ch in icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize_iter(title.chars())
    {
        if categories.get(ch) == GeneralCategory::NonspacingMark
            || matches!(ch, '.' | '\'' | '`' | '´' | '’' | '‘')
        {
            continue;
        }
        if ch == '&' {
            output.push_str("and");
        } else if ch.is_alphanumeric() || ch == '_' {
            output.push(ch);
        } else {
            output.push(' ');
        }
    }
    let joined = output.split_whitespace().collect::<Vec<_>>().join(" ");
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(&joined)
        .into_owned()
}
fn engine_title(title: &str, engine: SearchEngine) -> String {
    match engine {
        SearchEngine::Raw => title.replace('+', " "),
        SearchEngine::Sphinx => clean_title(title),
    }
}
fn title_variants(titles: &[String], raw: bool) -> Vec<String> {
    let mut variants = titles.to_vec();
    if raw {
        for title in titles {
            let cleaned = clean_title(title);
            if !cleaned.is_empty() && !variants.contains(&cleaned) {
                variants.push(cleaned);
            }
        }
    }
    variants
}
fn valid_text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn imdb(value: &str) -> Result<&str> {
    let digits = value.strip_prefix("tt").unwrap_or(value);
    if !(7..=10).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        Err(IndexerError::InvalidRequest)
    } else {
        Ok(digits)
    }
}
fn scopes(
    settings: &ProviderSettings,
) -> Result<(
    Option<&super::TvIndexerScope>,
    Option<&super::MovieIndexerScope>,
)> {
    match settings {
        ProviderSettings::Torznab { tv, movies, .. }
        | ProviderSettings::Newznab { tv, movies, .. } => Ok((tv.as_ref(), movies.as_ref())),
        _ => Err(IndexerError::Unsupported),
    }
}
fn plan_single(
    settings: &ProviderSettings,
    caps: &Capabilities,
    request: &IndexerSearch,
) -> Result<SearchPlan> {
    let (tv, movies) = scopes(settings)?;
    let (domain, offset, limit, anime) = match request {
        IndexerSearch::Rss {
            media_type,
            offset,
            limit,
            ..
        } => (*media_type, *offset, *limit, false),
        IndexerSearch::Tv {
            offset,
            limit,
            numbering,
            ..
        } => (
            MediaDomain::Tv,
            *offset,
            *limit,
            matches!(
                numbering,
                TvNumbering::Anime { .. } | TvNumbering::AnimeSeason { .. }
            ),
        ),
        IndexerSearch::Movie { offset, limit, .. } => (MediaDomain::Movies, *offset, *limit, false),
    };
    if limit == 0 || limit > MAX_ITEMS {
        return Err(IndexerError::InvalidRequest);
    }
    let categories: BTreeSet<u32> = match domain {
        MediaDomain::Tv => {
            let scope = tv.ok_or(IndexerError::Unsupported)?;
            match request {
                IndexerSearch::Rss { .. } => scope
                    .categories
                    .iter()
                    .chain(&scope.anime_categories)
                    .copied()
                    .collect(),
                _ if anime => scope.anime_categories.iter().copied().collect(),
                _ => scope.categories.iter().copied().collect(),
            }
        }
        MediaDomain::Movies => movies
            .ok_or(IndexerError::Unsupported)?
            .categories
            .iter()
            .copied()
            .collect(),
    };
    if categories.is_empty()
        || (domain == MediaDomain::Movies
            && !categories
                .iter()
                .any(|id| caps.categories.iter().any(|c| c.id == *id)))
    {
        return Err(IndexerError::Unsupported);
    }
    let mut params = vec![];
    match request {
        IndexerSearch::Rss { .. } => {
            if !caps.search.available {
                return Err(IndexerError::Unsupported);
            }
            params.push(("t".into(), "search".into()));
        }
        IndexerSearch::Tv {
            title,
            tvdb_id,
            tvmaze_id,
            rage_id,
            imdb_id,
            tmdb_id,
            numbering,
            ..
        } => {
            if !valid_text(title, MAX_QUERY_TEXT_BYTES)
                || [tvdb_id, tvmaze_id, rage_id, tmdb_id]
                    .iter()
                    .any(|n| n.is_some_and(|n| n == 0 || n > i32::MAX as u32))
            {
                return Err(IndexerError::InvalidRequest);
            }
            if let Some(id) = imdb_id {
                imdb(id)?;
            }
            let generic_title = engine_title(title, caps.search.search_engine);
            let tv_title = engine_title(title, caps.tv.search_engine);
            let (season, ep, suffix) = match numbering {
                TvNumbering::DailySeason { year } => {
                    if !(1800..=9999).contains(year) {
                        return Err(IndexerError::InvalidRequest);
                    }
                    (Some(year.to_string()), None, year.to_string())
                }
                TvNumbering::Special { episode_title } => {
                    if generic_title.trim().is_empty() {
                        return Err(IndexerError::Unsupported);
                    }
                    if !valid_text(episode_title, 512) {
                        return Err(IndexerError::InvalidRequest);
                    }
                    return generic_plan(
                        caps,
                        domain,
                        categories,
                        offset,
                        limit,
                        engine_title(
                            &format!("{title} {episode_title}"),
                            caps.search.search_engine,
                        ),
                    );
                }
                TvNumbering::Episode { season, episode } => {
                    if *season > 9999 || *episode > 9999 {
                        return Err(IndexerError::InvalidRequest);
                    }
                    (
                        Some(if *season == 0 {
                            "00".to_string()
                        } else {
                            season.to_string()
                        }),
                        Some(episode.to_string()),
                        format!("S{season:02}E{episode:02}"),
                    )
                }
                TvNumbering::Season { season } => {
                    if *season > 9999 {
                        return Err(IndexerError::InvalidRequest);
                    }
                    (
                        Some(if *season == 0 {
                            "00".to_string()
                        } else {
                            season.to_string()
                        }),
                        None,
                        format!("S{season:02}"),
                    )
                }
                TvNumbering::Daily { date } => {
                    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                        .map_err(|_| IndexerError::InvalidRequest)?;
                    if date.len() != 10 || parsed.format("%Y-%m-%d").to_string() != *date {
                        return Err(IndexerError::InvalidRequest);
                    }
                    (
                        Some(date[..4].to_string()),
                        Some(date[5..].replace('-', "/")),
                        date.replace('-', "."),
                    )
                }
                TvNumbering::AnimeSeason {
                    season,
                    season_aliases,
                } => {
                    if *season == 0
                        || *season > 9999
                        || season_aliases.len() > 16
                        || season_aliases.iter().any(|s| !valid_text(s, 512))
                    {
                        return Err(IndexerError::InvalidRequest);
                    }
                    if let Some(alias) = season_aliases.first() {
                        return generic_plan(
                            caps,
                            domain,
                            categories,
                            offset,
                            limit,
                            engine_title(alias, caps.search.search_engine),
                        );
                    }
                    if !tv.is_some_and(|scope| scope.anime_standard_format_search)
                        || !caps.tv.supports("season")
                    {
                        return Err(IndexerError::Unsupported);
                    }
                    (Some(season.to_string()), None, format!("S{season:02}"))
                }

                TvNumbering::Anime {
                    absolute_episode,
                    season,
                    episode,
                } => {
                    if season.is_some() != episode.is_some()
                        || season.is_some_and(|n| n == 0 || n > 9999)
                        || episode.is_some_and(|n| n == 0 || n > 9999)
                    {
                        return Err(IndexerError::InvalidRequest);
                    }
                    if *absolute_episode == 0 || *absolute_episode > 999999 {
                        return Err(IndexerError::InvalidRequest);
                    }
                    (None, None, format!("{absolute_episode:02}"))
                }
            };
            let absolute_anime = matches!(numbering, TvNumbering::Anime { .. });
            let structured = caps.tv.available
                && if absolute_anime {
                    caps.tv.supports("q")
                } else {
                    season.as_ref().is_none_or(|_| caps.tv.supports("season"))
                        && ep.as_ref().is_none_or(|_| caps.tv.supports("ep"))
                };
            if structured {
                let candidates = [
                    ("tvdbid", tvdb_id.map(|n| n.to_string())),
                    (
                        "imdbid",
                        imdb_id
                            .as_ref()
                            .map(|id| format!("tt{}", imdb(id).unwrap())),
                    ),
                    ("rid", rage_id.map(|n| n.to_string())),
                    ("tvmazeid", tvmaze_id.map(|n| n.to_string())),
                    ("tmdbid", tmdb_id.map(|n| n.to_string())),
                ];
                let identities = candidates.into_iter().filter_map(|(key, value)| {
                    value
                        .filter(|_| caps.tv.supports(key))
                        .map(|value| (key.to_string(), value))
                });
                params.extend(identities.take(if caps.tv.aggregate_ids { 5 } else { 1 }));
                if params.is_empty() && absolute_anime {
                    if generic_title.trim().is_empty() {
                        return Err(IndexerError::Unsupported);
                    }
                    return generic_plan(
                        caps,
                        domain,
                        categories,
                        offset,
                        limit,
                        format!("{generic_title} {suffix}"),
                    );
                }
                if absolute_anime {
                    params.push(("q".into(), suffix.clone()));
                }
                if params.is_empty() {
                    if caps.tv.supports("title") {
                        params.push(("title".into(), title.clone()));
                    } else if caps.tv.supports("q") {
                        if tv_title.trim().is_empty() {
                            return Err(IndexerError::Unsupported);
                        }
                        params.push(("q".into(), tv_title.clone()));
                    } else {
                        if matches!(numbering, TvNumbering::AnimeSeason { .. }) {
                            return Err(IndexerError::Unsupported);
                        }
                        if generic_title.trim().is_empty() {
                            return Err(IndexerError::Unsupported);
                        }
                        return generic_plan(
                            caps,
                            domain,
                            categories,
                            offset,
                            limit,
                            format!("{generic_title} {suffix}"),
                        );
                    }
                }
                params.push(("t".into(), "tvsearch".into()));
                if let Some(s) = season {
                    params.push(("season".into(), s));
                }
                if let Some(e) = ep {
                    params.push(("ep".into(), e));
                }
            } else {
                if generic_title.trim().is_empty() {
                    return Err(IndexerError::Unsupported);
                }
                return generic_plan(
                    caps,
                    domain,
                    categories,
                    offset,
                    limit,
                    format!("{generic_title} {suffix}"),
                );
            }
        }
        IndexerSearch::Movie {
            title,
            year,
            imdb_id,
            tmdb_id,
            ..
        } => {
            if !valid_text(title, MAX_QUERY_TEXT_BYTES)
                || year.is_some_and(|y| !(1800..=9999).contains(&y))
                || tmdb_id.is_some_and(|n| n == 0 || n > i32::MAX as u32)
            {
                return Err(IndexerError::InvalidRequest);
            }
            let imdb_digits = imdb_id.as_deref().map(imdb).transpose()?;
            let generic_title = engine_title(title, caps.search.search_engine);
            let candidates = [
                ("tmdbid", tmdb_id.map(|n| n.to_string())),
                ("imdbid", imdb_digits.map(str::to_string)),
            ];
            let identities = candidates.into_iter().filter_map(|(key, value)| {
                value
                    .filter(|_| caps.movies.supports(key))
                    .map(|value| (key.to_string(), value))
            });
            params.extend(identities.take(if caps.movies.aggregate_ids { 2 } else { 1 }));
            if params.is_empty() {
                let year = year.ok_or(IndexerError::Unsupported)?;
                if generic_title.trim().is_empty() {
                    return Err(IndexerError::Unsupported);
                }
                return generic_plan(
                    caps,
                    domain,
                    categories,
                    offset,
                    limit,
                    if movies.is_some_and(|scope| scope.remove_year) {
                        generic_title.clone()
                    } else {
                        format!("{generic_title} {year}")
                    },
                );
            }
            params.push(("t".into(), "movie".into()));
        }
    }
    Ok(finish_plan(caps, domain, categories, offset, limit, params))
}
fn search_mode(request: &IndexerSearch) -> TvSearchMode {
    match request {
        IndexerSearch::Tv {
            search_mode,
            numbering,
            ..
        } if !matches!(
            numbering,
            TvNumbering::Anime { .. } | TvNumbering::AnimeSeason { .. }
        ) =>
        {
            *search_mode
        }
        IndexerSearch::Tv {
            numbering: TvNumbering::Anime { .. } | TvNumbering::AnimeSeason { .. },
            ..
        } => TvSearchMode::Both,
        _ => TvSearchMode::Default,
    }
}
fn query_index(request: &IndexerSearch) -> u32 {
    match request {
        IndexerSearch::Rss { query_index, .. }
        | IndexerSearch::Tv { query_index, .. }
        | IndexerSearch::Movie { query_index, .. } => *query_index,
    }
}
fn has_ids(plan: &SearchPlan) -> bool {
    plan.parameters.iter().any(|(key, _)| {
        matches!(
            key.as_str(),
            "tvdbid" | "tvmazeid" | "rid" | "imdbid" | "tmdbid"
        )
    })
}
fn plan_queries(
    settings: &ProviderSettings,
    caps: &Capabilities,
    request: &IndexerSearch,
) -> Result<Vec<SearchPlan>> {
    let mut titles = Vec::new();
    if let IndexerSearch::Tv { title, aliases, .. } | IndexerSearch::Movie { title, aliases, .. } =
        request
    {
        if !valid_text(title, 512)
            || aliases.len() > 16
            || aliases.iter().any(|s| !valid_text(s, 512))
        {
            return Err(IndexerError::InvalidRequest);
        }
        for title in std::iter::once(title).chain(aliases) {
            if !titles.contains(title) {
                titles.push(title.clone());
            }
        }
    }
    let originals = titles.clone();
    if matches!(request, IndexerSearch::Tv { .. }) {
        titles = title_variants(
            &titles,
            caps.search.search_engine == SearchEngine::Raw
                || caps.tv.search_engine == SearchEngine::Raw,
        );
    }
    let all_titles = titles.clone();
    let first = match plan_single(settings, caps, request) {
        Ok(plan) => Some(plan),
        Err(IndexerError::Unsupported) => None,
        Err(error) => return Err(error),
    };
    let mode = search_mode(request);
    if mode == TvSearchMode::Ids && first.as_ref().is_none_or(|p| !has_ids(p)) {
        return Err(IndexerError::Unsupported);
    }
    let mut plans: Vec<SearchPlan> = first
        .into_iter()
        .filter(|p| mode != TvSearchMode::Titles || !has_ids(p))
        .collect();
    if mode == TvSearchMode::Ids {
        titles.clear();
    }
    if let IndexerSearch::Tv {
        numbering: TvNumbering::AnimeSeason { season_aliases, .. },
        ..
    } = request
    {
        let mut ids = request.clone();
        if let IndexerSearch::Tv {
            numbering: TvNumbering::AnimeSeason { season_aliases, .. },
            ..
        } = &mut ids
        {
            season_aliases.clear();
        }
        match plan_single(settings, caps, &ids) {
            Ok(plan)
                if has_ids(&plan) && !plans.iter().any(|p| p.parameters == plan.parameters) =>
            {
                plans.insert(0, plan)
            }
            Ok(_) | Err(IndexerError::Unsupported) => {}
            Err(error) => return Err(error),
        }
        titles.retain(|title| {
            !season_aliases
                .iter()
                .any(|alias| alias.to_lowercase() == title.to_lowercase())
        });
        for alias in &title_variants(
            season_aliases,
            caps.search.search_engine == SearchEngine::Raw,
        ) {
            let mut variant = request.clone();
            if let IndexerSearch::Tv {
                numbering: TvNumbering::AnimeSeason { season_aliases, .. },
                ..
            } = &mut variant
            {
                *season_aliases = vec![alias.clone()];
            }
            match plan_single(settings, caps, &variant) {
                Ok(plan) if !plans.iter().any(|p| p.parameters == plan.parameters) => {
                    plans.push(plan)
                }
                Ok(_) | Err(IndexerError::Unsupported) => {}
                Err(error) => return Err(error),
            }
        }
        if !scopes(settings)?
            .0
            .is_some_and(|scope| scope.anime_standard_format_search)
        {
            titles.clear();
        }
    }

    for title in titles {
        let mut variant = request.clone();
        if let IndexerSearch::Tv {
            numbering: TvNumbering::AnimeSeason { season_aliases, .. },
            ..
        } = &mut variant
        {
            season_aliases.clear();
        }
        match &mut variant {
            IndexerSearch::Tv {
                title: target,
                tvdb_id,
                tvmaze_id,
                rage_id,
                imdb_id,
                tmdb_id,
                ..
            } => {
                *target = title.clone();
                *tvdb_id = None;
                *tvmaze_id = None;
                *rage_id = None;
                *imdb_id = None;
                *tmdb_id = None;
            }
            IndexerSearch::Movie {
                title: target,
                imdb_id,
                tmdb_id,
                ..
            } => {
                *target = title.clone();
                *imdb_id = None;
                *tmdb_id = None;
            }
            IndexerSearch::Rss { .. } => continue,
        }
        match plan_single(settings, caps, &variant) {
            Ok(plan)
                if !originals.contains(&title)
                    && plan.parameters.iter().any(|(key, _)| key == "title") => {}
            Ok(plan) if !plans.iter().any(|p| p.parameters == plan.parameters) => plans.push(plan),
            Ok(_) | Err(IndexerError::Unsupported) => {}
            Err(error) => return Err(error),
        }
    }
    if let IndexerSearch::Tv {
        numbering:
            TvNumbering::Anime {
                season: Some(season),
                episode: Some(episode),
                ..
            },
        ..
    } = request
    {
        let (tv, _) = scopes(settings)?;
        if tv.is_some_and(|scope| scope.anime_standard_format_search)
            && caps.tv.supports("season")
            && caps.tv.supports("ep")
        {
            let mut scoped = settings.clone();
            if let ProviderSettings::Torznab {
                tv: Some(scope), ..
            }
            | ProviderSettings::Newznab {
                tv: Some(scope), ..
            } = &mut scoped
            {
                scope.categories = scope.anime_categories.clone();
            }
            for title in &all_titles {
                let mut variant = request.clone();
                if let IndexerSearch::Tv {
                    numbering,
                    title: target,
                    ..
                } = &mut variant
                {
                    *numbering = TvNumbering::Episode {
                        season: *season,
                        episode: *episode,
                    };
                    *target = title.clone();
                }
                match plan_single(&scoped, caps, &variant) {
                    Ok(plan)
                        if !originals.contains(title)
                            && plan.parameters.iter().any(|(key, _)| key == "title") => {}
                    Ok(plan) if !plans.iter().any(|p| p.parameters == plan.parameters) => {
                        plans.push(plan)
                    }
                    Ok(_) | Err(IndexerError::Unsupported) => {}
                    Err(error) => return Err(error),
                }
                if let IndexerSearch::Tv {
                    tvdb_id,
                    tvmaze_id,
                    rage_id,
                    imdb_id,
                    tmdb_id,
                    ..
                } = &mut variant
                {
                    *tvdb_id = None;
                    *tvmaze_id = None;
                    *rage_id = None;
                    *imdb_id = None;
                    *tmdb_id = None;
                }
                match plan_single(&scoped, caps, &variant) {
                    Ok(plan)
                        if !originals.contains(title)
                            && plan.parameters.iter().any(|(key, _)| key == "title") => {}
                    Ok(plan) if !plans.iter().any(|p| p.parameters == plan.parameters) => {
                        plans.push(plan)
                    }
                    Ok(_) | Err(IndexerError::Unsupported) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    if plans.is_empty() {
        return Err(IndexerError::Unsupported);
    }
    if plans.len() > MAX_QUERIES
        || plans.iter().any(|p| {
            p.parameters
                .iter()
                .any(|(_, value)| value.len() > MAX_QUERY_TEXT_BYTES)
        })
    {
        return Err(IndexerError::InvalidRequest);
    }
    if query_index(request) as usize >= plans.len() {
        return Err(IndexerError::InvalidRequest);
    }
    Ok(plans)
}
/// Selects a single query from the same deterministic plan used by the HTTP operation.
pub fn plan_search(
    settings: &ProviderSettings,
    caps: &Capabilities,
    request: &IndexerSearch,
) -> Result<SearchPlan> {
    let mut plans = plan_queries(settings, caps, request)?;
    Ok(plans.remove(query_index(request) as usize))
}

fn generic_plan(
    caps: &Capabilities,
    domain: MediaDomain,
    categories: BTreeSet<u32>,
    offset: u32,
    limit: u32,
    query: String,
) -> Result<SearchPlan> {
    if !caps.search.supports("q") || query.trim().is_empty() {
        return Err(IndexerError::Unsupported);
    }
    if query.len() > MAX_QUERY_TEXT_BYTES {
        return Err(IndexerError::InvalidRequest);
    }
    Ok(finish_plan(
        caps,
        domain,
        categories,
        offset,
        limit,
        vec![("t".into(), "search".into()), ("q".into(), query)],
    ))
}
fn finish_plan(
    caps: &Capabilities,
    domain: MediaDomain,
    categories: BTreeSet<u32>,
    offset: u32,
    limit: u32,
    mut params: Vec<(String, String)>,
) -> SearchPlan {
    let limit = limit.min(caps.max_limit);
    params.extend([
        (
            "cat".into(),
            categories
                .into_iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ),
        ("offset".into(), offset.to_string()),
        ("limit".into(), limit.to_string()),
        ("extended".into(), "1".into()),
        ("o".into(), "xml".into()),
    ]);
    SearchPlan {
        media_type: domain,
        parameters: params,
        offset,
        limit,
    }
}

fn extension(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().name() == name
        && matches!(node.tag_name().namespace(), Some(NEWZNAB | TORZNAB))
}
fn text(node: Node<'_, '_>, name: &str, max: usize) -> Result<String> {
    let n = child(node, name)?.ok_or(IndexerError::InvalidResponse)?;
    if n.children().any(|n| n.is_element()) {
        return Err(IndexerError::InvalidResponse);
    }
    let value = n.children().filter_map(|n| n.text()).collect::<String>();
    if !valid_text(&value, max) {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(value)
}
fn scalar<'a>(attrs: &'a BTreeMap<String, Vec<String>>, key: &str) -> Result<Option<&'a str>> {
    match attrs.get(key) {
        None => Ok(None),
        Some(v) if v.len() == 1 => Ok(Some(&v[0])),
        _ => Err(IndexerError::InvalidResponse),
    }
}
fn optional_number(attrs: &BTreeMap<String, Vec<String>>, key: &str) -> Result<Option<u32>> {
    scalar(attrs, key)?.map(|s| number(Some(s))).transpose()
}
fn locator(value: &str, torrent: bool) -> Result<String> {
    if !valid_text(value, 8192) {
        return Err(IndexerError::InvalidResponse);
    }
    let url = url::Url::parse(value).map_err(|_| IndexerError::InvalidResponse)?;
    let http = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none();
    let magnet = torrent
        && url.scheme() == "magnet"
        && url.query_pairs().any(|(key, value)| {
            key == "xt" && (value.starts_with("urn:btih:") || value.starts_with("urn:btmh:"))
        });
    if !http && !magnet {
        return Err(IndexerError::InvalidResponse);
    }
    Ok(value.to_string())
}
fn optional_text(node: Node<'_, '_>, name: &str, max: usize) -> Result<Option<String>> {
    if child(node, name)?.is_none() {
        return Ok(None);
    }
    text(node, name, max).map(Some)
}
fn attributes(item: Node<'_, '_>) -> Result<BTreeMap<String, Vec<String>>> {
    let mut attributes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (index, attr) in item
        .children()
        .filter(|n| extension(*n, "attr"))
        .enumerate()
    {
        let name = attr
            .attribute("name")
            .ok_or(IndexerError::InvalidResponse)?;
        let value = attr
            .attribute("value")
            .ok_or(IndexerError::InvalidResponse)?;
        if index >= 128
            || !valid_text(name, 64)
            || value.len() > 8192
            || value.chars().any(char::is_control)
        {
            return Err(IndexerError::InvalidResponse);
        }
        let values = attributes.entry(name.to_ascii_lowercase()).or_default();
        if !values.iter().any(|s| s == value) {
            values.push(value.to_string());
        }
    }
    Ok(attributes)
}
fn publication_date(item: Node<'_, '_>, torrent: bool) -> Result<String> {
    let usenet = if torrent {
        None
    } else {
        item.children()
            .find(|n| {
                extension(*n, "attr")
                    && n.attribute("name")
                        .is_some_and(|s| s.eq_ignore_ascii_case("usenetdate"))
            })
            .and_then(|n| n.attribute("value"))
    };
    let date = if let Some(date) = usenet {
        date.to_string()
    } else {
        text(item, "pubDate", 128)?
    };
    if !valid_text(&date, 128) {
        return Err(IndexerError::InvalidResponse);
    }
    let date = chrono::DateTime::parse_from_rfc2822(&date)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(&date))
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().fixed_offset())
        })
        .map_err(|_| IndexerError::InvalidResponse)?;
    Ok(date
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
fn boolean(attrs: &BTreeMap<String, Vec<String>>, key: &str) -> Result<Option<bool>> {
    scalar(attrs, key)?
        .map(|value| match value {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(IndexerError::InvalidResponse),
        })
        .transpose()
}
fn decimal(attrs: &BTreeMap<String, Vec<String>>, key: &str) -> Result<Option<f64>> {
    scalar(attrs, key)?
        .map(|s| {
            s.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or(IndexerError::InvalidResponse)
        })
        .transpose()
}
fn identifier(attrs: &BTreeMap<String, Vec<String>>, key: &str) -> Result<Option<u32>> {
    Ok(scalar(attrs, key)?
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0 && *n <= i32::MAX as u32))
}

fn parse_item(
    item: Node<'_, '_>,
    published_at: String,
    torrent: bool,
    domain: MediaDomain,
) -> Result<Release> {
    let title = optional_text(item, "title", 8192)?
        .map(|value| html_escape::decode_html_entities(&value).into_owned());
    if title.as_ref().is_some_and(|value| !valid_text(value, 2048)) {
        return Err(IndexerError::InvalidResponse);
    }
    let guid = optional_text(item, "guid", 8192)?;
    let attributes = attributes(item)?;
    let expected = if torrent {
        "application/x-bittorrent"
    } else {
        "application/x-nzb"
    };
    let opposite = if torrent {
        "application/x-nzb"
    } else {
        "application/x-bittorrent"
    };
    let mut enclosure = None;
    let mut compound_enclosure = None;
    let mut unknown = None;
    let mut opposite_seen = false;
    for (index, node) in item
        .children()
        .filter(|n| n.has_tag_name("enclosure"))
        .enumerate()
    {
        if index >= 16 {
            return Err(IndexerError::InvalidResponse);
        }
        let mime = node
            .attribute("type")
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if mime.eq_ignore_ascii_case(expected) {
            if node
                .attribute("type")
                .is_some_and(|value| value.contains(';'))
            {
                if compound_enclosure.is_none() {
                    compound_enclosure = Some(node);
                }
            } else if enclosure.is_none() {
                enclosure = Some(node);
            }
        } else if mime.eq_ignore_ascii_case(opposite) {
            opposite_seen = true;
        } else if unknown.is_none() {
            unknown = Some(node);
        }
    }
    let enclosure = enclosure.or(compound_enclosure).or(unknown);
    if enclosure.is_none() && opposite_seen {
        return Err(IndexerError::InvalidResponse);
    }
    let size = |s: &str| {
        s.parse::<u64>()
            .ok()
            .filter(|n| *n <= 9_007_199_254_740_991)
    };
    let size_attr = scalar(&attributes, "size")?;
    let size_enclosure = enclosure.and_then(|n| n.attribute("length"));
    let size_bytes = size_attr
        .and_then(size)
        .or_else(|| size_enclosure.and_then(size));
    if size_bytes.is_none() && (size_attr.is_some() || size_enclosure.is_some()) {
        return Err(IndexerError::InvalidResponse);
    }
    let categories = attributes
        .get("category")
        .into_iter()
        .flatten()
        .map(|s| category_id(Some(s)))
        .collect::<Result<Vec<_>>>()?;
    let magnet = scalar(&attributes, "magneturl")?
        .map(|s| locator(s, true))
        .transpose()?;
    let download_url = if let Some(url) = enclosure.and_then(|n| n.attribute("url")) {
        locator(url, torrent)?
    } else if let Some(url) = magnet.as_ref().filter(|_| torrent) {
        url.clone()
    } else {
        locator(&text(item, "link", 8192)?, torrent)?
    };
    let seeders = optional_number(&attributes, "seeders")?;
    let explicit_leechers = optional_number(&attributes, "leechers")?;
    let peers = match optional_number(&attributes, "peers")? {
        Some(p) => Some(p),
        None => seeders
            .zip(explicit_leechers)
            .map(|(s, l)| s.checked_add(l).ok_or(IndexerError::InvalidResponse))
            .transpose()?,
    };
    let leechers =
        explicit_leechers.or_else(|| peers.zip(seeders).and_then(|(p, s)| p.checked_sub(s)));
    let language_values = if let Some(values) = attributes.get("language") {
        values.clone()
    } else {
        let mut values = Vec::new();
        for node in item.children().filter(|n| n.has_tag_name("language")) {
            if values.len() >= 128 || node.children().any(|n| n.is_element()) {
                return Err(IndexerError::InvalidResponse);
            }
            let value = node.children().filter_map(|n| n.text()).collect::<String>();
            if !valid_text(&value, 1024) {
                return Err(IndexerError::InvalidResponse);
            }
            values.push(value);
        }
        values
    };
    let mut languages = Vec::new();
    for value in language_values {
        for language in if torrent {
            vec![value.as_str()]
        } else {
            value.split(',').collect()
        } {
            let language = language.trim();
            if !language.is_empty() && !languages.iter().any(|s| s == language) {
                languages.push(language.to_string());
            }
        }
    }
    let imdb_id = scalar(&attributes, "imdb")?
        .and_then(|s| s.strip_prefix("tt").unwrap_or(s).parse::<u64>().ok())
        .filter(|n| *n > 0 && *n <= 9_999_999_999)
        .map(|n| format!("tt{n:07}"));
    let tmdb_id = identifier(&attributes, "tmdbid")?;
    let identifiers = match domain {
        MediaDomain::Tv => ReleaseIdentifiers::Tv {
            tvdb_id: identifier(&attributes, "tvdbid")?,
            tvmaze_id: identifier(&attributes, "tvmazeid")?,
            tvrage_id: identifier(&attributes, "rageid")?,
            tmdb_id,
            imdb_id,
        },
        MediaDomain::Movies => ReleaseIdentifiers::Movie { tmdb_id, imdb_id },
    };
    let tags = attributes.get("tag");
    let scene_flags = [
        boolean(&attributes, "scene")?,
        boolean(&attributes, "prematch")?,
        boolean(&attributes, "haspretime")?,
    ];
    let scene = if tags.is_some_and(|v| v.iter().any(|s| s.eq_ignore_ascii_case("scene")))
        || scene_flags.contains(&Some(true))
    {
        Some(true)
    } else if scene_flags.contains(&Some(false)) {
        Some(false)
    } else {
        None
    };
    let nuked = boolean(&attributes, "nuked")?;
    let comments_url = optional_text(item, "comments", 8192)?
        .map(|s| locator(&s, false))
        .transpose()?;
    let info_url = scalar(&attributes, "info")?
        .map(|s| locator(s, false))
        .transpose()?
        .or_else(|| {
            comments_url
                .as_ref()
                .map(|url| url.strip_suffix("#comments").unwrap_or(url).to_string())
        });
    let torrent_facts = if torrent {
        let info_hash = scalar(&attributes, "infohash")?
            .map(|hash| {
                if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    Ok(hash.to_ascii_lowercase())
                } else {
                    Err(IndexerError::InvalidResponse)
                }
            })
            .transpose()?;
        let minimum_seed_seconds = scalar(&attributes, "minimumseedtime")?
            .map(|s| s.parse::<u64>().map_err(|_| IndexerError::InvalidResponse))
            .transpose()?;
        Some(TorrentFacts {
            info_hash,
            magnet_url: magnet,
            download_volume_factor: decimal(&attributes, "downloadvolumefactor")?,
            upload_volume_factor: decimal(&attributes, "uploadvolumefactor")?,
            minimum_ratio: decimal(&attributes, "minimumratio")?,
            minimum_seed_seconds,
            internal: boolean(&attributes, "internal")?.or_else(|| {
                tags.filter(|v| v.iter().any(|s| s.eq_ignore_ascii_case("internal")))
                    .map(|_| true)
            }),
        })
    } else {
        None
    };
    Ok(Release {
        metadata: ReleaseMetadata {
            title,
            size_bytes,
            published_at,
            categories,
            seeders,
            leechers,
            peers,
            languages,
        },
        guid,
        download_url,
        facts: ReleaseFacts {
            identifiers,
            scene,
            nuked,
            subtitles: attributes.get("subs").cloned().unwrap_or_default(),
            has_subtitles: if domain == MediaDomain::Tv {
                attributes
                    .get("subs")
                    .map(|values| values.iter().any(|s| !s.trim().is_empty()))
            } else {
                None
            },
            comments_url,
            info_url,
            torrent: torrent_facts,
        },
        attributes,
    })
}
pub fn parse_page(
    input: &str,
    offset: u32,
    limit: u32,
    torrent: bool,
    domain: MediaDomain,
) -> Result<ReleasePage> {
    if limit == 0 || limit > MAX_ITEMS {
        return Err(IndexerError::InvalidRequest);
    }
    let doc = xml(input)?;
    let root = doc.root_element();
    if !root.has_tag_name("rss") {
        return Err(IndexerError::InvalidResponse);
    }
    let channel = child(root, "channel")?.ok_or(IndexerError::InvalidResponse)?;
    let mut responses = channel.children().filter(|n| extension(*n, "response"));
    let total = if let Some(response) = responses.next() {
        if number(response.attribute("offset"))? != offset {
            return Err(IndexerError::InvalidResponse);
        }
        Some(number(response.attribute("total"))?)
    } else {
        None
    };
    if responses.next().is_some() {
        return Err(IndexerError::InvalidResponse);
    }
    let mut items = Vec::new();
    let mut guids = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut raw_count = 0;
    for item in channel.children().filter(|n| n.has_tag_name("item")) {
        if raw_count >= limit {
            return Err(IndexerError::InvalidResponse);
        }
        let index = offset
            .checked_add(raw_count)
            .ok_or(IndexerError::InvalidResponse)?;
        raw_count += 1;
        // A missing publication date is a feed failure, never a fabricated timestamp or silent drop.
        let date = publication_date(item, torrent)?;
        match parse_item(item, date, torrent, domain) {
            Ok(release)
                if release
                    .guid
                    .as_ref()
                    .is_none_or(|guid| guids.insert(guid.clone())) =>
            {
                items.push(release)
            }
            Ok(_) | Err(_) => warnings.push(IndexerItemWarning {
                index,
                code: IndexerItemWarningCode::InvalidItem,
            }),
        }
    }
    if items.is_empty() && !warnings.is_empty() {
        return Err(IndexerError::InvalidResponse);
    }
    let end = offset
        .checked_add(raw_count)
        .ok_or(IndexerError::InvalidResponse)?;
    if total.is_some_and(|t| (raw_count > 0 && end > t) || (raw_count == 0 && offset < t)) {
        return Err(IndexerError::InvalidResponse);
    }
    let next_offset = if raw_count == 0
        || total.is_some_and(|t| end >= t)
        || (total.is_none() && raw_count < limit)
    {
        None
    } else {
        Some(end)
    };
    Ok(ReleasePage {
        items,
        offset,
        limit,
        total,
        next_offset,
        warnings,
    })
}

fn endpoint(settings: &ProviderSettings) -> Result<(&str, bool)> {
    match settings {
        ProviderSettings::Torznab { endpoint, .. } => Ok((endpoint, true)),
        ProviderSettings::Newznab { endpoint, .. } => Ok((endpoint, false)),
        _ => Err(IndexerError::Unsupported),
    }
}
async fn fetch(
    operation: &super::http::HttpOperation<'_>,
    endpoint: &str,
    api_key: Option<&str>,
    mut params: Vec<(String, String)>,
) -> Result<String> {
    if let Some(key) = api_key {
        if !valid_text(key, 4096) {
            return Err(IndexerError::InvalidRequest);
        }
        params.push(("apikey".into(), key.into()));
    }
    let bytes = operation.get(endpoint, &params).await?;
    let body = String::from_utf8(bytes).map_err(|_| IndexerError::InvalidResponse)?;
    if api_key.is_none() {
        if let Ok(doc) = Document::parse_with_options(
            &body,
            ParsingOptions {
                allow_dtd: false,
                nodes_limit: 20_000,
                entity_resolver: None,
            },
        ) {
            let root = doc.root_element();
            let description = root
                .attribute("description")
                .unwrap_or("")
                .to_ascii_lowercase();
            if root.has_tag_name("error")
                && !description.contains("request limit reached")
                && (description.contains("missing parameter") || description.contains("apikey"))
            {
                return Err(IndexerError::Authentication);
            }
        }
    }
    Ok(body)
}
fn cooldown<T>(operation: &super::http::HttpOperation<'_>, result: Result<T>) -> Result<T> {
    match result {
        Err(IndexerError::RateLimited {
            retry_after_seconds,
        }) => Err(IndexerError::Transport(
            operation.rate_limit(retry_after_seconds),
        )),
        other => other,
    }
}
async fn capabilities(
    operation: &super::http::HttpOperation<'_>,
    endpoint: &str,
    api_key: Option<&str>,
) -> Result<Capabilities> {
    cooldown(
        operation,
        parse_capabilities(
            &fetch(
                operation,
                endpoint,
                api_key,
                vec![("t".into(), "caps".into()), ("o".into(), "xml".into())],
            )
            .await?,
        ),
    )
}
fn private_parameters(
    access: &IndexerAccess<'_>,
    domain: MediaDomain,
    mut parameters: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let private = match domain {
        MediaDomain::Tv => access.tv_parameters,
        MediaDomain::Movies => access.movie_parameters,
    };
    parameters.extend(private.iter().map(|p| (p.name.clone(), p.value.clone())));
    parameters
}
pub async fn test(
    operation: &super::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
) -> Result<IndexerTest> {
    if !access.validate() {
        return Err(IndexerError::InvalidRequest);
    }
    let (endpoint, torrent) = endpoint(settings)?;
    let caps = capabilities(operation, endpoint, access.api_key).await?;
    let (tv, movies) = scopes(settings)?;
    let mut domains = vec![];
    for domain in tv
        .map(|_| MediaDomain::Tv)
        .into_iter()
        .chain(movies.map(|_| MediaDomain::Movies))
    {
        let plan = plan_search(
            settings,
            &caps,
            &IndexerSearch::Rss {
                media_type: domain,
                offset: 0,
                query_index: 0,
                limit: 1,
            },
        )?;
        cooldown(
            operation,
            parse_page(
                &fetch(
                    operation,
                    endpoint,
                    access.api_key,
                    private_parameters(access, domain, plan.parameters),
                )
                .await?,
                0,
                plan.limit,
                torrent,
                domain,
            ),
        )?;
        domains.push(domain);
    }
    Ok(IndexerTest {
        capabilities: caps,
        domains,
    })
}
pub async fn search(
    operation: &super::http::HttpOperation<'_>,
    settings: &ProviderSettings,
    access: &IndexerAccess<'_>,
    request: &IndexerSearch,
) -> Result<IndexerPage> {
    if !access.validate() {
        return Err(IndexerError::InvalidRequest);
    }
    let (endpoint, torrent) = endpoint(settings)?;
    let caps = capabilities(operation, endpoint, access.api_key).await?;
    let plans = plan_queries(settings, &caps, request)?;
    let mut selected = query_index(request) as usize;
    let (plan, page) = loop {
        let plan = &plans[selected];
        let page = cooldown(
            operation,
            parse_page(
                &fetch(
                    operation,
                    endpoint,
                    access.api_key,
                    private_parameters(access, plan.media_type, plan.parameters.clone()),
                )
                .await?,
                plan.offset,
                plan.limit,
                torrent,
                plan.media_type,
            ),
        )?;
        if page.items.is_empty() && page.offset == 0 && selected + 1 < plans.len() {
            selected += 1;
            continue;
        }
        break (plan, page);
    };
    let next_query = if let Some(offset) = page.next_offset {
        Some(IndexerContinuation {
            query_index: selected as u32,
            offset,
        })
    } else if (!has_ids(plan) || search_mode(request) == TvSearchMode::Both)
        && selected + 1 < plans.len()
    {
        Some(IndexerContinuation {
            query_index: (selected + 1) as u32,
            offset: 0,
        })
    } else {
        None
    };
    let mut items = page
        .items
        .into_iter()
        .map(|r| r.metadata)
        .collect::<Vec<_>>();
    for key in access
        .api_key
        .into_iter()
        .chain(
            access
                .tv_parameters
                .iter()
                .chain(access.movie_parameters)
                .map(|p| p.value.as_str()),
        )
        .filter(|s| !s.is_empty())
    {
        for item in &mut items {
            if let Some(title) = &mut item.title {
                *title = title.replace(key, "[redacted]");
            }
            for language in &mut item.languages {
                *language = language.replace(key, "[redacted]");
            }
        }
    }
    Ok(IndexerPage {
        warnings: page.warnings,
        query_index: selected as u32,
        query_count: plans.len() as u32,
        next_query,
        media_type: plan.media_type,
        items,
        offset: page.offset,
        limit: page.limit,
        total: page.total,
        next_offset: page.next_offset,
    })
}
