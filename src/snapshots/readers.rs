use super::*;
use std::collections::BTreeSet;

fn table<'a>(source: &'a Source, name: &str, required: &[&str]) -> Result<&'a Table> {
    let table = source
        .tables
        .get(name)
        .ok_or(ImportError("required source table is absent"))?;
    if required
        .iter()
        .any(|c| !table.columns.iter().any(|actual| actual == c))
    {
        return Err(ImportError("source schema does not match supported layout"));
    }
    let mut ids = BTreeSet::new();
    for row in &table.rows {
        if !ids.insert(positive(row, "Id")?) {
            return Err(ImportError("duplicate source identity"));
        }
    }
    Ok(table)
}
fn val(v: impl Into<Value>) -> Field {
    Field::Value(v.into())
}
fn nullable(row: &Record, col: &str) -> Result<Field> {
    let v = row
        .get(col)
        .ok_or(ImportError("required source column is absent"))?;
    match (col, v) {
        ("Year", Value::Null | Value::Integer(_)) => Ok(Field::Value(v.clone())),
        ("ImdbId" | "Edition", Value::Null) => Ok(Field::Value(Value::Null)),
        ("ImdbId" | "Edition", Value::Text(s)) if s.trim().is_empty() => {
            Ok(Field::Value(Value::Null))
        }
        ("ImdbId" | "Edition", Value::Text(s)) if !s.contains('\0') => Ok(Field::Value(v.clone())),
        _ => Err(ImportError("invalid source optional field")),
    }
}
fn root(path: &str) -> Result<String> {
    // Paths are retained in the source namespace. No Windows-to-Unix conversion is guessed.
    if !path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(ImportError(
            "only absolute POSIX library roots without traversal are supported",
        ));
    }
    Ok(format!(
        "/{}",
        path.split('/')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    ))
}
fn file_path(base: &str, relative: &str) -> Result<String> {
    if relative.starts_with('/')
        || relative.contains('\\')
        || relative.contains(':')
        || relative
            .split('/')
            .any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err(ImportError(
            "invalid relative media path; absolute, Windows and traversal paths are unsupported",
        ));
    }
    Ok(format!("{}/{relative}", root(base)?.trim_end_matches('/')))
}
fn unsupported(source: &Source, used: &[(&str, &[&str])]) -> Vec<Unsupported> {
    source
        .tables
        .iter()
        .filter_map(|(name, table)| {
            if name == "VersionInfo" {
                return None;
            }
            let columns = match used.iter().find(|(n, _)| *n == name) {
                Some((_, cols)) => table
                    .columns
                    .iter()
                    .filter(|c| !cols.contains(&c.as_str()))
                    .cloned()
                    .collect::<Vec<_>>(),
                None => table.columns.clone(),
            };
            (!columns.is_empty()).then(|| Unsupported {
                table: name.clone(),
                rows: table.rows.len(),
                columns,
            })
        })
        .collect()
}

