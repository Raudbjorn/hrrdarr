//! Another Sonarr (TV) or Radarr (movies) instance as a list source.
//!
//! `GET {url}/api/v3/series` (Sonarr) or `{url}/api/v3/movie?excludeLocalCovers=true` (Radarr) with
//! the instance's API key in `X-Api-Key`; one JSON array, no paging. The base URL keeps any URL-base
//! prefix. Redirects are never followed (so the key cannot leave the configured host): a wrong URL
//! base surfaces as `redirect`. Optional filters combine with AND; inside one filter any value
//! matches (quality profile id membership, any shared tag, case-insensitive root-path substring).
//! The remote row `id` is never an identity; the TVDB/TMDb id is. Remote "monitored" is ignored.
use super::*;
use crate::import_lists::model::ListItem;

pub fn sonarr() -> Implementation {
    Implementation {
        name: "sonarr",
        label: "Sonarr (another instance)",
        description: "Series of another Sonarr instance, optionally filtered by quality profile, tag and root folder.",
        domains: &[MediaDomain::Tv],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: sonarr_fields,
        available: true,
    }
}
pub fn radarr() -> Implementation {
    Implementation {
        name: "radarr",
        label: "Radarr (another instance)",
        description: "Movies of another Radarr instance, optionally filtered by quality profile, tag and movie path.",
        domains: &[MediaDomain::Movies],
        default_interval_minutes: 360,
        min_interval_minutes: 15,
        fields: radarr_fields,
        available: true,
    }
}
fn common(root_label: &'static str, root_help: &'static str) -> Vec<Field> {
    vec![
        field("url", "URL", FieldKind::Url, "Base URL of the other instance, including any URL base.")
            .required()
            .max_length(2048),
        field("quality_profile_ids", "Quality profile IDs", FieldKind::Text, "Remote quality profile ids, comma or space separated; empty means any.")
            .max_length(256),
        field("tag_ids", "Tag IDs", FieldKind::Text, "Remote tag ids, comma or space separated; an item needs at least one; empty means any.")
            .max_length(256),
        field("root_folder_filter", root_label, FieldKind::Text, root_help).max_length(512),
        field("api_key", "API key", FieldKind::Text, "The other instance's API key; sent as X-Api-Key.")
            .required()
            .secret(),
    ]
}
fn sonarr_fields() -> Vec<Field> {
    common(
        "Root folder filter",
        "Comma separated text; a series matches when its root folder contains any of them (case-insensitive); empty means any.",
    )
}
fn radarr_fields() -> Vec<Field> {
    common(
        "Path filter",
        "Comma separated text; a movie matches when its path contains any of them (case-insensitive); empty means any.",
    )
}

/// Remote filters; an empty one accepts everything.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub profiles: Vec<i64>,
    pub tags: Vec<i64>,
    pub roots: Vec<String>,
}
impl Filters {
    pub fn from_list(list: &ListRuntime) -> Result<Self, FetchError> {
        Ok(Self {
            profiles: int_list(list.text("quality_profile_ids"))?,
            tags: int_list(list.text("tag_ids"))?,
            roots: list
                .text("root_folder_filter")
                .map(|raw| {
                    raw.split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

fn sentinel_id(value: Option<&Value>) -> Result<Option<i64>, FetchError> {
    // Both services use 0 for "no id"; that is absence, not corruption.
    match value {
        Some(Value::Number(n)) if n.as_i64() == Some(0) => Ok(None),
        other => id_of(other),
    }
}

pub fn parse(media: MediaDomain, filters: &Filters, document: &Value) -> Fetched {
    let rows = document.as_array().ok_or(FetchError::InvalidResponse)?;
    let path_key = match media {
        MediaDomain::Tv => "rootFolderPath",
        MediaDomain::Movies => "path",
    };
    let mut items = Vec::new();
    for row in rows {
        let object = row.as_object().ok_or(FetchError::InvalidResponse)?;
        let profile = match object.get("qualityProfileId") {
            None | Some(Value::Null) => None,
            other => id_of(other)?,
        };
        let tags: Vec<i64> = match object.get("tags") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(tags)) => tags
                .iter()
                .map(|t| t.as_i64().ok_or(FetchError::InvalidResponse))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(FetchError::InvalidResponse),
        };
        let root = match object.get(path_key) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.to_lowercase(),
            Some(_) => return Err(FetchError::InvalidResponse),
        };
        let keep = (filters.profiles.is_empty()
            || profile.is_some_and(|p| filters.profiles.contains(&p)))
            && (filters.tags.is_empty() || tags.iter().any(|t| filters.tags.contains(t)))
            && (filters.roots.is_empty()
                || filters.roots.iter().any(|f| root.contains(f.as_str())));
        let item = ListItem {
            tvdb_id: if media == MediaDomain::Tv {
                sentinel_id(object.get("tvdbId"))?
            } else {
                None
            },
            tmdb_id: if media == MediaDomain::Movies {
                sentinel_id(object.get("tmdbId"))?
            } else {
                None
            },
            imdb_id: if media == MediaDomain::Movies {
                imdb_of(object.get("imdbId"))?
            } else {
                None
            },
            title: title_of(object.get("title")),
            year: match year_of(object.get("year")) {
                Ok(Some(y)) => Some(y),
                // Sonarr/Radarr report year 0 for "unknown".
                _ if object.get("year").and_then(Value::as_i64) == Some(0) => None,
                Ok(None) => None,
                Err(e) => return Err(e),
            },
        };
        if keep {
            items.push(item);
        }
    }
    finish(media, items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let base = list.need_text("url")?;
    let key = list.need_secret("api_key")?;
    let path = match list.implementation.as_str() {
        "sonarr" if list.media == MediaDomain::Tv => "api/v3/series",
        "radarr" if list.media == MediaDomain::Movies => "api/v3/movie?excludeLocalCovers=true",
        _ => return Err(FetchError::Unsupported),
    };
    let filters = Filters::from_list(list)?;
    let document = get_json(transport, join(base, path), &[("x-api-key", key)]).await?;
    parse(list.media, &filters, &document)
}
