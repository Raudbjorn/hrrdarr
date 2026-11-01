//! RSS/Atom list (Sonarr "RSS List" / Radarr "RSS List Import"): the entries of a feed are the items.
//!
//! For each entry the title is parsed like a release title when it looks like one (`Show.Name.S01E01`
//! or `Movie.Name.2020.1080p`), otherwise used as is with a trailing `(YYYY)` split into the year.
//! An IMDb id (`tt1234567`) found in the entry's link, id or description becomes the identity.
//! The document is bounded in size and nesting; a document that is not RSS or Atom fails the fetch.
use super::*;
use crate::import_lists::model::ListItem;
use roxmltree::{Document, Node};

const MAX_DEPTH: usize = 24;

pub fn implementation() -> Implementation {
    Implementation {
        name: "rss",
        label: "RSS list",
        description: "Titles from an RSS or Atom feed; release-style titles are parsed to a name and year.",
        domains: &[MediaDomain::Tv, MediaDomain::Movies],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "url",
            "Feed URL",
            FieldKind::Url,
            "An RSS 2.0 or Atom feed.",
        )
        .required()
        .max_length(2048),
    ]
}

fn depth_ok(input: &str) -> bool {
    let (mut depth, mut max) = (0usize, 0usize);
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            match bytes.get(i + 1) {
                Some(b'/') => depth = depth.saturating_sub(1),
                Some(b'?') | Some(b'!') => {}
                _ => {
                    // A self-closing tag does not nest.
                    let end = input[i..].find('>').map_or(bytes.len(), |n| i + n);
                    if !input[i..end].ends_with('/') {
                        depth += 1;
                        max = max.max(depth);
                    }
                }
            }
        }
        i += 1;
    }
    max <= MAX_DEPTH
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .map(|c| {
            c.descendants()
                .filter(|d| d.is_text())
                .filter_map(|d| d.text())
                .collect::<String>()
        })
}
fn atom_link(node: Node<'_, '_>) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == "link")
        .and_then(|c| c.attribute("href").map(str::to_owned))
}

/// First `tt` + digits run in the text.
fn find_imdb(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b't'
            && bytes[i + 1] == b't'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            let digits = bytes[i + 2..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            let after = bytes.get(i + 2 + digits);
            if (6..=12).contains(&digits) && after.is_none_or(|b| !b.is_ascii_alphanumeric()) {
                return Some(text[i..i + 2 + digits].to_owned());
            }
        }
        i += 1;
    }
    None
}

/// Name and year from a feed title.
fn name_and_year(media: MediaDomain, raw: &str) -> (String, Option<i64>) {
    let tv = media == MediaDomain::Tv;
    if let Ok(parsed) = crate::search::parser::parse(raw, tv) {
        // A bare name parses to itself; a release title yields its leading name and year.
        let looks_like_release = parsed.numbering.is_some()
            || parsed.quality_name.is_some()
            || (parsed.year.is_some() && raw.contains(['.', '_']));
        if looks_like_release && !parsed.title.is_empty() {
            return (clean_title(&parsed.title), parsed.year);
        }
    }
    // "Name (2020)" or "Name [2020]".
    let trimmed = raw.trim();
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(rest) = trimmed.strip_suffix(close) {
            if let Some(idx) = rest.rfind(open) {
                let year = &rest[idx + 1..];
                if year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()) {
                    if let Ok(year) = year.parse::<i64>() {
                        return (clean_title(rest[..idx].trim()), Some(year));
                    }
                }
            }
        }
    }
    (clean_title(trimmed), None)
}

pub fn parse(media: MediaDomain, input: &str) -> Fetched {
    if input.len() > MAX_BODY_BYTES || !depth_ok(input) {
        return Err(FetchError::InvalidResponse);
    }
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 200_000,
        ..roxmltree::ParsingOptions::default()
    };
    let doc =
        Document::parse_with_options(input, options).map_err(|_| FetchError::InvalidResponse)?;
    let root = doc.root_element();
    let entries: Vec<Node<'_, '_>> = match root.tag_name().name() {
        "rss" | "RDF" => doc
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "item")
            .collect(),
        "feed" => root
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "entry")
            .collect(),
        _ => return Err(FetchError::InvalidResponse),
    };
    let mut items = Vec::with_capacity(entries.len().min(1024));
    for entry in entries {
        let Some(title) = child_text(entry, "title") else {
            continue;
        };
        let (name, year) = name_and_year(media, &title);
        let mut imdb = None;
        for candidate in [
            child_text(entry, "link"),
            atom_link(entry),
            child_text(entry, "guid"),
            child_text(entry, "id"),
            child_text(entry, "description"),
            child_text(entry, "summary"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(id) = find_imdb(&candidate) {
                imdb = Some(id);
                break;
            }
        }
        items.push(ListItem {
            tvdb_id: None,
            tmdb_id: None,
            imdb_id: imdb,
            title: name,
            year,
        });
    }
    finish(media, items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let url = list.need_text("url")?.to_owned();
    let body = get(transport, url, &[]).await?;
    let text = std::str::from_utf8(&body).map_err(|_| FetchError::InvalidResponse)?;
    parse(list.media, text)
}