pub(super) fn sonarr(source: &Source) -> Result<Plan> {
    if source.version != 233 {
        return Err(ImportError(
            "unsupported Sonarr schema version; supported: 233",
        ));
    }
    const SERIES: &[&str] = &[
        "Id",
        "TvdbId",
        "Title",
        "Year",
        "Path",
        "Monitored",
        "Seasons",
    ];
    const EPISODES: &[&str] = &[
        "Id",
        "SeriesId",
        "SeasonNumber",
        "EpisodeNumber",
        "Title",
        "Monitored",
        "EpisodeFileId",
    ];
    const FILES: &[&str] = &["Id", "SeriesId", "RelativePath"];
    let series = table(source, "Series", SERIES)?;
    let episodes = table(source, "Episodes", EPISODES)?;
    let files = table(source, "EpisodeFiles", FILES)?;
    let mut file_columns = FILES.to_vec();
    file_columns.extend(FILE_METADATA.iter().map(|(source, _)| *source));
    let mut episode_columns = EPISODES.to_vec();
    episode_columns.extend(EPISODE_METADATA.iter().map(|(source, _)| *source));
    let mut plan = Plan {
        entities: Vec::new(),
        seasons: Vec::new(),
        missing: 0,
        unsupported: unsupported(
            source,
            &[
                ("Series", SERIES),
                ("Episodes", &episode_columns),
                ("EpisodeFiles", &file_columns),
            ],
        ),
    };
    let mut roots = BTreeMap::new();
    let mut seasons = BTreeSet::new();
    for row in &series.rows {
        let id = positive(row, "Id")?;
        let path = root(text(row, "Path")?)?;
        roots.insert(id, path.clone());
        plan.entities.push(Entity {
            table: "series",
            source_id: id,
            fields: vec![
                ("tvdb_id", val(positive(row, "TvdbId")?)),
                ("title", val(text(row, "Title")?)),
                ("year", nullable(row, "Year")?),
                ("path", val(path)),
                ("monitored", val(boolean(row, "Monitored")?)),
            ],
            keys: vec![vec!["tvdb_id"], vec!["path"]],
        });
        let season_rows: serde_json::Value = serde_json::from_str(text(row, "Seasons")?)
            .map_err(|_| ImportError("invalid serialized seasons"))?;
        for season in season_rows
            .as_array()
            .ok_or(ImportError("invalid serialized seasons"))?
        {
            let number = season
                .get("seasonNumber")
                .and_then(|v| v.as_i64())
                .filter(|n| *n >= 0)
                .ok_or(ImportError("invalid season number"))?;
            let monitored = season
                .get("monitored")
                .and_then(|v| v.as_bool())
                .ok_or(ImportError("invalid season monitoring"))?;
            if !seasons.insert((id, number)) {
                return Err(ImportError("duplicate source season"));
            }
            plan.seasons.push((id, number, i64::from(monitored)));
            // Embedded extras are archived but not applied (e.g. season artwork).
            if season.as_object().is_some_and(|o| {
                o.keys()
                    .any(|k| !["seasonNumber", "monitored"].contains(&k.as_str()))
            }) && !plan
                .unsupported
                .iter()
                .any(|u| u.table == "Series" && u.columns.contains(&"Seasons".to_string()))
            {
                plan.unsupported.push(Unsupported {
                    table: "Series".into(),
                    rows: series.rows.len(),
                    columns: vec!["Seasons".into()],
                });
            }
        }
    }
    let mut file_owners = BTreeMap::new();
    for row in &files.rows {
        let id = positive(row, "Id")?;
        let series_id = positive(row, "SeriesId")?;
        let base = roots
            .get(&series_id)
            .ok_or(ImportError("orphan source episode file"))?;
        let path = file_path(base, text(row, "RelativePath")?)?;
        file_owners.insert(id, series_id);
        plan.entities.push(Entity {
            table: "episode_files",
            source_id: id,
            fields: vec![
                ("series_id", Field::Reference("series", series_id)),
                ("path", val(path)),
            ],
            keys: vec![vec!["path"]],
        });
        add_file_metadata(&mut plan, row, "tv", id)?;
    }
    for row in &episodes.rows {
        let series_id = positive(row, "SeriesId")?;
        let season = integer(row, "SeasonNumber")?;
        if !seasons.contains(&(series_id, season)) {
            return Err(ImportError(
                "source episode references missing series or season",
            ));
        }
        let number = integer(row, "EpisodeNumber")?;
        if number < 0 {
            return Err(ImportError("invalid episode number"));
        }
        let file = match optional_id(row, "EpisodeFileId")? {
            Some(id) => match file_owners.get(&id) {
                Some(owner) if *owner == series_id => Field::Reference("episode_files", id),
                Some(_) => {
                    return Err(ImportError("source episode file belongs to another series"));
                }
                None => {
                    plan.missing += 1;
                    Field::Value(Value::Null)
                }
            },
            None => Field::Value(Value::Null),
        };
        let mut fields = vec![
            ("series_id", Field::Reference("series", series_id)),
            ("season", val(season)),
            ("number", val(number)),
            ("title", val(text(row, "Title")?)),
            ("episode_file_id", file),
            ("monitored", val(boolean(row, "Monitored")?)),
        ];
        fields.extend(episode_metadata(row)?);
        if let Some(Value::Text(raw)) = row.get("Images") {
            let images: serde_json::Value =
                serde_json::from_str(raw).map_err(|_| ImportError("invalid episode image data"))?;
            if images.as_array().is_some_and(|images| {
                images.iter().any(|image| {
                    image.as_object().is_some_and(|image| {
                        image
                            .keys()
                            .any(|key| !["coverType", "url", "remoteUrl"].contains(&key.as_str()))
                    })
                })
            }) {
                plan.unsupported.push(Unsupported {
                    table: "Episodes".into(),
                    rows: 1,
                    columns: vec!["Images: unknown cover fields retained privately".into()],
                });
            }
        }

        plan.entities.push(Entity {
            table: "episodes",
            source_id: positive(row, "Id")?,
            fields,
            keys: vec![vec!["series_id", "season", "number"]],
        });
    }
    Ok(plan)
}

