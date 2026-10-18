//! AutoTagging (api.002): per-domain rules that add (and optionally remove) tags from library items
//! using stored catalog facts. A rule matches when every `required` specification matches and, if
//! any non-required specifications exist, at least one of them matches. A fact that is unknown
//! (NULL) never matches, even when the specification is negated: absence of evidence is not evidence.
//! Rules are applied inside the caller's transaction on add, metadata refresh and library update.
use crate::{
    api::MediaDomain,
    db::Database,
    qualities::{Error, Result, invalid},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::get,
};
use libsql::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

pub const MAX_RULES: usize = 256;
pub const MAX_SPECIFICATIONS: usize = 64;
pub const MAX_RULE_TAGS: usize = 64;
const MAX_VALUES: usize = 64;
const MAX_TEXT_BYTES: usize = 256;
const MAX_ID: i64 = 9_007_199_254_740_991;
const MAX_YEAR: i64 = 9999;
const MAX_RUNTIME_MINUTES: i64 = 10_080;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_ASSIGNED_TAGS: usize = crate::tags::MAX_ASSIGNMENTS;

const TV_STATUSES: [&str; 4] = ["continuing", "ended", "upcoming", "deleted"];
const MOVIE_STATUSES: [&str; 5] = ["tba", "announced", "in_cinemas", "released", "deleted"];
const SERIES_TYPES: [&str; 3] = ["standard", "daily", "anime"];
const AVAILABILITIES: [&str; 4] = ["tba", "announced", "in_cinemas", "released"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagTextValues")]
pub struct TextValues {
    pub values: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagIntegerValues")]
pub struct IntegerValues {
    pub values: Vec<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagRange", optional_fields = nullable)]
pub struct Range {
    pub min: Option<i64>,
    pub max: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagRootFolder")]
pub struct RootFolderValue {
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagFlag")]
pub struct Flag {
    pub value: bool,
}

/// Matching is case-insensitive for text; text lists match when any value equals the fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "implementation", rename_all = "snake_case")]
#[ts(rename = "AutoTagCondition")]
pub enum Condition {
    Genre(TextValues),
    Keyword(TextValues),
    Network(TextValues),
    Studio(TextValues),
    OriginalCountry(TextValues),
    Year(Range),
    Runtime(Range),
    RootFolder(RootFolderValue),
    OriginalLanguage(IntegerValues),
    QualityProfile(IntegerValues),
    SeriesType(TextValues),
    Status(TextValues),
    MinimumAvailability(TextValues),
    Monitored(Flag),
    Collection(IntegerValues),
}
impl Condition {
    fn implementation(&self) -> &'static str {
        match self {
            Self::Genre(_) => "genre",
            Self::Keyword(_) => "keyword",
            Self::Network(_) => "network",
            Self::Studio(_) => "studio",
            Self::OriginalCountry(_) => "original_country",
            Self::Year(_) => "year",
            Self::Runtime(_) => "runtime",
            Self::RootFolder(_) => "root_folder",
            Self::OriginalLanguage(_) => "original_language",
            Self::QualityProfile(_) => "quality_profile",
            Self::SeriesType(_) => "series_type",
            Self::Status(_) => "status",
            Self::MinimumAvailability(_) => "minimum_availability",
            Self::Monitored(_) => "monitored",
            Self::Collection(_) => "collection",
        }
    }
    fn allowed(&self, d: MediaDomain) -> bool {
        match self {
            Self::Network(_) | Self::OriginalCountry(_) | Self::SeriesType(_) => {
                d == MediaDomain::Tv
            }
            Self::Keyword(_)
            | Self::Studio(_)
            | Self::MinimumAvailability(_)
            | Self::Collection(_) => d == MediaDomain::Movies,
            _ => true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagSpecificationInput")]
pub struct SpecificationInput {
    pub name: String,
    pub negate: bool,
    pub required: bool,
    pub condition: Condition,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "AutoTagSpecification")]
pub struct Specification {
    pub id: i64,
    pub name: String,
    pub negate: bool,
    pub required: bool,
    pub condition: Condition,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "AutoTagInput")]
pub struct Input {
    pub name: String,
    pub remove_tags_automatically: bool,
    pub tag_ids: Vec<i64>,
    pub specifications: Vec<SpecificationInput>,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "AutoTag")]
pub struct Rule {
    pub id: i64,
    pub media_type: MediaDomain,
    pub name: String,
    pub remove_tags_automatically: bool,
    pub tag_ids: Vec<i64>,
    pub specifications: Vec<Specification>,
}

#[derive(Clone, Copy, Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "AutoTagFieldKind")]
pub enum FieldKind {
    TextList,
    IntegerList,
    Integer,
    Text,
    Boolean,
    EnumList,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "AutoTagSchemaField")]
pub struct SchemaField {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub options: Vec<&'static str>,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "AutoTagSchemaEntry")]
pub struct SchemaEntry {
    pub implementation: &'static str,
    pub label: &'static str,
    pub fields: Vec<SchemaField>,
}

fn field(
    name: &'static str,
    label: &'static str,
    kind: FieldKind,
    options: &[&'static str],
) -> SchemaField {
    SchemaField {
        name,
        label,
        kind,
        required: !matches!(name, "min" | "max"),
        options: options.to_vec(),
    }
}
/// Implementations offered for a domain; deliberately omits certification (not stored).
pub fn schema(d: MediaDomain) -> Vec<SchemaEntry> {
    use FieldKind::*;
    let statuses: &[&str] = if d == MediaDomain::Tv {
        &TV_STATUSES
    } else {
        &MOVIE_STATUSES
    };
    let mut all = vec![
        (
            "genre",
            "Genre",
            vec![field("values", "Genres", TextList, &[])],
        ),
        (
            "keyword",
            "Keyword",
            vec![field("values", "Keywords", TextList, &[])],
        ),
        (
            "network",
            "Network",
            vec![field("values", "Networks", TextList, &[])],
        ),
        (
            "studio",
            "Studio",
            vec![field("values", "Studios", TextList, &[])],
        ),
        (
            "original_country",
            "Original country",
            vec![field(
                "values",
                "ISO 3166 alpha-3 codes (for example ISL)",
                TextList,
                &[],
            )],
        ),
        (
            "year",
            "Year",
            vec![
                field("min", "From year", Integer, &[]),
                field("max", "To year", Integer, &[]),
            ],
        ),
        (
            "runtime",
            "Runtime (minutes)",
            vec![
                field("min", "Minimum", Integer, &[]),
                field("max", "Maximum", Integer, &[]),
            ],
        ),
        (
            "root_folder",
            "Root folder",
            vec![field("path", "Root folder path", Text, &[])],
        ),
        (
            "original_language",
            "Original language",
            vec![field("values", "Language IDs", IntegerList, &[])],
        ),
        (
            "quality_profile",
            "Quality profile",
            vec![field("values", "Quality profile IDs", IntegerList, &[])],
        ),
        (
            "series_type",
            "Series type",
            vec![field("values", "Series types", EnumList, &SERIES_TYPES)],
        ),
        (
            "status",
            "Status",
            vec![field("values", "Statuses", EnumList, statuses)],
        ),
        (
            "minimum_availability",
            "Minimum availability",
            vec![field("values", "Availability", EnumList, &AVAILABILITIES)],
        ),
        (
            "monitored",
            "Monitored",
            vec![field("value", "Monitored", Boolean, &[])],
        ),
        (
            "collection",
            "Collection",
            vec![field("values", "Collection TMDB IDs", IntegerList, &[])],
        ),
    ];
    all.retain(|(implementation, _, _)| {
        sample(implementation).is_some_and(|condition| condition.allowed(d))
    });
    all.into_iter()
        .map(|(implementation, label, fields)| SchemaEntry {
            implementation,
            label,
            fields,
        })
        .collect()
}
fn sample(implementation: &str) -> Option<Condition> {
    let text = || TextValues { values: vec![] };
    let ints = || IntegerValues { values: vec![] };
    let range = || Range {
        min: None,
        max: None,
    };
    Some(match implementation {
        "genre" => Condition::Genre(text()),
        "keyword" => Condition::Keyword(text()),
        "network" => Condition::Network(text()),
        "studio" => Condition::Studio(text()),
        "original_country" => Condition::OriginalCountry(text()),
        "year" => Condition::Year(range()),
        "runtime" => Condition::Runtime(range()),
        "root_folder" => Condition::RootFolder(RootFolderValue {
            path: String::new(),
        }),
        "original_language" => Condition::OriginalLanguage(ints()),
        "quality_profile" => Condition::QualityProfile(ints()),
        "series_type" => Condition::SeriesType(text()),
        "status" => Condition::Status(text()),
        "minimum_availability" => Condition::MinimumAvailability(text()),
        "monitored" => Condition::Monitored(Flag { value: true }),
        "collection" => Condition::Collection(ints()),
        _ => return None,
    })
}

fn clean_text(raw: &str, limit: usize) -> std::result::Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > limit
        || trimmed.chars().any(|c| c.is_control())
        || trimmed.contains('\0')
    {
        return Err("AutoTagging text must be 1 to 256 bytes without control characters");
    }
    Ok(trimmed.to_owned())
}
fn clean_values(
    values: &[String],
    allowed: Option<&[&str]>,
) -> std::result::Result<Vec<String>, &'static str> {
    if values.is_empty() || values.len() > MAX_VALUES {
        return Err("AutoTagging conditions require 1 to 64 values");
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        let value = clean_text(value, MAX_TEXT_BYTES)?;
        let key = value.to_lowercase();
        if let Some(allowed) = allowed {
            if !allowed.contains(&key.as_str()) {
                return Err("AutoTagging condition value is not supported in this media domain");
            }
        }
        if seen.insert(key.clone()) {
            out.push(if allowed.is_some() { key } else { value });
        }
    }
    Ok(out)
}
fn clean_ints(values: &[i64]) -> std::result::Result<Vec<i64>, &'static str> {
    if values.is_empty()
        || values.len() > MAX_VALUES
        || values.iter().any(|v| !(0..=MAX_ID).contains(v))
    {
        return Err("AutoTagging conditions require 1 to 64 non-negative integer values");
    }
    Ok(values
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
fn clean_range(r: &Range, max: i64, min_floor: i64) -> std::result::Result<Range, &'static str> {
    if r.min.is_none() && r.max.is_none() {
        return Err("Range conditions require a minimum, a maximum or both");
    }
    for v in [r.min, r.max].into_iter().flatten() {
        if !(min_floor..=max).contains(&v) {
            return Err("Range condition bound is out of range");
        }
    }
    if let (Some(a), Some(b)) = (r.min, r.max) {
        if a > b {
            return Err("Range minimum exceeds its maximum");
        }
    }
    Ok(r.clone())
}
fn normalize_condition(
    d: MediaDomain,
    condition: &Condition,
) -> std::result::Result<Condition, &'static str> {
    if !condition.allowed(d) {
        return Err("This AutoTagging condition is not available in this media domain");
    }
    let statuses: &[&str] = if d == MediaDomain::Tv {
        &TV_STATUSES
    } else {
        &MOVIE_STATUSES
    };
    Ok(match condition {
        Condition::Genre(v) => Condition::Genre(TextValues {
            values: clean_values(&v.values, None)?,
        }),
        Condition::Keyword(v) => Condition::Keyword(TextValues {
            values: clean_values(&v.values, None)?,
        }),
        Condition::Network(v) => Condition::Network(TextValues {
            values: clean_values(&v.values, None)?,
        }),
        Condition::Studio(v) => Condition::Studio(TextValues {
            values: clean_values(&v.values, None)?,
        }),
        Condition::OriginalCountry(v) => {
            let values = clean_values(&v.values, None)?
                .into_iter()
                .map(|c| c.to_ascii_uppercase())
                .collect::<Vec<_>>();
            if values
                .iter()
                .any(|c| c.len() != 3 || !c.bytes().all(|b| b.is_ascii_uppercase()))
            {
                return Err("Original country values are three-letter ISO codes");
            }
            Condition::OriginalCountry(TextValues { values })
        }
        Condition::Year(r) => Condition::Year(clean_range(r, MAX_YEAR, 1)?),
        Condition::Runtime(r) => Condition::Runtime(clean_range(r, MAX_RUNTIME_MINUTES, 0)?),
        Condition::RootFolder(v) => {
            let path = clean_text(&v.path, 4096)?;
            let path = path.trim_end_matches('/').to_owned();
            if !path.starts_with('/') || path.len() < 2 {
                return Err("Root folder conditions require an absolute root folder path");
            }
            Condition::RootFolder(RootFolderValue { path })
        }
        Condition::OriginalLanguage(v) => Condition::OriginalLanguage(IntegerValues {
            values: clean_ints(&v.values)?,
        }),
        Condition::QualityProfile(v) => Condition::QualityProfile(IntegerValues {
            values: clean_ints(&v.values)?,
        }),
        Condition::Collection(v) => Condition::Collection(IntegerValues {
            values: clean_ints(&v.values)?,
        }),
        Condition::SeriesType(v) => Condition::SeriesType(TextValues {
            values: clean_values(&v.values, Some(&SERIES_TYPES))?,
        }),
        Condition::Status(v) => Condition::Status(TextValues {
            values: clean_values(&v.values, Some(statuses))?,
        }),
        Condition::MinimumAvailability(v) => Condition::MinimumAvailability(TextValues {
            values: clean_values(&v.values, Some(&AVAILABILITIES))?,
        }),
        Condition::Monitored(v) => Condition::Monitored(v.clone()),
    })
}

/// Validated, normalized input. Tag existence is checked against storage by the writer.
pub(crate) fn normalize(d: MediaDomain, input: &Input) -> Result<Input> {
    let name = clean_text(&input.name, MAX_TEXT_BYTES).map_err(invalid)?;
    if input.tag_ids.is_empty()
        || input.tag_ids.len() > MAX_RULE_TAGS
        || input.tag_ids.iter().any(|id| !(1..=MAX_ID).contains(id))
    {
        return Err(invalid("AutoTagging rules require 1 to 64 tag IDs"));
    }
    if input.specifications.is_empty() || input.specifications.len() > MAX_SPECIFICATIONS {
        return Err(invalid("AutoTagging rules require 1 to 64 specifications"));
    }
    let mut specifications = Vec::new();
    for spec in &input.specifications {
        specifications.push(SpecificationInput {
            name: clean_text(&spec.name, MAX_TEXT_BYTES).map_err(invalid)?,
            negate: spec.negate,
            required: spec.required,
            condition: normalize_condition(d, &spec.condition).map_err(invalid)?,
        });
    }
    Ok(Input {
        name,
        remove_tags_automatically: input.remove_tags_automatically,
        tag_ids: input
            .tag_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        specifications,
    })
}

/// Stored catalog facts for one library item. `None` is unknown, never "empty".
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub genres: Option<Vec<String>>,
    pub keywords: Option<Vec<String>>,
    pub network: Option<String>,
    pub studio: Option<String>,
    pub original_country: Option<String>,
    pub year: Option<i64>,
    pub runtime: Option<i64>,
    pub root_folder: Option<String>,
    pub original_language: Option<i64>,
    pub quality_profile_id: Option<i64>,
    pub series_type: Option<String>,
    pub status: Option<String>,
    pub minimum_availability: Option<String>,
    pub monitored: Option<bool>,
    pub collection_ids: Option<Vec<i64>>,
}
fn any_text(values: &[String], fact: &Option<Vec<String>>) -> Option<bool> {
    let fact = fact.as_ref()?;
    Some(
        fact.iter()
            .any(|f| values.iter().any(|v| v.to_lowercase() == f.to_lowercase())),
    )
}
fn one_text(values: &[String], fact: &Option<String>) -> Option<bool> {
    let fact = fact.as_ref()?.to_lowercase();
    Some(values.iter().any(|v| v.to_lowercase() == fact))
}
fn in_range(r: &Range, fact: Option<i64>) -> Option<bool> {
    let fact = fact?;
    Some(r.min.is_none_or(|m| fact >= m) && r.max.is_none_or(|m| fact <= m))
}
/// Raw (pre-negation) outcome; `None` means the needed fact is unknown.
fn raw(condition: &Condition, facts: &Facts) -> Option<bool> {
    match condition {
        Condition::Genre(v) => any_text(&v.values, &facts.genres),
        Condition::Keyword(v) => any_text(&v.values, &facts.keywords),
        Condition::Network(v) => one_text(&v.values, &facts.network),
        Condition::Studio(v) => one_text(&v.values, &facts.studio),
        Condition::OriginalCountry(v) => one_text(&v.values, &facts.original_country),
        Condition::SeriesType(v) => one_text(&v.values, &facts.series_type),
        Condition::Status(v) => one_text(&v.values, &facts.status),
        Condition::MinimumAvailability(v) => one_text(&v.values, &facts.minimum_availability),
        Condition::Year(r) => in_range(r, facts.year),
        Condition::Runtime(r) => in_range(r, facts.runtime),
        Condition::RootFolder(v) => facts.root_folder.as_ref().map(|f| *f == v.path),
        Condition::OriginalLanguage(v) => facts
            .original_language
            .map(|language| v.values.contains(&language)),
        Condition::QualityProfile(v) => facts
            .quality_profile_id
            .map(|profile| v.values.contains(&profile)),
        Condition::Collection(v) => facts
            .collection_ids
            .as_ref()
            .map(|ids| ids.iter().any(|id| v.values.contains(id))),
        Condition::Monitored(v) => facts.monitored.map(|m| m == v.value),
    }
}
fn spec_matches(spec: &Specification, facts: &Facts) -> bool {
    raw(&spec.condition, facts).is_some_and(|hit| hit != spec.negate)
}
pub fn rule_matches(rule: &Rule, facts: &Facts) -> bool {
    if rule.specifications.is_empty() {
        return false;
    }
    let (required, optional): (Vec<_>, Vec<_>) =
        rule.specifications.iter().partition(|s| s.required);
    required.iter().all(|s| spec_matches(s, facts))
        && (optional.is_empty() || optional.iter().any(|s| spec_matches(s, facts)))
}
/// Final tag set after applying all rules to the existing assignment (add, then remove losers).
pub fn plan(rules: &[Rule], facts: &Facts, existing: &BTreeSet<i64>) -> BTreeSet<i64> {
    let mut add = BTreeSet::new();
    let mut remove = BTreeSet::new();
    for rule in rules {
        if rule_matches(rule, facts) {
            add.extend(rule.tag_ids.iter().copied());
        } else if rule.remove_tags_automatically {
            remove.extend(rule.tag_ids.iter().copied());
        }
    }
    let mut next = existing.clone();
    next.extend(add.iter().copied());
    for id in remove.difference(&add) {
        next.remove(id);
    }
    next
}

fn storage_invariant() -> Error {
    Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "autotagging_invariant",
        "Stored AutoTagging rule is invalid",
    )
}
fn json_set(raw: Option<String>) -> Result<Option<Vec<String>>> {
    raw.map(|json| serde_json::from_str::<Vec<String>>(&json).map_err(|_| storage_invariant()))
        .transpose()
}
async fn load_facts(c: &Connection, d: MediaDomain, id: i64) -> Result<Option<Facts>> {
    let sql = if d == MediaDomain::Tv {
        "SELECT r.genres_json,NULL,r.network,NULL,r.original_country,r.year,r.runtime,r.path,r.original_language,s.quality_profile_id,s.series_type,r.status,NULL,r.monitored,NULL FROM series r LEFT JOIN library_settings s ON s.series_id=r.id AND s.media_type='tv' WHERE r.id=?"
    } else {
        "SELECT m.genres_json,m.keywords_json,NULL,m.studio,NULL,m.year,m.runtime,r.path,m.original_language,s.quality_profile_id,NULL,m.status,s.minimum_availability,r.monitored,r.metadata_id FROM movies r JOIN movie_metadata m ON m.id=r.metadata_id LEFT JOIN library_settings s ON s.movie_id=r.id AND s.media_type='movies' WHERE r.id=?"
    };
    let Some(row) = c.query(sql, [id]).await?.next().await? else {
        return Ok(None);
    };
    let path: String = row.get(7)?;
    let mut facts = Facts {
        genres: json_set(row.get(0)?)?,
        keywords: json_set(row.get(1)?)?,
        network: row.get(2)?,
        studio: row.get(3)?,
        original_country: row.get(4)?,
        year: row.get(5)?,
        runtime: row.get(6)?,
        original_language: row.get(8)?,
        quality_profile_id: row.get(9)?,
        series_type: row.get(10)?,
        status: row.get(11)?,
        minimum_availability: row.get(12)?,
        monitored: Some(row.get::<i64>(13)? != 0),
        ..Facts::default()
    };
    let metadata_id: Option<i64> = row.get(14)?;
    drop(row);
    if let Some(metadata_id) = metadata_id {
        let mut rows = c
            .query(
                "SELECT c.tmdb_id FROM movie_collection_members m JOIN movie_collections c ON c.id=m.collection_id WHERE m.metadata_id=? AND c.tmdb_id IS NOT NULL ORDER BY c.tmdb_id LIMIT 16",
                [metadata_id],
            )
            .await?;
        let mut ids = vec![];
        while let Some(r) = rows.next().await? {
            ids.push(r.get(0)?);
        }
        // Absence of a stored membership is unknown (it may simply not be fetched yet), not "none".
        facts.collection_ids = (!ids.is_empty()).then_some(ids);
    }
    let mut rows = c
        .query(
            "SELECT path FROM root_folders WHERE media_type=? ORDER BY length(path) DESC,id",
            [names(d)],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        let root: String = r.get(0)?;
        if path == root || path.starts_with(&format!("{root}/")) {
            facts.root_folder = Some(root);
            break;
        }
    }
    Ok(Some(facts))
}
fn names(d: MediaDomain) -> &'static str {
    match d {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
pub(crate) async fn load_rules(
    c: &Connection,
    d: MediaDomain,
    only: Option<i64>,
) -> Result<Vec<Rule>> {
    let mut rows = c
        .query(
            "SELECT id,name,remove_tags_automatically FROM autotagging_rules WHERE media_type=?1 AND (?2 IS NULL OR id=?2) ORDER BY name,id LIMIT 257",
            params![names(d), only],
        )
        .await?;
    let mut rules = vec![];
    while let Some(r) = rows.next().await? {
        if rules.len() == MAX_RULES {
            return Err(storage_invariant());
        }
        rules.push(Rule {
            id: r.get(0)?,
            media_type: d,
            name: r.get(1)?,
            remove_tags_automatically: r.get::<i64>(2)? != 0,
            tag_ids: vec![],
            specifications: vec![],
        });
    }
    drop(rows);
    for rule in &mut rules {
        let mut rows = c
            .query(
                "SELECT tag_id FROM autotagging_rule_tags WHERE rule_id=? ORDER BY tag_id LIMIT 65",
                [rule.id],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            rule.tag_ids.push(r.get(0)?);
        }
        drop(rows);
        let mut rows = c
            .query(
                "SELECT id,name,negate,required,condition_json FROM autotagging_specifications WHERE rule_id=? ORDER BY position LIMIT 65",
                [rule.id],
            )
            .await?;
        while let Some(r) = rows.next().await? {
            rule.specifications.push(Specification {
                id: r.get(0)?,
                name: r.get(1)?,
                negate: r.get::<i64>(2)? != 0,
                required: r.get::<i64>(3)? != 0,
                condition: serde_json::from_str(&r.get::<String>(4)?)
                    .map_err(|_| storage_invariant())?,
            });
        }
        if rule.tag_ids.len() > MAX_RULE_TAGS || rule.specifications.len() > MAX_SPECIFICATIONS {
            return Err(storage_invariant());
        }
    }
    Ok(rules)
}

/// Applies every rule of the item's domain inside the caller's transaction. Returns whether the
/// tag assignment changed. Automatic assignments are not recorded as local user edits.
pub(crate) async fn apply_item(c: &Connection, d: MediaDomain, id: i64) -> Result<bool> {
    let rules = load_rules(c, d, None).await?;
    if rules.is_empty() {
        return Ok(false);
    }
    let Some(facts) = load_facts(c, d, id).await? else {
        return Ok(false);
    };
    let existing = crate::tags::assigned(c, d, id)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let next = plan(&rules, &facts, &existing);
    if next == existing {
        return Ok(false);
    }
    if next.len() > MAX_ASSIGNED_TAGS {
        return Err(invalid(
            "AutoTagging would exceed the 200 tag assignment limit",
        ));
    }
    crate::tags::assign(
        c,
        d,
        id,
        &crate::tags::Assignment {
            mode: crate::tags::Mode::Replace,
            ids: next.into_iter().collect(),
        },
        false,
    )
    .await?;
    Ok(true)
}

fn domain(s: &str) -> Result<MediaDomain> {
    MediaDomain::parse(s).map_err(invalid)
}
fn rule_id(s: &str) -> Result<i64> {
    s.parse::<i64>()
        .ok()
        .filter(|id| (1..=MAX_ID).contains(id))
        .ok_or_else(|| invalid("Invalid AutoTagging rule ID"))
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "autotagging_not_found",
        "AutoTagging rule does not exist in this media domain",
    )
}
fn conflict() -> Error {
    Error(
        StatusCode::CONFLICT,
        "autotagging_conflict",
        "An AutoTagging rule with this name already exists",
    )
}
fn body(v: std::result::Result<Json<Input>, JsonRejection>) -> Result<Input> {
    v.map(|v| v.0)
        .map_err(|_| invalid("Expected a valid AutoTagging rule object"))
}

