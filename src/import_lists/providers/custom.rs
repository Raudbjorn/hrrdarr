//! Custom list (Sonarr "Custom Lists"): any HTTP(S) endpoint that returns a JSON array of objects.
//!
//! Accepted keys per object (snake_case or camelCase): `tvdb_id`, `tmdb_id`, `imdb_id`, `title`,
//! `year`. An optional bearer token is sent as `Authorization: Bearer`. The whole document must be
//! a JSON array of objects: any other shape, or a present-but-malformed identity, fails the fetch
//! so a corrupt document never authorises additions.
use super::*;
use crate::import_lists::model::{ListItem, SettingValue};

pub fn implementation() -> Implementation {
    Implementation {
        name: "custom",
        label: "Custom list",
        description: "A JSON array of items from any HTTP(S) URL, with tvdb_id/tmdb_id/imdb_id/title/year keys.",
        domains: &[MediaDomain::Tv, MediaDomain::Movies],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field("url", "List URL", FieldKind::Url, "Returns a JSON array of items.").required().max_length(2048),
        field("bearer_token", "Bearer token", FieldKind::Text, "Optional; sent as Authorization: Bearer.").secret(),
    ]
}

fn pick<'a>(object: &'a serde_json::Map<String, Value>, snake: &str, camel: &str) -> Option<&'a Value> {
    object.get(snake).or_else(|| object.get(camel))
}

pub fn parse(media: MediaDomain, document: &Value) -> Fetched {
    let array = document.as_array().ok_or(FetchError::InvalidResponse)?;
    let mut items = Vec::with_capacity(array.len().min(MAX_ITEMS_HINT));
    for value in array {
        let object = value.as_object().ok_or(FetchError::InvalidResponse)?;
        items.push(ListItem {
            tvdb_id: id_of(pick(object, "tvdb_id", "tvdbId"))?,
            tmdb_id: id_of(pick(object, "tmdb_id", "tmdbId"))?,
            imdb_id: imdb_of(pick(object, "imdb_id", "imdbId"))?,
            title: title_of(object.get("title")),
            year: year_of(object.get("year"))?,
        });
    }
    finish(media, items)
}
const MAX_ITEMS_HINT: usize = 1024;

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let url = list.need_text("url")?.to_owned();
    let bearer = list.secret("bearer_token").map(|t| format!("Bearer {t}"));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(value) = &bearer {
        headers.push(("authorization", value));
    }
    let document = get_json(transport, url, &headers).await?;
    parse(list.media, &document)
}

#[allow(dead_code)]
fn _unused(_: SettingValue) {}