pub(super) fn radarr(source: &Source) -> Result<Plan> {
    // Migration 207 is the source contract boundary, not a guessed same-ID join.
    let modern = match source.version {
        242 => true,
        206 => false,
        _ => {
            return Err(ImportError(
                "unsupported Radarr schema version; supported: 206 and 242",
            ));
        }
    };
    const CORE: &[&str] = &["Id", "Path", "Monitored", "MovieFileId"];
    const META: &[&str] = &["Id", "TmdbId", "ImdbId", "Title", "Year"];
    const FILES: &[&str] = &["Id", "MovieId", "RelativePath", "Edition"];
    let movies = table(source, "Movies", CORE)?;
    let metadata = if modern {
        table(source, "MovieMetadata", META)?
    } else {
        table(source, "Movies", META)?
    };
    if modern && !movies.columns.iter().any(|c| c == "MovieMetadataId") {
        return Err(ImportError("split movie schema requires MovieMetadataId"));
    }
    if !modern && source.tables.contains_key("MovieMetadata") {
        return Err(ImportError(
            "inline movie schema has unexpected split metadata",
        ));
    }
    let files = table(source, "MovieFiles", FILES)?;
    let mut file_columns = FILES.to_vec();
    file_columns.extend(FILE_METADATA.iter().map(|(source, _)| *source));
    let mut movie_columns = CORE.to_vec();
    if modern {
        movie_columns.push("MovieMetadataId");
    } else {
        movie_columns.extend(META);
    }
    let mut plan = Plan {
        entities: Vec::new(),
        seasons: Vec::new(),
        missing: 0,
        unsupported: unsupported(
            source,
            &[
                ("Movies", &movie_columns),
                ("MovieMetadata", META),
                ("MovieFiles", &file_columns),
            ],
        ),
    };
    let mut metadata_ids = BTreeSet::new();
    for row in &metadata.rows {
        let id = positive(row, "Id")?;
        metadata_ids.insert(id);
        plan.entities.push(Entity {
            table: "movie_metadata",
            source_id: id,
            fields: vec![
                ("tmdb_id", val(positive(row, "TmdbId")?)),
                ("imdb_id", nullable(row, "ImdbId")?),
                ("title", val(text(row, "Title")?)),
                ("year", nullable(row, "Year")?),
            ],
            keys: vec![vec!["tmdb_id"], vec!["imdb_id"]],
        });
    }
    let mut roots = BTreeMap::new();
    let mut selected_files = BTreeMap::new();
    for row in &movies.rows {
        let id = positive(row, "Id")?;
        let meta = if modern {
            positive(row, "MovieMetadataId")?
        } else {
            id
        };
        if !metadata_ids.contains(&meta) {
            return Err(ImportError("movie references missing source metadata"));
        }
        let path = root(text(row, "Path")?)?;
        roots.insert(id, path.clone());
        if let Some(file) = optional_id(row, "MovieFileId")? {
            if selected_files.insert(file, id).is_some() {
                return Err(ImportError(
                    "source movie file is selected by multiple movies",
                ));
            }
        }
        plan.entities.push(Entity {
            table: "movies",
            source_id: id,
            fields: vec![
                ("metadata_id", Field::Reference("movie_metadata", meta)),
                ("path", val(path)),
                ("monitored", val(boolean(row, "Monitored")?)),
            ],
            keys: vec![vec!["metadata_id"], vec!["path"]],
        });
    }
    let mut found = BTreeSet::new();
    for row in &files.rows {
        let id = positive(row, "Id")?;
        let owner = positive(row, "MovieId")?;
        let base = roots
            .get(&owner)
            .ok_or(ImportError("orphan source movie file"))?;
        let path = file_path(base, text(row, "RelativePath")?)?;
        match selected_files.get(&id) {
            Some(expected) if *expected != owner => {
                return Err(ImportError("source movie file belongs to another movie"));
            }
            None => {
                plan.unsupported.push(Unsupported {
                    table: "MovieFiles".into(),
                    rows: 1,
                    columns: vec!["unselected file record".into()],
                });
                continue;
            }
            _ => {}
        }
        found.insert(id);
        plan.entities.push(Entity {
            table: "movie_files",
            source_id: id,
            fields: vec![
                ("movie_id", Field::Reference("movies", owner)),
                ("path", val(path)),
                ("edition", nullable(row, "Edition")?),
            ],
            keys: vec![vec!["movie_id"], vec!["path"]],
        });
        add_file_metadata(&mut plan, row, "movies", id)?;
    }
    plan.missing = selected_files
        .keys()
        .filter(|id| !found.contains(id))
        .count();
    Ok(plan)
}

