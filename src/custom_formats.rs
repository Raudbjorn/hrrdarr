//! Native, domain-scoped custom formats and bounded evaluation.
mod matching;
use crate::{
    api::MediaDomain,
    db::{
        Database,
        custom_formats::{Specification, decode_specifications},
    },
    qualities::{Error, Result, invalid},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, put},
};
use libsql::{Connection, params};
pub use matching::Score;
pub(crate) use matching::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub const MAX_FORMATS: usize = 128;
const MAX_CATALOG_BYTES: usize = 262144;
#[derive(Clone, Debug, Deserialize, Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CustomFormatInput")]
pub struct Input {
    pub name: String,
    pub include_when_renaming: bool,
    pub specifications: Vec<Specification>,
}
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
#[ts(rename = "CustomFormat")]
pub struct Format {
    pub id: i64,
    pub media_type: MediaDomain,
    #[serde(flatten)]
    pub definition: Input,
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "CustomFormatBulkInput")]
pub struct Bulk {
    pub ids: Vec<i64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub include_when_renaming: Option<bool>,
}
fn missing() -> Error {
    Error(
        StatusCode::NOT_FOUND,
        "custom_format_not_found",
        "Custom format does not exist in this media domain",
    )
}
fn domain(media: &str) -> Result<MediaDomain> {
    MediaDomain::parse(media).map_err(invalid)
}
fn id(raw: &str) -> Result<i64> {
    raw.parse()
        .ok()
        .filter(|v| (1..=9_007_199_254_740_991i64).contains(v))
        .ok_or_else(|| invalid("Invalid custom format id"))
}
pub fn router(db: Arc<Database>) -> Router {
    let mut router = Router::new();
    for media in ["tv", "movies"] {
        let prefix = format!("/api/v1/{media}/custom-formats");
        router = router.merge(
            Router::new()
                .route(&prefix, get(list).post(create))
                .route(&format!("{prefix}/schema"), get(schema))
                .route(
                    &format!("{prefix}/bulk"),
                    put(bulk_update).delete(bulk_delete),
                )
                .route(
                    &format!("{prefix}/{{id}}"),
                    get(read).put(replace).delete(delete),
                )
                .layer(Extension(media.to_owned()))
                .layer(DefaultBodyLimit::max(73728))
                .with_state(db.clone()),
        );
    }
    router
}
pub(crate) async fn catalog(c: &Connection, media: MediaDomain) -> Result<Vec<Format>> {
    let mut rows=c.query("SELECT id,name,include_when_renaming,specification_version,specifications_json FROM custom_formats WHERE media_type=? ORDER BY id LIMIT 129",[crate::search::domain(media)]).await?;
    let mut result = Vec::new();
    let mut bytes = 0;
    while let Some(row) = rows.next().await? {
        let json: String = row.get(4)?;
        bytes += json.len();
        if result.len() == MAX_FORMATS || bytes > MAX_CATALOG_BYTES {
            return Err(invalid("Custom format catalog exceeds evaluation limits"));
        }
        let specs = decode_specifications(media, row.get(3)?, &json).map_err(invalid)?;
        result.push(Format {
            id: row.get(0)?,
            media_type: media,
            definition: Input {
                name: row.get(1)?,
                include_when_renaming: row.get::<i64>(2)? == 1,
                specifications: specs,
            },
        });
    }
    Ok(result)
}
async fn list(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
) -> Result<Json<Vec<Format>>> {
    let c = db.connect().await?;
    Ok(Json(catalog(&c, domain(&media)?).await?))
}
async fn read(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(raw): Path<String>,
) -> Result<Json<Format>> {
    let id = id(&raw)?;
    let c = db.connect().await?;
    Ok(Json(
        catalog(&c, domain(&media)?)
            .await?
            .into_iter()
            .find(|f| f.id == id)
            .ok_or_else(missing)?,
    ))
}
pub(crate) async fn validate(media: MediaDomain, input: &Input) -> Result<String> {
    if input.name.trim().is_empty()
        || input.name.chars().count() > 100
        || input.name.chars().any(char::is_control)
    {
        return Err(invalid("Invalid custom format name"));
    }
    let json = serde_json::to_string(&input.specifications)
        .map_err(|_| invalid("Invalid custom format specifications"))?;
    let specs = decode_specifications(media, 1, &json).map_err(invalid)?;
    compile_specs(specs).await.map_err(|code| match code {
        "custom_format_busy" | "custom_format_timeout" | "custom_format_worker_failed" => Error(
            StatusCode::SERVICE_UNAVAILABLE,
            code,
            "Custom format validation is temporarily unavailable",
        ),
        _ => invalid(code),
    })?;
    Ok(json)
}
async fn save(db: &Database, media: MediaDomain, id: Option<i64>, input: Input) -> Result<Format> {
    let json = validate(media, &input).await?;
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = persist_validated(&tx, media, id, input, json).await;
    match outcome {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
/// Caller validates/compiles before acquiring its write transaction.
pub(crate) async fn persist_validated(
    conn: &Connection,
    media: MediaDomain,
    id: Option<i64>,
    input: Input,
    json: String,
) -> Result<Format> {
    let existing = catalog(conn, media).await?;
    if id.is_some_and(|id| !existing.iter().any(|f| f.id == id)) {
        return Err(missing());
    }
    if existing
        .iter()
        .any(|f| Some(f.id) != id && f.definition.name == input.name)
    {
        return Err(Error(
            StatusCode::CONFLICT,
            "custom_format_name_conflict",
            "Name already exists",
        ));
    }
    let bytes: usize = existing
        .iter()
        .filter(|f| Some(f.id) != id)
        .map(|f| {
            serde_json::to_string(&f.definition.specifications)
                .map(|s| s.len())
                .unwrap_or(MAX_CATALOG_BYTES)
        })
        .sum();
    if (id.is_none() && existing.len() >= MAX_FORMATS) || bytes + json.len() > MAX_CATALOG_BYTES {
        return Err(invalid("Custom format catalog exceeds evaluation limits"));
    }
    let id = if let Some(id) = id {
        conn.execute("UPDATE custom_formats SET name=?,include_when_renaming=?,specifications_json=? WHERE id=? AND media_type=?",params![input.name.clone(),i64::from(input.include_when_renaming),json,id,crate::search::domain(media)]).await?;
        id
    } else {
        conn.execute("INSERT INTO custom_formats(media_type,name,include_when_renaming,specifications_json)VALUES(?,?,?,?)",params![crate::search::domain(media),input.name.clone(),i64::from(input.include_when_renaming),json]).await?;
        conn.last_insert_rowid()
    };
    Ok(Format {
        id,
        media_type: media,
        definition: input,
    })
}

async fn create(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    input: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<(StatusCode, Json<Format>)> {
    let Json(input) = input.map_err(|_| invalid("Invalid custom format input"))?;
    Ok((
        StatusCode::CREATED,
        Json(save(&db, domain(&media)?, None, input).await?),
    ))
}
async fn replace(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(raw): Path<String>,
    input: std::result::Result<Json<Input>, JsonRejection>,
) -> Result<Json<Format>> {
    let Json(input) = input.map_err(|_| invalid("Invalid custom format input"))?;
    Ok(Json(
        save(&db, domain(&media)?, Some(id(&raw)?), input).await?,
    ))
}
async fn mutate(
    db: &Database,
    media: MediaDomain,
    input: Bulk,
    delete: bool,
) -> Result<Vec<Format>> {
    if input.ids.is_empty()
        || input.ids.len() > MAX_FORMATS
        || input
            .ids
            .iter()
            .any(|v| !(1..=9_007_199_254_740_991i64).contains(v))
        || input
            .ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != input.ids.len()
    {
        return Err(invalid(
            "Provide distinct positive format ids within the catalog limit",
        ));
    }
    let c = db.connect().await?;
    let tx = c
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    let outcome = async {
        let mut all = catalog(&tx, media).await?;
        if input.ids.iter().any(|id| !all.iter().any(|f| f.id == *id)) {
            return Err(missing());
        }
        for id in &input.ids {
            if delete {
                tx.execute("DELETE FROM custom_formats WHERE id=?", [*id])
                    .await?;
            } else if let Some(value) = input.include_when_renaming {
                tx.execute(
                    "UPDATE custom_formats SET include_when_renaming=? WHERE id=?",
                    params![i64::from(value), *id],
                )
                .await?;
            }
        }
        all.retain(|f| input.ids.contains(&f.id));
        if let Some(value) = input.include_when_renaming {
            for f in &mut all {
                f.definition.include_when_renaming = value;
            }
        }
        Ok(all)
    }
    .await;
    match outcome {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
async fn bulk_update(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    input: std::result::Result<Json<Bulk>, JsonRejection>,
) -> Result<Json<Vec<Format>>> {
    let Json(input) = input.map_err(|_| invalid("Invalid bulk input"))?;
    Ok(Json(mutate(&db, domain(&media)?, input, false).await?))
}
async fn bulk_delete(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    input: std::result::Result<Json<Bulk>, JsonRejection>,
) -> Result<StatusCode> {
    let Json(input) = input.map_err(|_| invalid("Invalid bulk input"))?;
    mutate(&db, domain(&media)?, input, true).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn delete(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
    Path(raw): Path<String>,
) -> Result<StatusCode> {
    mutate(
        &db,
        domain(&media)?,
        Bulk {
            ids: vec![id(&raw)?],
            include_when_renaming: None,
        },
        true,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "CustomFormatChoice")]
pub struct Choice {
    pub value: i32,
    pub label: String,
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "CustomFormatPreset")]
pub struct Preset {
    pub label: String,
    pub specification: Specification,
}
#[derive(Serialize, ts_rs::TS)]
#[ts(rename = "CustomFormatSchema")]
pub struct Schema {
    pub version: i32,
    pub media_type: MediaDomain,
    pub conditions: Vec<crate::db::custom_formats::Condition>,
    pub presets: std::collections::BTreeMap<String, Vec<Preset>>,
    pub choices: std::collections::BTreeMap<String, Vec<Choice>>,
    pub max_formats: usize,
    pub max_specifications: usize,
    pub regex: String,
}
async fn schema(
    State(db): State<Arc<Database>>,
    Extension(media): Extension<String>,
) -> Result<Json<Schema>> {
    use crate::db::custom_formats::Condition as C;
    let movies = media == "movies";
    let domain = if movies {
        MediaDomain::Movies
    } else {
        MediaDomain::Tv
    };
    let mut conditions = vec![
        C::ReleaseTitle {
            pattern: ".+".into(),
        },
        C::ReleaseGroup {
            pattern: ".+".into(),
        },
        C::Language {
            value: 1,
            except_language: false,
        },
        C::Size {
            min_gib: 0.0,
            max_gib: 1.0,
        },
        C::Source { value: 0 },
        C::Resolution { value: 0 },
        C::IndexerFlag { value: 1 },
    ];
    if movies {
        conditions.extend([
            C::Edition {
                pattern: ".+".into(),
            },
            C::Year {
                min: 1900,
                max: 2100,
            },
            C::QualityModifier { value: 0 },
        ]);
    } else {
        conditions.push(C::ReleaseType { value: 0 });
    }
    let mut choices = std::collections::BTreeMap::new();
    let make = |pairs: Vec<(i32, &str)>| {
        pairs
            .into_iter()
            .map(|(value, label)| Choice {
                value,
                label: label.into(),
            })
            .collect::<Vec<_>>()
    };
    choices.insert(
        "language".into(),
        make(crate::languages::language_choices(domain)),
    );
    let source = if movies {
        vec![
            "Unknown",
            "CAM",
            "Telesync",
            "Telecine",
            "Workprint",
            "DVD",
            "TV",
            "WEB-DL",
            "WEBRip",
            "Bluray",
        ]
    } else {
        vec![
            "Unknown",
            "Television",
            "Television raw",
            "WEB-DL",
            "WEBRip",
            "DVD",
            "Bluray",
            "Bluray raw",
        ]
    };
    choices.insert(
        "source".into(),
        make(
            source
                .into_iter()
                .enumerate()
                .map(|(i, n)| (i as i32, n))
                .collect(),
        ),
    );
    choices.insert(
        "resolution".into(),
        make(vec![
            (0, "Unknown"),
            (360, "360p"),
            (480, "480p"),
            (540, "540p"),
            (576, "576p"),
            (720, "720p"),
            (1080, "1080p"),
            (2160, "2160p"),
        ]),
    );
    let mut flags = vec![(1, "Freeleech"), (2, "Halfleech"), (4, "Double upload")];
    if movies {
        flags.extend([
            (8, "PTP Golden"),
            (16, "PTP Approved"),
            (32, "Internal"),
            (64, "AHD Internal (legacy)"),
            (128, "Scene"),
            (256, "75% download counted"),
            (512, "25% download counted"),
            (1024, "AHD user release (legacy)"),
            (2048, "Nuked"),
        ]);
        choices.insert(
            "quality_modifier".into(),
            make(vec![
                (0, "None"),
                (1, "Regional"),
                (2, "Screener"),
                (3, "Raw HD"),
                (4, "Bluray disk"),
                (5, "Remux"),
            ]),
        );
    } else {
        flags.extend([
            (8, "Internal"),
            (16, "Scene"),
            (32, "75% download counted"),
            (64, "25% download counted"),
            (128, "Nuked"),
            (256, "Subtitles"),
        ]);
        choices.insert(
            "release_type".into(),
            make(vec![
                (0, "Unknown"),
                (1, "Single episode"),
                (2, "Multiple episodes"),
                (3, "Season pack"),
            ]),
        );
    }
    choices.insert("indexer_flag".into(), make(flags));
    let mut presets = built_in_presets();
    for format in catalog(&db.connect().await?, domain).await? {
        for specification in format.definition.specifications {
            presets
                .entry(condition_kind(&specification.condition).into())
                .or_default()
                .push(Preset {
                    label: format!("{}: {}", format.definition.name, specification.name),
                    specification,
                });
        }
    }
    Ok(Json(Schema{version:1,media_type:domain,conditions,presets,choices,max_formats:MAX_FORMATS,max_specifications:64,regex:"fancy-regex 0.19.2; case insensitive; bounded AST/backtracking; unsupported syntax rejected".into()}))
}

fn condition_kind(condition: &crate::db::custom_formats::Condition) -> &'static str {
    use crate::db::custom_formats::Condition as C;
    match condition {
        C::ReleaseTitle { .. } => "release_title",
        C::ReleaseGroup { .. } => "release_group",
        C::Edition { .. } => "edition",
        C::Language { .. } => "language",
        C::Size { .. } => "size",
        C::Source { .. } => "source",
        C::Resolution { .. } => "resolution",
        C::QualityModifier { .. } => "quality_modifier",
        C::IndexerFlag { .. } => "indexer_flag",
        C::ReleaseType { .. } => "release_type",
        C::Year { .. } => "year",
    }
}

// Independently authored token patterns. Presets are editable starting points, not parser rules.
fn built_in_presets() -> std::collections::BTreeMap<String, Vec<Preset>> {
    // Codec and subtitle presets intentionally search substrings; the word preset does not.
    let definitions = [
        ("x264", r"h264|x264|h[.]264|x[.]264"),
        ("x265", r"hevc|h265|x265|h[.]265|x[.]265"),
        ("Simple Hardcoded Subs", r"sub"),
        ("Hardcoded Subs", r"(?:\w+sub(?:s)?|hc|subbed)(?!\w)"),
        (
            "Surround Sound",
            r"atmos|truehd|dts.?hd|dts.?es|dts.?x(?=$|\d)|(?:ddp|dd[+]|eac3).?[56789]",
        ),
        ("Preferred Words", r"(?<!\w)(?:framestor|sparks)(?!\w)"),
    ];
    [(
        "release_title".into(),
        definitions
            .into_iter()
            .map(|(name, pattern)| Preset {
                label: name.into(),
                specification: Specification {
                    name: name.into(),
                    negate: false,
                    required: false,
                    condition: crate::db::custom_formats::Condition::ReleaseTitle {
                        pattern: pattern.into(),
                    },
                },
            })
            .collect(),
    )]
    .into()
}
