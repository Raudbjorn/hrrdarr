//! Atomic catalog reconciliation, without network or transaction ownership.
//! Callers must roll back their transaction on every error, including storage failures.
use crate::metadata::{EpisodeDetails, MovieDetails, SeriesDetails};
use libsql::{Connection, Value, params};
use std::collections::{BTreeMap, BTreeSet};
const MAX_ID: i64 = 9007199254740991;
const MAX_EPISODES: usize = 10000;
const MAX_SEASONS: usize = 1000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tv { series_id: i64 },
    Movies { movie_id: i64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedTarget {
    pub target: Target,
    pub external_id: i64,
    pub metadata_id: Option<i64>,
}
pub enum Details {
    Series(SeriesDetails),
    Movie(MovieDetails),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    TargetChanged,
    Conflict,
    Storage,
}
impl From<libsql::Error> for Error {
    fn from(_: libsql::Error) -> Self {
        Self::Storage
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TargetChanged => "metadata target changed",
            Self::Conflict => "metadata reconciliation conflict",
            Self::Storage => "metadata storage failed",
        })
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
pub async fn capture(c: &Connection, target: Target) -> Result<CapturedTarget> {
    let (sql, id) = match target {
        Target::Tv { series_id } => ("SELECT tvdb_id,NULL FROM series WHERE id=?", series_id),
        Target::Movies { movie_id } => (
            "SELECT d.tmdb_id,m.metadata_id FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",
            movie_id,
        ),
    };
    if !(1..=MAX_ID).contains(&id) {
        return Err(Error::Conflict);
    }
    let row = c
        .query(sql, [id])
        .await?
        .next()
        .await?
        .ok_or(Error::TargetChanged)?;
    let external_id = row
        .get::<Option<i64>>(0)?
        .filter(|id| (1..=MAX_ID).contains(id))
        .ok_or(Error::Conflict)?;
    Ok(CapturedTarget {
        target,
        external_id,
        metadata_id: row.get(1)?,
    })
}
/// Details must come from MetadataClient validation. Identity and graph bounds are
/// checked again here because the destination may have changed during the fetch.
pub async fn apply(c: &Connection, captured: &CapturedTarget, details: Details) -> Result<u16> {
    if capture(c, captured.target).await.map_err(|error| {
        if error == Error::Storage {
            error
        } else {
            Error::TargetChanged
        }
    })? != *captured
    {
        return Err(Error::TargetChanged);
    }
    match (captured.target, details) {
        (Target::Tv { series_id }, Details::Series(details))
            if details.tvdb_id == captured.external_id =>
        {
            series(c, series_id, details).await
        }
        (Target::Movies { .. }, Details::Movie(details))
            if details.tmdb_id == captured.external_id =>
        {
            movie(
                c,
                captured.metadata_id.ok_or(Error::TargetChanged)?,
                details,
            )
            .await
        }
        _ => Err(Error::TargetChanged),
    }
}
async fn movie(c: &Connection, id: i64, detail: MovieDetails) -> Result<u16> {
    if let Some(imdb) = &detail.imdb_id {
        if c.query(
            "SELECT 1 FROM movie_metadata WHERE imdb_id=? AND id!=? LIMIT 1",
            params![imdb.clone(), id],
        )
        .await?
        .next()
        .await?
        .is_some()
        {
            return Err(Error::Conflict);
        }
    }
    let facts_changed = movie_facts(c, id, &detail, true).await?;
    let changed=c.execute("UPDATE movie_metadata SET title=?1,year=COALESCE(?2,year),imdb_id=COALESCE(?3,imdb_id) WHERE id=?4 AND (title IS NOT ?1 OR year IS NOT COALESCE(?2,year) OR imdb_id IS NOT COALESCE(?3,imdb_id))",params![detail.title,detail.year,detail.imdb_id,id]).await?;
    Ok(u16::from(facts_changed || changed > 0))
}

/// Caller owns the transaction. Sparse facts preserve prior values; an explicit
/// alias set replaces the complete set. Catalog adoption rejects existing conflicts.
pub(crate) async fn movie_facts(
    c: &Connection,
    id: i64,
    detail: &MovieDetails,
    replace: bool,
) -> Result<bool> {
    const FIELDS: &str = "runtime,status,in_cinemas,digital_release,physical_release,secondary_year,original_language";
    let supplied: Vec<Value> = vec![
        detail.runtime.into(),
        detail.status.clone().into(),
        detail.in_cinemas.clone().into(),
        detail.digital_release.clone().into(),
        detail.physical_release.clone().into(),
        detail.secondary_year.into(),
        detail.original_language.into(),
    ];
    let row = c
        .query(
            &format!("SELECT {FIELDS} FROM movie_metadata WHERE id=?"),
            [id],
        )
        .await?
        .next()
        .await?
        .ok_or(Error::TargetChanged)?;
    let mut changed = false;
    let mut values = Vec::with_capacity(8);
    for (i, incoming) in supplied.into_iter().enumerate() {
        let prior = row.get_value(i as i32)?;
        let value = if incoming == Value::Null {
            prior.clone()
        } else {
            incoming
        };
        if !replace && prior != Value::Null && value != prior {
            return Err(Error::Conflict);
        }
        changed |= value != prior;
        values.push(value);
    }
    drop(row);
    let mut aliases_changed = false;
    let aliases = if let Some(titles) = &detail.alternative_titles {
        if titles.len() > 64 {
            return Err(Error::Conflict);
        }
        let desired = titles.iter().cloned().collect::<BTreeSet<_>>();
        let mut rows=c.query("SELECT title FROM movie_alternative_titles WHERE metadata_id=? ORDER BY title LIMIT 65",[id]).await?;
        let mut prior = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            prior.insert(row.get::<String>(0)?);
        }
        if prior.len() > 64 {
            return Err(Error::Conflict);
        }
        aliases_changed = prior != desired;
        if !replace && !prior.is_empty() && aliases_changed {
            return Err(Error::Conflict);
        }
        Some(desired)
    } else {
        None
    };
    if changed {
        values.push(id.into());
        c.execute("UPDATE movie_metadata SET runtime=?,status=?,in_cinemas=?,digital_release=?,physical_release=?,secondary_year=?,original_language=? WHERE id=?",values).await?;
    }
    if aliases_changed {
        c.execute(
            "DELETE FROM movie_alternative_titles WHERE metadata_id=?",
            [id],
        )
        .await?;
        for title in aliases.unwrap_or_default() {
            c.execute(
                "INSERT INTO movie_alternative_titles(metadata_id,title) VALUES(?,?)",
                params![id, title],
            )
            .await?;
        }
    }
    Ok(changed || aliases_changed)
}