pub fn router(db: Arc<Database>) -> Router {
    let mut r = Router::new();
    for d in ["tv", "movies"] {
        let p = format!("/api/v1/{d}/autotagging");
        r = r.merge(
            Router::new()
                .route(&p, get(list).post(create))
                .route(&format!("{p}/schema"), get(schema_route))
                .route(
                    &format!("{p}/{{id}}"),
                    get(read).put(replace).delete(remove),
                )
                .layer(Extension(d.to_owned()))
                .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
                .with_state(db.clone()),
        );
    }
    r
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
) -> Result<Json<Vec<Rule>>> {
    Ok(Json(
        load_rules(&db.connect().await?, domain(&m)?, None).await?,
    ))
}
async fn schema_route(Extension(m): Extension<String>) -> Result<Json<Vec<SchemaEntry>>> {
    Ok(Json(schema(domain(&m)?)))
}
async fn read(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
) -> Result<Json<Rule>> {
    let d = domain(&m)?;
    load_rules(&db.connect().await?, d, Some(rule_id(&raw)?))
        .await?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(missing)
}
async fn write_rule(tx: &Connection, d: MediaDomain, id: i64, input: &Input) -> Result<()> {
    for tag in &input.tag_ids {
        if tx
            .query(
                "SELECT 1 FROM tags WHERE id=? AND media_type=?",
                params![*tag, names(d)],
            )
            .await?
            .next()
            .await?
            .is_none()
        {
            return Err(invalid("Tag does not exist in this media domain"));
        }
        tx.execute(
            "INSERT INTO autotagging_rule_tags(rule_id,media_type,tag_id)VALUES(?,?,?)",
            params![id, names(d), *tag],
        )
        .await?;
    }
    for (position, spec) in input.specifications.iter().enumerate() {
        let json = serde_json::to_string(&spec.condition).map_err(|_| storage_invariant())?;
        tx.execute(
            "INSERT INTO autotagging_specifications(rule_id,position,name,negate,required,condition_json)VALUES(?,?,?,?,?,?)",
            params![
                id,
                position as i64,
                spec.name.clone(),
                i64::from(spec.negate),
                i64::from(spec.required),
                json
            ],
        )
        .await?;
    }
    Ok(())
}
async fn name_taken(tx: &Connection, d: MediaDomain, name: &str, except: i64) -> Result<bool> {
    Ok(tx
        .query(
            "SELECT 1 FROM autotagging_rules WHERE media_type=? AND name=? AND id<>?",
            params![names(d), name, except],
        )
        .await?
        .next()
        .await?
        .is_some())
}
async fn create(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    v: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<(StatusCode, Json<Rule>)> {
    let d = domain(&m)?;
    let input = normalize(d, &body(v)?)?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if name_taken(&tx, d, &input.name, 0).await? {
            return Err(conflict());
        }
        let count = tx
            .query(
                "SELECT count(*) FROM autotagging_rules WHERE media_type=?",
                [names(d)],
            )
            .await?
            .next()
            .await?
            .ok_or_else(missing)?
            .get::<i64>(0)?;
        if count as usize >= MAX_RULES {
            return Err(invalid("AutoTagging rule limit reached"));
        }
        tx.execute(
            "INSERT INTO autotagging_rules(media_type,name,remove_tags_automatically)VALUES(?,?,?)",
            params![
                names(d),
                input.name.clone(),
                i64::from(input.remove_tags_automatically)
            ],
        )
        .await?;
        let id = tx.last_insert_rowid();
        write_rule(&tx, d, id, &input).await?;
        load_rules(&tx, d, Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(missing)
    }
    .await;
    match outcome {
        Ok(rule) => {
            tx.commit().await?;
            Ok((StatusCode::CREATED, Json(rule)))
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
async fn replace(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
    v: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<Json<Rule>> {
    let d = domain(&m)?;
    let id = rule_id(&raw)?;
    let input = normalize(d, &body(v)?)?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        if load_rules(&tx, d, Some(id)).await?.is_empty() {
            return Err(missing());
        }
        if name_taken(&tx, d, &input.name, id).await? {
            return Err(conflict());
        }
        tx.execute(
            "UPDATE autotagging_rules SET name=?,remove_tags_automatically=? WHERE id=?",
            params![
                input.name.clone(),
                i64::from(input.remove_tags_automatically),
                id
            ],
        )
        .await?;
        tx.execute("DELETE FROM autotagging_rule_tags WHERE rule_id=?", [id])
            .await?;
        tx.execute(
            "DELETE FROM autotagging_specifications WHERE rule_id=?",
            [id],
        )
        .await?;
        write_rule(&tx, d, id, &input).await?;
        load_rules(&tx, d, Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(missing)
    }
    .await;
    match outcome {
        Ok(rule) => {
            tx.commit().await?;
            Ok(Json(rule))
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
async fn remove(
    State(db): State<Arc<Database>>,
    Extension(m): Extension<String>,
    Path(raw): Path<String>,
) -> Result<StatusCode> {
    let d = domain(&m)?;
    let id = rule_id(&raw)?;
    let c = db.connect().await?;
    let changed = c
        .execute(
            "DELETE FROM autotagging_rules WHERE id=? AND media_type=?",
            params![id, names(d)],
        )
        .await?;
    if changed == 0 {
        return Err(missing());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(remove: bool, tags: &[i64], specs: Vec<(bool, bool, Condition)>) -> Rule {
        Rule {
            id: 1,
            media_type: MediaDomain::Movies,
            name: "r".into(),
            remove_tags_automatically: remove,
            tag_ids: tags.to_vec(),
            specifications: specs
                .into_iter()
                .enumerate()
                .map(|(i, (negate, required, condition))| Specification {
                    id: i as i64,
                    name: "s".into(),
                    negate,
                    required,
                    condition,
                })
                .collect(),
        }
    }
    fn genre(v: &[&str]) -> Condition {
        Condition::Genre(TextValues {
            values: v.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn required_and_optional_semantics() {
        let facts = Facts {
            genres: Some(vec!["Drama".into()]),
            year: Some(1999),
            ..Facts::default()
        };
        let year = |a, b| {
            Condition::Year(Range {
                min: Some(a),
                max: Some(b),
            })
        };
        // required drama AND (optional: any of year 2000+ or year 1990s)
        let r = rule(
            false,
            &[1],
            vec![
                (false, true, genre(&["drama"])),
                (false, false, year(2000, 2100)),
                (false, false, year(1990, 1999)),
            ],
        );
        assert!(rule_matches(&r, &facts));
        let r = rule(false, &[1], vec![(false, true, genre(&["horror"]))]);
        assert!(!rule_matches(&r, &facts));
        let r = rule(
            false,
            &[1],
            vec![
                (false, true, genre(&["drama"])),
                (false, false, year(2000, 2100)),
            ],
        );
        assert!(!rule_matches(&r, &facts), "an optional group needs one hit");
        let r = rule(false, &[1], vec![(true, true, genre(&["horror"]))]);
        assert!(rule_matches(&r, &facts), "negation flips a known fact");
        assert!(!rule_matches(&rule(false, &[1], vec![]), &facts));
    }

    #[test]
    fn unknown_facts_never_match_even_when_negated() {
        let facts = Facts::default();
        for negate in [false, true] {
            let r = rule(false, &[1], vec![(negate, true, genre(&["drama"]))]);
            assert!(!rule_matches(&r, &facts));
        }
        let empty_known = Facts {
            genres: Some(vec![]),
            ..Facts::default()
        };
        let r = rule(false, &[1], vec![(true, true, genre(&["drama"]))]);
        assert!(rule_matches(&r, &empty_known));
    }

    #[test]
    fn plan_adds_removes_and_protects_manual_tags() {
        let facts = Facts {
            genres: Some(vec!["Anime".into()]),
            ..Facts::default()
        };
        let hit = rule(true, &[1], vec![(false, true, genre(&["anime"]))]);
        let miss = rule(true, &[2], vec![(false, true, genre(&["horror"]))]);
        let miss_keep = rule(false, &[3], vec![(false, true, genre(&["horror"]))]);
        let existing = [2, 3, 9].into_iter().collect();
        let next = plan(&[hit, miss, miss_keep], &facts, &existing);
        assert_eq!(next, [1, 3, 9].into_iter().collect());
        // A tag that one rule adds is never removed by another rule's non-match.
        let a = rule(false, &[5], vec![(false, true, genre(&["anime"]))]);
        let b = rule(true, &[5], vec![(false, true, genre(&["horror"]))]);
        assert!(plan(&[a, b], &facts, &BTreeSet::new()).contains(&5));
    }

    #[test]
    fn domain_scoped_conditions_and_normalization() {
        let input = |condition| Input {
            name: " Rule ".into(),
            remove_tags_automatically: false,
            tag_ids: vec![2, 1, 2],
            specifications: vec![SpecificationInput {
                name: "s".into(),
                negate: false,
                required: true,
                condition,
            }],
        };
        let network = Condition::Network(TextValues {
            values: vec!["BBC".into()],
        });
        assert!(normalize(MediaDomain::Movies, &input(network.clone())).is_err());
        let ok = normalize(MediaDomain::Tv, &input(network)).unwrap();
        assert_eq!(ok.name, "Rule");
        assert_eq!(ok.tag_ids, [1, 2]);
        let studio = Condition::Studio(TextValues {
            values: vec!["A24".into()],
        });
        assert!(normalize(MediaDomain::Tv, &input(studio)).is_err());
        let bad_range = Condition::Year(Range {
            min: Some(2000),
            max: Some(1990),
        });
        assert!(normalize(MediaDomain::Tv, &input(bad_range)).is_err());
        let status = Condition::Status(TextValues {
            values: vec!["Released".into()],
        });
        assert!(normalize(MediaDomain::Tv, &input(status.clone())).is_err());
        assert!(normalize(MediaDomain::Movies, &input(status)).is_ok());
        let country = Condition::OriginalCountry(TextValues {
            values: vec!["isl".into()],
        });
        assert!(matches!(
            normalize(MediaDomain::Tv, &input(country)).unwrap().specifications[0].condition,
            Condition::OriginalCountry(ref v) if v.values == ["ISL"]
        ));
    }

    #[test]
    fn schema_is_domain_scoped_and_has_no_certification() {
        let tv: Vec<_> = schema(MediaDomain::Tv)
            .iter()
            .map(|e| e.implementation)
            .collect();
        let movies: Vec<_> = schema(MediaDomain::Movies)
            .iter()
            .map(|e| e.implementation)
            .collect();
        assert!(tv.contains(&"network") && !tv.contains(&"studio"));
        assert!(movies.contains(&"studio") && movies.contains(&"collection"));
        assert!(!movies.contains(&"network") && !tv.contains(&"certification"));
    }
}
