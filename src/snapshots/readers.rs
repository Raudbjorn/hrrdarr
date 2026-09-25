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
    let mut plan = Plan {
        entities: Vec::new(),
        seasons: Vec::new(),
        missing: 0,
        unsupported: unsupported(
            source,
            &[
                ("Series", SERIES),
                ("Episodes", EPISODES),
                ("EpisodeFiles", FILES),
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
        plan.entities.push(Entity {
            table: "episodes",
            source_id: positive(row, "Id")?,
            fields: vec![
                ("series_id", Field::Reference("series", series_id)),
                ("season", val(season)),
                ("number", val(number)),
                ("title", val(text(row, "Title")?)),
                ("episode_file_id", file),
                ("monitored", val(boolean(row, "Monitored")?)),
            ],
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
                ("MovieFiles", FILES),
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
    }
    plan.missing = selected_files
        .keys()
        .filter(|id| !found.contains(id))
        .count();
    Ok(plan)
}
