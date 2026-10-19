//! IMDb public list export: `https://www.imdb.com/list/{ls...}/export`, a CSV document.
//!
//! The header row must name at least `Const` and `Title`; `Year` and `Title Type` are optional. Rows
//! whose title type is television (TV series, mini series, episode) are skipped, everything else is a
//! movie. Each `Const` must be `tt` + digits. Anything that is not such a CSV (an HTML page, a login
//! wall) fails the fetch. Only public lists are reachable; no credentials are sent.
use super::*;
use crate::import_lists::model::ListItem;

const ORIGIN: &str = "https://www.imdb.com";
const MAX_LIST_ID_DIGITS: usize = 20;
const MAX_COLUMNS: usize = 128;
const MAX_RECORDS: usize = 2 * crate::import_lists::model::MAX_ITEMS;

pub fn implementation() -> Implementation {
    Implementation {
        name: "imdb",
        label: "IMDb list",
        description: "Movies from a public IMDb list (ls id) through its CSV export.",
        domains: &[MediaDomain::Movies],
        default_interval_minutes: 720,
        min_interval_minutes: 60,
        fields: fields,
        available: true,
    }
}
fn fields() -> Vec<Field> {
    vec![
        field(
            "list_id",
            "List ID",
            FieldKind::Text,
            "Public list id, ls followed by digits (e.g. ls012345678).",
        )
        .required()
        .max_length(32),
    ]
}

fn valid_list_id(id: &str) -> bool {
    id.strip_prefix("ls").is_some_and(|d| {
        (1..=MAX_LIST_ID_DIGITS).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit())
    })
}

/// RFC 4180 records: quoted fields with doubled quotes and embedded delimiters or line breaks, CRLF or
/// LF endings, a leading BOM, and no records for blank lines. Bounded in records and columns; a
/// stray character after a closing quote or an unterminated quoted field is an error.
pub fn parse_csv(input: &str) -> Result<Vec<Vec<String>>, FetchError> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        FieldStart,
        Unquoted,
        Quoted,
        AfterQuote,
    }
    fn push_field(record: &mut Vec<String>, field: &mut String) -> Result<(), FetchError> {
        if record.len() >= MAX_COLUMNS {
            return Err(FetchError::InvalidResponse);
        }
        record.push(std::mem::take(field));
        Ok(())
    }
    fn end_record(
        records: &mut Vec<Vec<String>>,
        record: &mut Vec<String>,
    ) -> Result<(), FetchError> {
        if records.len() >= MAX_RECORDS {
            return Err(FetchError::ResourceLimit);
        }
        records.push(std::mem::take(record));
        Ok(())
    }
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = State::FieldStart;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        let line_end = c == '\r' || c == '\n';
        if line_end && c == '\r' && chars.peek() == Some(&'\n') && state != State::Quoted {
            chars.next();
        }
        match state {
            State::FieldStart => match c {
                '"' => state = State::Quoted,
                ',' => push_field(&mut record, &mut field)?,
                _ if line_end => {
                    // A blank line carries no record.
                    if !record.is_empty() {
                        push_field(&mut record, &mut field)?;
                        end_record(&mut records, &mut record)?;
                    }
                }
                _ => {
                    field.push(c);
                    state = State::Unquoted;
                }
            },
            State::Unquoted => match c {
                ',' => {
                    push_field(&mut record, &mut field)?;
                    state = State::FieldStart;
                }
                _ if line_end => {
                    push_field(&mut record, &mut field)?;
                    end_record(&mut records, &mut record)?;
                    state = State::FieldStart;
                }
                _ => field.push(c),
            },
            State::Quoted => match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => state = State::AfterQuote,
                _ => field.push(c),
            },
            State::AfterQuote => match c {
                ',' => {
                    push_field(&mut record, &mut field)?;
                    state = State::FieldStart;
                }
                _ if line_end => {
                    push_field(&mut record, &mut field)?;
                    end_record(&mut records, &mut record)?;
                    state = State::FieldStart;
                }
                _ => return Err(FetchError::InvalidResponse),
            },
        }
    }
    match state {
        State::Quoted => return Err(FetchError::InvalidResponse),
        State::FieldStart if record.is_empty() => {}
        _ => {
            push_field(&mut record, &mut field)?;
            end_record(&mut records, &mut record)?;
        }
    }
    Ok(records)
}

fn column(header: &[String], name: &str) -> Option<usize> {
    header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case(name))
}
fn is_television(title_type: &str) -> bool {
    let t = title_type.to_ascii_lowercase();
    t.contains("tv series") || t.contains("tv mini series") || t.contains("tv episode")
}

pub fn parse(media: MediaDomain, input: &str) -> Fetched {
    let records = parse_csv(input)?;
    let (header, rows) = records.split_first().ok_or(FetchError::InvalidResponse)?;
    let (Some(const_at), Some(title_at)) = (column(header, "Const"), column(header, "Title"))
    else {
        return Err(FetchError::InvalidResponse);
    };
    let (year_at, type_at) = (column(header, "Year"), column(header, "Title Type"));
    let mut items = Vec::with_capacity(rows.len().min(1024));
    for row in rows {
        let cell = |at: usize| {
            row.get(at)
                .map(String::as_str)
                .ok_or(FetchError::InvalidResponse)
        };
        if let Some(at) = type_at {
            if is_television(cell(at)?) {
                continue;
            }
        }
        let imdb = cell(const_at)?.trim();
        let imdb = valid_imdb(imdb)
            .ok_or(FetchError::InvalidResponse)?
            .to_owned();
        let year = match year_at {
            Some(at) => match cell(at)?.trim() {
                "" => None,
                y => y
                    .parse::<i64>()
                    .ok()
                    .filter(|y| (1..=9999).contains(y))
                    .map(Some)
                    .ok_or(FetchError::InvalidResponse)?,
            },
            None => None,
        };
        items.push(ListItem {
            tvdb_id: None,
            tmdb_id: None,
            imdb_id: Some(imdb),
            title: clean_title(cell(title_at)?),
            year,
        });
    }
    finish(media, items)
}

pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let id = list.need_text("list_id")?;
    if !valid_list_id(id) {
        return Err(FetchError::InvalidSettings);
    }
    let body = get(transport, format!("{ORIGIN}/list/{id}/export"), &[]).await?;
    let text = std::str::from_utf8(&body).map_err(|_| FetchError::InvalidResponse)?;
    parse(list.media, text)
}