const EPISODE_METADATA: &[(&str, &str)] = &[
    ("TvdbId", "tvdb_id"),
    ("AirDate", "air_date"),
    ("AirDateUtc", "air_date_utc"),
    ("LastSearchTime", "last_search_time"),
    ("Runtime", "runtime"),
    ("FinaleType", "finale_type"),
    ("Overview", "overview"),
    ("AbsoluteEpisodeNumber", "absolute_episode_number"),
    (
        "SceneAbsoluteEpisodeNumber",
        "scene_absolute_episode_number",
    ),
    ("SceneEpisodeNumber", "scene_episode_number"),
    ("SceneSeasonNumber", "scene_season_number"),
    ("UnverifiedSceneNumbering", "unverified_scene_numbering"),
    ("Images", "images_json"),
];
fn episode_metadata(row: &Record) -> Result<Vec<(&'static str, Field)>> {
    let mut fields = Vec::new();
    for &(source, destination) in EPISODE_METADATA {
        let value = match row.get(source) {
            None | Some(Value::Null) => Value::Null,
            Some(value) => match source {
                "TvdbId" => match value {
                    Value::Integer(0) => Value::Null,
                    Value::Integer(n) if *n > 0 => value.clone(),
                    _ => return Err(ImportError("invalid episode TVDB identity")),
                },
                "Runtime"
                | "AbsoluteEpisodeNumber"
                | "SceneAbsoluteEpisodeNumber"
                | "SceneEpisodeNumber"
                | "SceneSeasonNumber" => match value {
                    Value::Integer(n) if *n >= 0 => value.clone(),
                    _ => return Err(ImportError("invalid episode runtime or numbering")),
                },
                "UnverifiedSceneNumbering" => match value {
                    Value::Integer(0 | 1) => value.clone(),
                    _ => return Err(ImportError("invalid episode scene flag")),
                },
                "AirDate" | "AirDateUtc" | "LastSearchTime" => match value {
                    Value::Text(s) if s.is_empty() => Value::Null,
                    Value::Text(s) => Value::Text(
                        if source == "AirDate" {
                            crate::episodes::normalize_date(s)
                        } else {
                            crate::episodes::normalize_utc(s)
                        }
                        .ok_or(ImportError("invalid episode date or UTC timestamp"))?,
                    ),
                    _ => return Err(ImportError("invalid episode date type")),
                },
                "Images" => match value {
                    Value::Text(s) if s.len() <= 65536 => {
                        let images: serde_json::Value = serde_json::from_str(s)
                            .map_err(|_| ImportError("invalid episode images JSON"))?;
                        let array =
                            images
                                .as_array()
                                .filter(|a| a.len() <= 32)
                                .ok_or(ImportError(
                                    "episode images must be an array of at most 32 covers",
                                ))?;
                        let mut public_images = Vec::new();
                        for image in array {
                            let object = image
                                .as_object()
                                .ok_or(ImportError("invalid episode cover"))?;
                            let kind = match object.get("coverType") {
                                Some(serde_json::Value::String(s))
                                    if [
                                        "unknown",
                                        "poster",
                                        "banner",
                                        "fanart",
                                        "screenshot",
                                        "headshot",
                                        "clearlogo",
                                    ]
                                    .contains(&s.as_str()) =>
                                {
                                    s.as_str()
                                }
                                Some(serde_json::Value::Number(n))
                                    if n.as_u64().is_some_and(|n| n <= 6) =>
                                {
                                    [
                                        "unknown",
                                        "poster",
                                        "banner",
                                        "fanart",
                                        "screenshot",
                                        "headshot",
                                        "clearlogo",
                                    ][n.as_u64().unwrap() as usize]
                                }
                                _ => return Err(ImportError("invalid episode cover type")),
                            };
                            let mut public = serde_json::json!({"coverType":kind});
                            for key in ["url", "remoteUrl"] {
                                if let Some(value) = object.get(key) {
                                    if !value.is_null()
                                        && !value.as_str().is_some_and(|s| {
                                            s.len() <= 4096 && !s.chars().any(char::is_control)
                                        })
                                    {
                                        return Err(ImportError("invalid episode image URL"));
                                    }
                                    public[key] = value.clone();
                                }
                            }
                            public_images.push(public);
                        }
                        Value::Text(serde_json::Value::Array(public_images).to_string())
                    }
                    _ => return Err(ImportError("invalid episode image data")),
                },
                _ => match value {
                    Value::Text(s)
                        if !s.contains('\0')
                            && s.len() <= if source == "Overview" { 65536 } else { 128 } =>
                    {
                        value.clone()
                    }
                    _ => return Err(ImportError("invalid episode metadata text")),
                },
            },
        };
        fields.push((destination, Field::Value(value)));
    }
    Ok(fields)
}