struct StoredEpisode {
    id: i64,
    season: i64,
    number: i64,
}
fn episode_facts(episode: &EpisodeDetails) -> Vec<Value> {
    vec![
        episode.title.clone().into(),
        episode.air_date.clone().into(),
        episode.air_date_utc.clone().into(),
        episode.absolute_episode_number.into(),
        episode.runtime.into(),
        episode.overview.clone().into(),
        episode.finale_type.clone().into(),
    ]
}
async fn series(c: &Connection, id: i64, detail: SeriesDetails) -> Result<u16> {
    if detail.seasons.len() > MAX_SEASONS || detail.episodes.len() > MAX_EPISODES {
        return Err(Error::Conflict);
    }
    let incoming_seasons = detail.seasons.iter().copied().collect::<BTreeSet<_>>();
    if incoming_seasons.len() != detail.seasons.len() || incoming_seasons.iter().any(|n| *n < 0) {
        return Err(Error::Conflict);
    }
    let mut incoming_ids = BTreeSet::new();
    let mut incoming_numbers = BTreeSet::new();
    for episode in &detail.episodes {
        if !(1..=MAX_ID).contains(&episode.tvdb_id)
            || episode.number < 0
            || !incoming_seasons.contains(&episode.season)
            || !incoming_ids.insert(episode.tvdb_id)
            || !incoming_numbers.insert((episode.season, episode.number))
        {
            return Err(Error::Conflict);
        }
    }
    let row=c.query("SELECT l.monitor_new_items FROM series s LEFT JOIN library_settings l ON l.series_id=s.id AND l.media_type='tv' WHERE s.id=?",[id]).await?.next().await?.ok_or(Error::TargetChanged)?;
    let new_policy = row.get::<Option<String>>(0)?;
    drop(row);
    let mut seasons = BTreeMap::new();
    let mut rows = c
        .query(
            "SELECT number,monitored FROM seasons WHERE series_id=? ORDER BY number LIMIT 1001",
            [id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        seasons.insert(row.get::<i64>(0)?, row.get::<i64>(1)?);
    }
    drop(rows);
    if seasons.len() > MAX_SEASONS || seasons.keys().any(|n| !incoming_seasons.contains(n)) {
        return Err(Error::Conflict);
    }
    let mut old = BTreeMap::new();
    let mut rows=c.query("SELECT id,tvdb_id,season,number FROM episodes WHERE series_id=? ORDER BY id LIMIT 10001",[id]).await?;
    while let Some(row) = rows.next().await? {
        let external = row.get::<Option<i64>>(1)?.ok_or(Error::Conflict)?;
        let value = StoredEpisode {
            id: row.get(0)?,
            season: row.get(2)?,
            number: row.get(3)?,
        };
        if old.insert(external, value).is_some() || old.len() > MAX_EPISODES {
            return Err(Error::Conflict);
        }
    }
    drop(rows);
    if old.keys().any(|external| !incoming_ids.contains(external)) {
        return Err(Error::Conflict);
    }
    for episode in &detail.episodes {
        if let Some(stored) = old.get(&episode.tvdb_id) {
            if stored.season != episode.season || stored.number != episode.number {
                return Err(Error::Conflict);
            }
        }
    }
    // Enforce global catalog ownership even where a legacy database has a nonunique index.
    let ids = incoming_ids.iter().copied().collect::<Vec<_>>();
    for chunk in ids.chunks(200) {
        let mut rows = c
            .query(
                &format!(
                    "SELECT tvdb_id,series_id FROM episodes WHERE tvdb_id IN ({}) LIMIT 201",
                    vec!["?"; chunk.len()].join(",")
                ),
                chunk
                    .iter()
                    .copied()
                    .map(Value::Integer)
                    .collect::<Vec<_>>(),
            )
            .await?;
        let mut seen = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            if row.get::<i64>(1)? != id || !seen.insert(row.get::<i64>(0)?) {
                return Err(Error::Conflict);
            }
        }
    }
    let mut additions = Vec::new();
    for number in incoming_seasons {
        if !seasons.contains_key(&number) {
            let monitored = if number == 0 {
                0
            } else {
                match new_policy.as_deref() {
                    Some("all") => 1,
                    Some("none") => 0,
                    _ => return Err(Error::Conflict),
                }
            };
            additions.push((number, monitored));
            seasons.insert(number, monitored);
        }
    }
    // All ambiguous identities and monitoring policy are checked before modifying any row.
    let mut changed=c.execute("UPDATE series SET title=?1,year=COALESCE(?2,year) WHERE id=?3 AND (title IS NOT ?1 OR year IS NOT COALESCE(?2,year))",params![detail.title,detail.year,id]).await? as u16;
    for (number, monitored) in additions {
        c.execute(
            "INSERT INTO seasons(series_id,number,monitored) VALUES(?,?,?)",
            params![id, number, monitored],
        )
        .await?;
        changed += 1;
    }
    for episode in detail.episodes {
        let mut facts = episode_facts(&episode);
        if let Some(stored) = old.get(&episode.tvdb_id) {
            facts.push(stored.id.into());
            changed+=c.execute("UPDATE episodes SET title=?1,air_date=COALESCE(?2,air_date),air_date_utc=COALESCE(?3,air_date_utc),absolute_episode_number=COALESCE(?4,absolute_episode_number),runtime=COALESCE(?5,runtime),overview=COALESCE(?6,overview),finale_type=COALESCE(?7,finale_type) WHERE id=?8 AND (title IS NOT ?1 OR air_date IS NOT COALESCE(?2,air_date) OR air_date_utc IS NOT COALESCE(?3,air_date_utc) OR absolute_episode_number IS NOT COALESCE(?4,absolute_episode_number) OR runtime IS NOT COALESCE(?5,runtime) OR overview IS NOT COALESCE(?6,overview) OR finale_type IS NOT COALESCE(?7,finale_type))",facts).await? as u16;
        } else {
            let monitored = if episode.number == 0 && episode.season != 1 {
                0
            } else {
                *seasons.get(&episode.season).ok_or(Error::Conflict)?
            };
            let mut values = vec![
                id.into(),
                episode.tvdb_id.into(),
                episode.season.into(),
                episode.number.into(),
                monitored.into(),
            ];
            values.extend(facts);
            c.execute("INSERT INTO episodes(series_id,tvdb_id,season,number,monitored,title,air_date,air_date_utc,absolute_episode_number,runtime,overview,finale_type) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",values).await?;
            changed += 1;
        }
    }
    Ok(changed)
}
