//! AniList (TV only): a user's anime lists through the public GraphQL endpoint.
//!
//! `POST https://graphql.anilist.co` with a `MediaListCollection(userName, type: ANIME[, status])`
//! query; no authentication (public profiles only). The English title is preferred over the romaji
//! one; `startDate.year` is the year. Movies and music formats are skipped (a TV list). A GraphQL
//! `errors` array, or a null collection (unknown or private user), is `rejected`; a response of the
//! wrong shape is `invalid_response`. Items carry no TVDB id, so the sync resolves them by title.
use super::*;
use crate::import_lists::model::ListItem;
use serde_json::json;

const ORIGIN: &str = "https://graphql.anilist.co";
const STATUSES: &[&str] = &[
    "all",
    "current",
    "planning",
    "completed",
    "paused",
    "dropped",
    "repeating",
];
const MAX_LISTS: usize = 64;
const MAX_ENTRIES: usize = 20_000;
const SELECTION: &str =
    "lists { entries { media { title { romaji english } startDate { year } format } } }";

pub fn implementation() -> Implementation {
    Implementation {
        name: "anilist",
        label: "AniList",
        description: "An AniList user's anime lists filtered by status, read without authentication. AniList ids are not mapped to TVDB ids, so items are resolved by exact title.",
        domains: &[MediaDomain::Tv],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "username",
            "Username",
            FieldKind::Text,
            "The AniList user name (the profile must be public).",
        )
        .required()
        .max_length(64),
        field(
            "status",
            "Status",
            FieldKind::Select,
            "Which list status to import; all imports every status.",
        )
        .options(STATUSES)
        .with_default(text_default("current")),
    ]
}

/// The request body for a user and optional status.
pub fn request_body(username: &str, status: Option<&str>) -> Value {
    match status {
        Some(status) => json!({
            "query": format!("query ($userName: String, $status: MediaListStatus) {{ MediaListCollection(userName: $userName, type: ANIME, status: $status) {{ {SELECTION} }} }}"),
            "variables": {"userName": username, "status": status.to_ascii_uppercase()},
        }),
        None => json!({
            "query": format!("query ($userName: String) {{ MediaListCollection(userName: $userName, type: ANIME) {{ {SELECTION} }} }}"),
            "variables": {"userName": username},
        }),
    }
}

pub fn parse(document: &Value) -> Fetched {
    let root = document.as_object().ok_or(FetchError::InvalidResponse)?;
    match root.get("errors") {
        None | Some(Value::Null) => {}
        Some(Value::Array(errors)) if errors.is_empty() => {}
        Some(_) => return Err(FetchError::Rejected),
    }
    let data = match root.get("data") {
        Some(Value::Object(data)) => data,
        Some(Value::Null) | None => return Err(FetchError::Rejected),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let collection = match data.get("MediaListCollection") {
        Some(Value::Object(collection)) => collection,
        Some(Value::Null) | None => return Err(FetchError::Rejected),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    let lists = match collection.get("lists") {
        Some(Value::Array(lists)) => lists,
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(_) => return Err(FetchError::InvalidResponse),
    };
    if lists.len() > MAX_LISTS {
        return Err(FetchError::ResourceLimit);
    }
    let (mut items, mut seen) = (Vec::new(), 0usize);
    for list in lists {
        let entries = match list.get("entries") {
            Some(Value::Array(entries)) => entries,
            None | Some(Value::Null) => continue,
            Some(_) => return Err(FetchError::InvalidResponse),
        };
        seen += entries.len();
        if seen > MAX_ENTRIES {
            return Err(FetchError::ResourceLimit);
        }
        for entry in entries {
            let media = match entry.get("media") {
                Some(Value::Object(media)) => media,
                Some(Value::Null) => continue,
                _ => return Err(FetchError::InvalidResponse),
            };
            match media.get("format") {
                None | Some(Value::Null) => {}
                Some(Value::String(format)) if matches!(format.as_str(), "MOVIE" | "MUSIC") => {
                    continue;
                }
                Some(Value::String(_)) => {}
                Some(_) => return Err(FetchError::InvalidResponse),
            }
            let (english, romaji) = match media.get("title") {
                Some(Value::Object(t)) => (title_of(t.get("english")), title_of(t.get("romaji"))),
                None | Some(Value::Null) => (String::new(), String::new()),
                Some(_) => return Err(FetchError::InvalidResponse),
            };
            let year = match media.get("startDate") {
                None | Some(Value::Null) => None,
                Some(Value::Object(date)) => year_of(date.get("year"))?,
                Some(_) => return Err(FetchError::InvalidResponse),
            };
            items.push(ListItem {
                tvdb_id: None,
                tmdb_id: None,
                imdb_id: None,
                title: if english.is_empty() { romaji } else { english },
                year,
            });
        }
    }
    Ok(items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let username = list.need_text("username")?;
    let status = list.text("status").unwrap_or("current");
    if !STATUSES.contains(&status) {
        return Err(FetchError::InvalidSettings);
    }
    let body = request_body(username, (status != "all").then_some(status));
    let document = post_json(transport, ORIGIN.to_owned(), &[], body).await?;
    finish(list.media, parse(&document)?)
}
