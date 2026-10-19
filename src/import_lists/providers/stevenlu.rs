//! StevenLu's popular-movies list: a public JSON array of `{title, imdb_id, poster_url, ...}`.
//!
//! Items carry only the IMDb id and title (extra keys are ignored); the sync resolves them through
//! the metadata catalog. A present but malformed `imdb_id` fails the fetch; an entry with neither a
//! valid IMDb id nor a title is skipped.
use super::*;
use crate::import_lists::model::ListItem;

const DEFAULT_LINK: &str = "https://s3.amazonaws.com/popular-movies/movies.json";

pub fn implementation() -> Implementation {
    Implementation {
        name: "stevenlu",
        label: "StevenLu popular movies",
        description: "The community-curated popular movies list published by StevenLu (IMDb ids).",
        domains: &[MediaDomain::Movies],
        default_interval_minutes: 1440,
        min_interval_minutes: 60,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "link",
            "List URL",
            FieldKind::Url,
            "JSON array of movies with imdb_id and title.",
        )
        .required()
        .max_length(2048)
        .with_default(text_default(DEFAULT_LINK)),
    ]
}

pub fn parse(media: MediaDomain, document: &Value) -> Fetched {
    let array = document.as_array().ok_or(FetchError::InvalidResponse)?;
    let mut items = Vec::with_capacity(array.len().min(1024));
    for value in array {
        let object = value.as_object().ok_or(FetchError::InvalidResponse)?;
        items.push(ListItem {
            tvdb_id: None,
            tmdb_id: None,
            imdb_id: imdb_of(object.get("imdb_id"))?,
            title: title_of(object.get("title")),
            year: None,
        });
    }
    finish(media, items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let link = list.text("link").unwrap_or(DEFAULT_LINK).to_owned();
    let document = get_json(transport, link, &[]).await?;
    parse(list.media, &document)
}