const FILE_METADATA: &[(&str, &str)] = &[
    ("MediaInfo", "media_info_json"),
    ("Quality", "quality_id"),
    ("Languages", "languages_json"),
    ("Size", "size"),
    ("DateAdded", "date_added"),
    ("SeasonNumber", "season_number"),
    ("OriginalFilePath", "original_file_path"),
    ("ReleaseGroup", "release_group"),
    ("IndexerFlags", "indexer_flags"),
    ("ReleaseType", "release_type"),
];
fn add_file_metadata(plan: &mut Plan, row: &Record, media: &'static str, id: i64) -> Result<()> {
    use serde_json::{Value as J, json};
    let target = if media == "tv" {
        "episode_file_id"
    } else {
        "movie_file_id"
    };
    let core = if media == "tv" {
        "episode_files"
    } else {
        "movie_files"
    };
    let source_table = if media == "tv" {
        "EpisodeFiles"
    } else {
        "MovieFiles"
    };
    let mut fields = vec![
        ("media_type", val(media)),
        (target, Field::Reference(core, id)),
        ("quality_id", Field::Value(Value::Null)),
        ("revision_json", Field::Value(Value::Null)),
    ];
    let mut unsupported = vec![];
    for (source, destination) in FILE_METADATA {
        let raw = row.get(*source).unwrap_or(&Value::Null);
        if *source == "Quality" {
            if matches!(raw, Value::Null) {
                continue;
            }
            let Value::Text(raw) = raw else {
                return Err(ImportError("invalid source file quality"));
            };
            let quality: J = serde_json::from_str(raw)
                .map_err(|_| ImportError("invalid source file quality JSON"))?;
            if quality.is_null() {
                continue;
            }
            let obj = quality
                .as_object()
                .ok_or(ImportError("invalid source file quality object"))?;
            let qid = obj
                .get("quality")
                .and_then(J::as_i64)
                .ok_or(ImportError("invalid source file quality id"))?;
            // Supported ids are the pinned catalog identities seeded by migration0004.
            let known = if media == "tv" {
                matches!(qid,0..=10|12..=22)
            } else {
                matches!(qid,0..=10|12|14..=31)
            };
            if !known {
                unsupported.push("Quality".to_string());
                continue;
            }
            let revision = match obj.get("revision") {
                None | Some(J::Null) => J::Null,
                Some(r) => {
                    let r = r
                        .as_object()
                        .ok_or(ImportError("invalid source file revision"))?;
                    if r.keys()
                        .any(|k| !matches!(k.as_str(), "version" | "real" | "isRepack"))
                    {
                        unsupported.push("Quality.revision".into());
                    }
                    json!({"version":r.get("version"),"real":r.get("real"),"is_repack":r.get("isRepack")})
                }
            };
            if obj
                .keys()
                .any(|k| !matches!(k.as_str(), "quality" | "revision"))
            {
                unsupported.push("Quality.extra_fields".into());
            }
            let (qid, revision) =
                crate::media_files::quality(&json!({"quality_id":qid,"revision":revision}))
                    .map_err(ImportError)?;
            fields[2].1 = val(qid);
            fields[3].1 = Field::Value(revision.map(Value::Text).unwrap_or(Value::Null));
            continue;
        }
        if (media == "movies" && matches!(*source, "SeasonNumber" | "ReleaseType"))
            || (media == "tv" && *source == "OriginalFilePath")
        {
            if !matches!(raw, Value::Null) {
                unsupported.push(source.to_string());
            }
            continue;
        }
        let value = match raw {
            Value::Null => Value::Null,
            _ => match *source {
                "MediaInfo" => {
                    let Value::Text(raw) = raw else {
                        return Err(ImportError("invalid source media info"));
                    };
                    let (normalized, unknown) =
                        crate::media_files::media_info::normalize(raw, media)
                            .map_err(ImportError)?;
                    if unknown {
                        unsupported.push("MediaInfo.unsupported_fields_or_values".into());
                    }
                    normalized.map(Value::Text).unwrap_or(Value::Null)
                }
                "Languages" => {
                    let Value::Text(raw) = raw else {
                        return Err(ImportError("invalid source file languages"));
                    };
                    let mut languages: J = serde_json::from_str(raw)
                        .map_err(|_| ImportError("invalid source file languages JSON"))?;
                    if let Some(array) = languages.as_array_mut() {
                        for value in array {
                            if value.is_null() {
                                *value = json!(0);
                            }
                        }
                    }
                    match crate::media_files::languages(&languages, media) {
                        Ok(v) => v.map(Value::Text).unwrap_or(Value::Null),
                        Err(_) => {
                            unsupported.push("Languages".into());
                            Value::Null
                        }
                    }
                }
                "DateAdded" => {
                    let Value::Text(raw) = raw else {
                        return Err(ImportError("invalid source file date"));
                    };
                    Value::Text(
                        crate::episodes::normalize_utc(raw)
                            .ok_or(ImportError("invalid source file date"))?,
                    )
                }
                "ReleaseGroup" | "OriginalFilePath" => {
                    let Value::Text(raw) = raw else {
                        return Err(ImportError("invalid source file text"));
                    };
                    crate::media_files::string(
                        &json!(raw),
                        if *source == "OriginalFilePath" {
                            4096
                        } else {
                            1024
                        },
                    )
                    .map_err(ImportError)?
                }
                "Size" | "SeasonNumber" => {
                    let Value::Integer(n) = raw else {
                        return Err(ImportError("invalid source file number"));
                    };
                    crate::media_files::number(&json!(n), i64::MAX).map_err(ImportError)?
                }
                "IndexerFlags" | "ReleaseType" => {
                    let Value::Integer(n) = raw else {
                        return Err(ImportError("invalid source file enum"));
                    };
                    let max = if *source == "ReleaseType" {
                        3
                    } else if media == "tv" {
                        511
                    } else {
                        4095
                    };
                    match crate::media_files::number(&json!(n), max) {
                        Ok(v) => v,
                        Err(_) => {
                            unsupported.push(source.to_string());
                            Value::Null
                        }
                    }
                }
                _ => unreachable!(),
            },
        };
        fields.push((destination, Field::Value(value)));
    }
    if !unsupported.is_empty() {
        plan.unsupported.push(Unsupported {
            table: source_table.into(),
            rows: 1,
            columns: unsupported,
        });
    }
    plan.entities.push(Entity {
        table: "file_metadata",
        source_id: id,
        fields,
        keys: vec![vec![target]],
    });
    Ok(())
}
