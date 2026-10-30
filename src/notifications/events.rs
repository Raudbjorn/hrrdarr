//! Event kinds, the outbox row, and the domain-aware message builder.
//!
//! A [`Message`] is built per delivery from the immutable event row plus the live library state,
//! and is the only thing providers see. TV and movie events produce different structured payloads
//! and different script environments (`sonarr_*` / `radarr_*`); there is no shared id field.
use crate::api::MediaDomain;
use libsql::{Connection, params};
use serde_json::{Value, json};

pub const INSTANCE_NAME: &str = "hrrdarr";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Grab,
    Import,
    Rename,
    Add,
    Delete,
    FileDelete,
    HealthIssue,
    HealthRestored,
    ApplicationUpdate,
    ManualInteraction,
    Test,
}
impl EventKind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "grab" => Self::Grab,
            "import" => Self::Import,
            "rename" => Self::Rename,
            "add" => Self::Add,
            "delete" => Self::Delete,
            "file_delete" => Self::FileDelete,
            "health_issue" => Self::HealthIssue,
            "health_restored" => Self::HealthRestored,
            "application_update" => Self::ApplicationUpdate,
            "manual_interaction" => Self::ManualInteraction,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grab => "grab",
            Self::Import => "import",
            Self::Rename => "rename",
            Self::Add => "add",
            Self::Delete => "delete",
            Self::FileDelete => "file_delete",
            Self::HealthIssue => "health_issue",
            Self::HealthRestored => "health_restored",
            Self::ApplicationUpdate => "application_update",
            Self::ManualInteraction => "manual_interaction",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Domain(MediaDomain),
    System,
}
impl Scope {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tv" => Some(Self::Domain(MediaDomain::Tv)),
            "movies" => Some(Self::Domain(MediaDomain::Movies)),
            "system" => Some(Self::System),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Domain(MediaDomain::Tv) => "tv",
            Self::Domain(MediaDomain::Movies) => "movies",
            Self::System => "system",
        }
    }
}

/// One immutable outbox row.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub sequence: i64,
    pub kind: EventKind,
    pub scope: Scope,
    pub subject_id: Option<i64>,
    pub payload: Value,
    pub tag_ids: Option<Vec<i64>>,
}

/// What a provider formats. Never contains credentials.
#[derive(Debug, Clone)]
pub struct Message {
    pub kind: EventKind,
    pub domain: MediaDomain,
    pub title: String,
    pub text: String,
    /// Full structured event (the generic webhook body).
    pub json: Value,
    /// Script environment, already prefixed `sonarr_` / `radarr_`.
    pub env: Vec<(String, String)>,
    /// Library paths this event touched (media-server updaters).
    pub paths: Vec<String>,
    /// Health severity (1 notice, 2 warning, 3 error) for health events.
    pub severity: Option<i64>,
}

#[derive(Debug)]
pub enum BuildError {
    Storage,
}
impl From<libsql::Error> for BuildError {
    fn from(_: libsql::Error) -> Self {
        Self::Storage
    }
}

fn prefix(domain: MediaDomain) -> &'static str {
    match domain {
        MediaDomain::Tv => "sonarr",
        MediaDomain::Movies => "radarr",
    }
}
fn s(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn i(payload: &Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(Value::as_i64)
}
fn relative(path: &str, root: &str) -> String {
    let root = root.trim_end_matches('/');
    path.strip_prefix(root)
        .map(|rest| rest.trim_start_matches('/').to_owned())
        .filter(|rest| !rest.is_empty())
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_owned())
}
fn folder(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_owned())
        .unwrap_or_default()
}
fn pascal_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

struct Series {
    id: i64,
    title: String,
    year: Option<i64>,
    tvdb_id: Option<i64>,
    path: String,
    tags: Vec<String>,
}
struct Movie {
    id: i64,
    title: String,
    year: Option<i64>,
    tmdb_id: Option<i64>,
    imdb_id: Option<String>,
    path: String,
    tags: Vec<String>,
}
struct Episode {
    id: i64,
    season: i64,
    number: i64,
    title: String,
    air_date: Option<String>,
}

async fn snapshot_tags(
    c: &Connection,
    domain: MediaDomain,
    ids: &[i64],
) -> Result<Vec<String>, BuildError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let encoded = serde_json::to_string(ids).map_err(|_| BuildError::Storage)?;
    let media = match domain {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    };
    let mut rows = c
        .query(
            "SELECT t.label FROM tags t JOIN json_each(?) j ON j.value=t.id WHERE t.media_type=? ORDER BY t.label",
            params![encoded, media],
        )
        .await?;
    let mut labels = Vec::new();
    while let Some(r) = rows.next().await? {
        labels.push(r.get(0)?);
    }
    Ok(labels)
}

async fn series(c: &Connection, id: i64, snapshot: &Value) -> Result<Series, BuildError> {
    let mut rows = c
        .query(
            "SELECT title,year,tvdb_id,path FROM series WHERE id=?",
            [id],
        )
        .await?;
    let (title, year, tvdb_id, path) = if let Some(r) = rows.next().await? {
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)
    } else {
        (
            s(snapshot, "title").unwrap_or_else(|| "Unknown series".into()),
            i(snapshot, "year"),
            i(snapshot, "tvdb_id"),
            s(snapshot, "path").unwrap_or_default(),
        )
    };
    let mut tags = Vec::new();
    let mut rows = c
        .query(
            "SELECT t.label FROM series_tags st JOIN tags t ON t.id=st.tag_id WHERE st.series_id=? ORDER BY t.label",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        tags.push(r.get(0)?);
    }
    Ok(Series {
        id,
        title,
        year,
        tvdb_id,
        path,
        tags,
    })
}
async fn movie(c: &Connection, id: i64, snapshot: &Value) -> Result<Movie, BuildError> {
    let mut rows = c
        .query(
            "SELECT d.title,d.year,d.tmdb_id,d.imdb_id,m.path FROM movies m JOIN movie_metadata d ON d.id=m.metadata_id WHERE m.id=?",
            [id],
        )
        .await?;
    let (title, year, tmdb_id, imdb_id, path) = if let Some(r) = rows.next().await? {
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)
    } else {
        (
            s(snapshot, "title").unwrap_or_else(|| "Unknown movie".into()),
            i(snapshot, "year"),
            i(snapshot, "tmdb_id"),
            s(snapshot, "imdb_id"),
            s(snapshot, "path").unwrap_or_default(),
        )
    };
    let mut tags = Vec::new();
    let mut rows = c
        .query(
            "SELECT t.label FROM movie_tags mt JOIN tags t ON t.id=mt.tag_id WHERE mt.movie_id=? ORDER BY t.label",
            [id],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        tags.push(r.get(0)?);
    }
    Ok(Movie {
        id,
        title,
        year,
        tmdb_id,
        imdb_id,
        path,
        tags,
    })
}
async fn episodes(
    c: &Connection,
    sql: &str,
    key: impl libsql::params::IntoParams,
) -> Result<Vec<Episode>, BuildError> {
    let mut out = Vec::new();
    let mut rows = c.query(sql, key).await?;
    while let Some(r) = rows.next().await? {
        out.push(Episode {
            id: r.get(0)?,
            season: r.get(1)?,
            number: r.get(2)?,
            title: r.get(3)?,
            air_date: r.get(4)?,
        });
    }
    Ok(out)
}
async fn provider_name(c: &Connection, id: Option<String>) -> Result<Option<String>, BuildError> {
    let Some(id) = id else { return Ok(None) };
    let mut rows = c
        .query("SELECT name FROM providers WHERE id=?", [id])
        .await?;
    Ok(match rows.next().await? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

fn series_json(v: &Series, with_path: bool) -> Value {
    let mut value =
        json!({"id":v.id,"title":v.title,"year":v.year,"tvdbId":v.tvdb_id,"tags":v.tags});
    if with_path {
        value["path"] = json!(v.path);
    }
    value
}
fn movie_json(v: &Movie, with_path: bool) -> Value {
    let mut value = json!({"id":v.id,"title":v.title,"year":v.year,"tmdbId":v.tmdb_id,"imdbId":v.imdb_id,"tags":v.tags});
    if with_path {
        value["folderPath"] = json!(v.path);
    }
    value
}
fn episodes_json(list: &[Episode]) -> Value {
    Value::Array(
        list.iter()
            .map(|e| {
                json!({"id":e.id,"seasonNumber":e.season,"episodeNumber":e.number,"title":e.title,"airDate":e.air_date})
            })
            .collect(),
    )
}
fn episode_label(list: &[Episode]) -> String {
    list.iter()
        .map(|e| format!("{}x{:02} - {}", e.season, e.number, e.title))
        .collect::<Vec<_>>()
        .join(", ")
}
fn base_env(domain: MediaDomain, event_type: &str) -> Vec<(String, String)> {
    let p = prefix(domain);
    vec![
        (format!("{p}_eventtype"), event_type.into()),
        (format!("{p}_instancename"), INSTANCE_NAME.into()),
        (format!("{p}_applicationurl"), String::new()),
    ]
}
fn tv_env(env: &mut Vec<(String, String)>, v: &Series, with_path: bool) {
    env.push(("sonarr_series_id".into(), v.id.to_string()));
    env.push(("sonarr_series_title".into(), v.title.clone()));
    env.push((
        "sonarr_series_tvdbid".into(),
        v.tvdb_id.map(|n| n.to_string()).unwrap_or_default(),
    ));
    env.push((
        "sonarr_series_year".into(),
        v.year.map(|n| n.to_string()).unwrap_or_default(),
    ));
    env.push(("sonarr_series_tags".into(), v.tags.join("|")));
    if with_path {
        env.push(("sonarr_series_path".into(), v.path.clone()));
    }
}
fn movie_env(env: &mut Vec<(String, String)>, v: &Movie, with_path: bool) {
    env.push(("radarr_movie_id".into(), v.id.to_string()));
    env.push(("radarr_movie_title".into(), v.title.clone()));
    env.push((
        "radarr_movie_year".into(),
        v.year.map(|n| n.to_string()).unwrap_or_default(),
    ));
    env.push((
        "radarr_movie_imdbid".into(),
        v.imdb_id.clone().unwrap_or_default(),
    ));
    env.push((
        "radarr_movie_tmdbid".into(),
        v.tmdb_id.map(|n| n.to_string()).unwrap_or_default(),
    ));
    env.push(("radarr_movie_tags".into(), v.tags.join("|")));
    if with_path {
        env.push(("radarr_movie_path".into(), v.path.clone()));
    }
}
fn movie_label(v: &Movie) -> String {
    match v.year {
        Some(year) if year > 0 => format!("{} ({year})", v.title),
        _ => v.title.clone(),
    }
}

/// Sample message for the Test endpoint. Uses fixed illustrative data, never library rows.
pub fn test_message(domain: MediaDomain) -> Message {
    Message {
        kind: EventKind::Test,
        domain,
        title: "Test Notification".into(),
        text: "This is a test message from hrrdarr.".into(),
        json: json!({"eventType":"Test","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":match domain {MediaDomain::Tv=>"tv",MediaDomain::Movies=>"movies"}}),
        env: base_env(domain, "Test"),
        paths: Vec::new(),
        severity: None,
    }
}

/// Builds the message for one delivery. `domain` is the notification scope the delivery matched;
/// system-wide events (health, updates) are rendered for that scope's application contract.
pub async fn build(
    c: &Connection,
    event: &EventRow,
    domain: MediaDomain,
) -> Result<Message, BuildError> {
    let payload = &event.payload;
    let p = prefix(domain);
    let mut env;
    let (title, text, json, paths, severity);
    match (event.kind, domain) {
        (EventKind::Grab | EventKind::ManualInteraction, _) => {
            let manual = event.kind == EventKind::ManualInteraction;
            let candidate = s(payload, "candidate_id").unwrap_or_default();
            let release = s(payload, "release_title").unwrap_or_default();
            let indexer = provider_name(c, s(payload, "indexer_id")).await?;
            let client = provider_name(c, s(payload, "client_id")).await?;
            let hash: Option<String> = {
                let mut rows = c
                    .query(
                        "SELECT observed_hash FROM rss_candidates WHERE id=?",
                        [candidate.clone()],
                    )
                    .await?;
                match rows.next().await? {
                    Some(r) => r.get(0)?,
                    None => None,
                }
            };
            let event_type = if manual {
                "ManualInteractionRequired"
            } else {
                "Grab"
            };
            env = base_env(domain, event_type);
            let release_json = json!({"title":release,"indexer":indexer,"downloadClient":client,"downloadId":hash});
            if let (MediaDomain::Tv, Some(id)) = (domain, event.subject_id) {
                let v = series(c, id, payload).await?;
                let eps = episodes(
                    c,
                    "SELECT e.id,e.season,e.number,e.title,e.air_date FROM rss_candidate_episodes ce JOIN episodes e ON e.id=ce.episode_id WHERE ce.candidate_id=? ORDER BY e.season,e.number",
                    [candidate],
                )
                .await?;
                tv_env(&mut env, &v, false);
                env.push(("sonarr_release_title".into(), release.clone()));
                env.push((
                    "sonarr_release_indexer".into(),
                    indexer.clone().unwrap_or_default(),
                ));
                env.push((
                    "sonarr_download_client".into(),
                    client.clone().unwrap_or_default(),
                ));
                env.push((
                    "sonarr_download_id".into(),
                    hash.clone().unwrap_or_default(),
                ));
                env.push(("sonarr_release_episodecount".into(), eps.len().to_string()));
                env.push((
                    "sonarr_release_seasonnumber".into(),
                    eps.first()
                        .map(|e| e.season.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "sonarr_release_episodenumbers".into(),
                    eps.iter()
                        .map(|e| e.number.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                ));
                env.push((
                    "sonarr_release_episodetitles".into(),
                    eps.iter()
                        .map(|e| e.title.clone())
                        .collect::<Vec<_>>()
                        .join("|"),
                ));
                env.push((
                    "sonarr_release_episodeairdates".into(),
                    eps.iter()
                        .map(|e| e.air_date.clone().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(","),
                ));
                title = if manual {
                    "Manual Interaction Required"
                } else {
                    "Episode Grabbed"
                }
                .to_owned();
                text = format!("{} - {}\n{}", v.title, episode_label(&eps), release);
                json = json!({"eventType":event_type,"instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"tv",
                    "series":series_json(&v,false),"episodes":episodes_json(&eps),"release":release_json});
            } else if let (MediaDomain::Movies, Some(id)) = (domain, event.subject_id) {
                let v = movie(c, id, payload).await?;
                movie_env(&mut env, &v, false);
                env.push(("radarr_release_title".into(), release.clone()));
                env.push((
                    "radarr_release_indexer".into(),
                    indexer.clone().unwrap_or_default(),
                ));
                env.push((
                    "radarr_download_client".into(),
                    client.clone().unwrap_or_default(),
                ));
                env.push((
                    "radarr_download_id".into(),
                    hash.clone().unwrap_or_default(),
                ));
                title = if manual {
                    "Manual Interaction Required"
                } else {
                    "Movie Grabbed"
                }
                .to_owned();
                text = format!("{}\n{}", movie_label(&v), release);
                json = json!({"eventType":event_type,"instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"movies",
                    "movie":movie_json(&v,false),"release":release_json});
            } else {
                return Err(BuildError::Storage);
            }
            paths = Vec::new();
            severity = None;
        }
        (EventKind::Import, _) => {
            let is_upgrade = i(payload, "is_upgrade") == Some(1);
            let path = s(payload, "path").unwrap_or_default();
            let source = s(payload, "source_path").unwrap_or_default();
            env = base_env(domain, "Download");
            env.push((format!("{p}_isupgrade"), pascal_bool(is_upgrade).into()));
            let verb = if is_upgrade { "Upgraded" } else { "Imported" };
            if let (MediaDomain::Tv, Some(id)) = (domain, event.subject_id) {
                let v = series(c, id, payload).await?;
                let operation = s(payload, "operation_id").unwrap_or_default();
                let eps = episodes(
                    c,
                    "SELECT e.id,e.season,e.number,e.title,e.air_date FROM episodes e WHERE e.id IN (SELECT episode_id FROM import_history WHERE operation_id=?1 UNION SELECT episode_id FROM import_history_episodes WHERE operation_id=?1) ORDER BY e.season,e.number",
                    [operation],
                )
                .await?;
                tv_env(&mut env, &v, true);
                env.push((
                    "sonarr_episodefile_id".into(),
                    i(payload, "episode_file_id")
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "sonarr_episodefile_relativepath".into(),
                    relative(&path, &v.path),
                ));
                env.push(("sonarr_episodefile_path".into(), path.clone()));
                env.push((
                    "sonarr_episodefile_episodecount".into(),
                    eps.len().to_string(),
                ));
                env.push((
                    "sonarr_episodefile_episodeids".into(),
                    eps.iter()
                        .map(|e| e.id.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                ));
                env.push((
                    "sonarr_episodefile_seasonnumber".into(),
                    eps.first()
                        .map(|e| e.season.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "sonarr_episodefile_episodenumbers".into(),
                    eps.iter()
                        .map(|e| e.number.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                ));
                env.push((
                    "sonarr_episodefile_episodetitles".into(),
                    eps.iter()
                        .map(|e| e.title.clone())
                        .collect::<Vec<_>>()
                        .join("|"),
                ));
                env.push(("sonarr_episodefile_sourcepath".into(), source.clone()));
                env.push(("sonarr_episodefile_sourcefolder".into(), folder(&source)));
                title = format!("Episode {verb}");
                text = format!("{} - {}", v.title, episode_label(&eps));
                json = json!({"eventType":"Download","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"tv",
                    "isUpgrade":is_upgrade,"series":series_json(&v,true),"episodes":episodes_json(&eps),
                    "episodeFile":{"id":i(payload,"episode_file_id"),"path":path,"relativePath":relative(&path,&v.path),"sourcePath":source,"size":i(payload,"size")}});
            } else if let (MediaDomain::Movies, Some(id)) = (domain, event.subject_id) {
                let v = movie(c, id, payload).await?;
                movie_env(&mut env, &v, true);
                env.push((
                    "radarr_moviefile_id".into(),
                    i(payload, "movie_file_id")
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "radarr_moviefile_relativepath".into(),
                    relative(&path, &v.path),
                ));
                env.push(("radarr_moviefile_path".into(), path.clone()));
                env.push(("radarr_moviefile_sourcepath".into(), source.clone()));
                env.push(("radarr_moviefile_sourcefolder".into(), folder(&source)));
                title = format!("Movie {verb}");
                text = movie_label(&v);
                json = json!({"eventType":"Download","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"movies",
                    "isUpgrade":is_upgrade,"movie":movie_json(&v,true),
                    "movieFile":{"id":i(payload,"movie_file_id"),"path":path,"relativePath":relative(&path,&v.path),"sourcePath":source,"size":i(payload,"size")}});
            } else {
                return Err(BuildError::Storage);
            }
            paths = vec![path];
            severity = None;
        }
        (EventKind::Add | EventKind::Delete, _) => {
            let add = event.kind == EventKind::Add;
            let (tv_type, movie_type) = if add {
                ("SeriesAdd", "MovieAdded")
            } else {
                ("SeriesDelete", "MovieDelete")
            };
            if let (MediaDomain::Tv, Some(id)) = (domain, event.subject_id) {
                let mut v = series(c, id, payload).await?;
                if event.kind == EventKind::Delete
                    && let Some(ids) = event.tag_ids.as_deref()
                {
                    v.tags = snapshot_tags(c, domain, ids).await?;
                }
                env = base_env(domain, tv_type);
                tv_env(&mut env, &v, !add);
                title = if add {
                    "Series Added"
                } else {
                    "Series Deleted"
                }
                .into();
                text = v.title.clone();
                json = json!({"eventType":tv_type,"instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"tv","series":series_json(&v,!add)});
                paths = if add {
                    Vec::new()
                } else {
                    vec![v.path.clone()]
                };
            } else if let (MediaDomain::Movies, Some(id)) = (domain, event.subject_id) {
                let mut v = movie(c, id, payload).await?;
                if event.kind == EventKind::Delete
                    && let Some(ids) = event.tag_ids.as_deref()
                {
                    v.tags = snapshot_tags(c, domain, ids).await?;
                }
                env = base_env(domain, movie_type);
                movie_env(&mut env, &v, true);
                title = if add { "Movie Added" } else { "Movie Deleted" }.into();
                text = movie_label(&v);
                json = json!({"eventType":movie_type,"instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"movies","movie":movie_json(&v,true)});
                paths = if add {
                    Vec::new()
                } else {
                    vec![v.path.clone()]
                };
            } else {
                return Err(BuildError::Storage);
            }
            severity = None;
        }
        (EventKind::FileDelete, _) => {
            let path = s(payload, "path").unwrap_or_default();
            let reason = s(payload, "reason").unwrap_or_else(|| "Manual".into());
            if let (MediaDomain::Tv, Some(id)) = (domain, event.subject_id) {
                let v = series(c, id, payload).await?;
                env = base_env(domain, "EpisodeFileDelete");
                env.push(("sonarr_episodefile_deletereason".into(), reason.clone()));
                tv_env(&mut env, &v, true);
                env.push((
                    "sonarr_episodefile_id".into(),
                    i(payload, "episode_file_id")
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "sonarr_episodefile_relativepath".into(),
                    relative(&path, &v.path),
                ));
                env.push(("sonarr_episodefile_path".into(), path.clone()));
                title = "Episode File Deleted".into();
                text = format!("{}\n{}", v.title, path);
                json = json!({"eventType":"EpisodeFileDelete","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"tv",
                    "deleteReason":reason,"series":series_json(&v,true),
                    "episodeFile":{"id":i(payload,"episode_file_id"),"path":path,"relativePath":relative(&path,&v.path)}});
            } else if let (MediaDomain::Movies, Some(id)) = (domain, event.subject_id) {
                let v = movie(c, id, payload).await?;
                env = base_env(domain, "MovieFileDelete");
                env.push(("radarr_moviefile_deletereason".into(), reason.clone()));
                movie_env(&mut env, &v, true);
                env.push((
                    "radarr_moviefile_id".into(),
                    i(payload, "movie_file_id")
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                ));
                env.push((
                    "radarr_moviefile_relativepath".into(),
                    relative(&path, &v.path),
                ));
                env.push(("radarr_moviefile_path".into(), path.clone()));
                title = "Movie File Deleted".into();
                text = format!("{}\n{}", movie_label(&v), path);
                json = json!({"eventType":"MovieFileDelete","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"movies",
                    "deleteReason":reason,"movie":movie_json(&v,true),
                    "movieFile":{"id":i(payload,"movie_file_id"),"path":path,"relativePath":relative(&path,&v.path)}});
            } else {
                return Err(BuildError::Storage);
            }
            paths = vec![path];
            severity = None;
        }
        (EventKind::Rename, _) => {
            // Emitted by the `notification_event_rename_*` triggers when a rename command settles:
            // `paths` and `previous_paths` are parallel arrays of the renamed items.
            let paths_now: Vec<String> = payload
                .get("paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let previous: Vec<String> = payload
                .get("previous_paths")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            env = base_env(domain, "Rename");
            if let (MediaDomain::Tv, Some(id)) = (domain, event.subject_id) {
                let v = series(c, id, payload).await?;
                tv_env(&mut env, &v, true);
                env.push(("sonarr_episodefile_paths".into(), paths_now.join("|")));
                env.push((
                    "sonarr_episodefile_previouspaths".into(),
                    previous.join("|"),
                ));
                title = "Series Renamed".into();
                text = v.title.clone();
                json = json!({"eventType":"Rename","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"tv",
                    "series":series_json(&v,true),"renamedEpisodeFiles":paths_now.iter().zip(previous.iter()).map(|(n,p)|json!({"path":n,"previousPath":p})).collect::<Vec<_>>()});
            } else if let (MediaDomain::Movies, Some(id)) = (domain, event.subject_id) {
                let v = movie(c, id, payload).await?;
                movie_env(&mut env, &v, true);
                env.push(("radarr_moviefile_paths".into(), paths_now.join("|")));
                env.push(("radarr_moviefile_previouspaths".into(), previous.join("|")));
                title = "Movie Renamed".into();
                text = movie_label(&v);
                json = json!({"eventType":"Rename","instanceName":INSTANCE_NAME,"applicationUrl":"","mediaType":"movies",
                    "movie":movie_json(&v,true),"renamedMovieFiles":paths_now.iter().zip(previous.iter()).map(|(n,p)|json!({"path":n,"previousPath":p})).collect::<Vec<_>>()});
            } else {
                return Err(BuildError::Storage);
            }
            paths = paths_now;
            severity = None;
        }
        (EventKind::HealthIssue | EventKind::HealthRestored, _) => {
            let restored = event.kind == EventKind::HealthRestored;
            let level = match i(payload, "severity") {
                Some(3) => "Error",
                Some(2) => "Warning",
                Some(1) => "Notice",
                _ => "Ok",
            };
            let message = s(payload, "message").unwrap_or_default();
            let kind_type = s(payload, "type").unwrap_or_default();
            let wiki = s(payload, "wiki_url").unwrap_or_default();
            let (event_type, key) = if restored {
                ("HealthRestored", "restored")
            } else {
                ("HealthIssue", "issue")
            };
            env = base_env(domain, event_type);
            env.push((format!("{p}_health_{key}_level"), level.into()));
            env.push((format!("{p}_health_{key}_message"), message.clone()));
            env.push((format!("{p}_health_{key}_type"), kind_type.clone()));
            env.push((format!("{p}_health_{key}_wiki"), wiki.clone()));
            title = if restored {
                "Health Check Restored"
            } else {
                "Health Check Failure"
            }
            .into();
            text = message.clone();
            json = json!({"eventType":event_type,"instanceName":INSTANCE_NAME,"applicationUrl":"",
                "level":level,"message":message,"type":kind_type,"wikiUrl":wiki,"checkKey":s(payload,"check_key"),
                "scope":event.scope.as_str(),"mediaType":match domain {MediaDomain::Tv=>"tv",MediaDomain::Movies=>"movies"}});
            paths = Vec::new();
            severity = i(payload, "severity");
        }
        (EventKind::ApplicationUpdate, _) => {
            let message = s(payload, "message").unwrap_or_default();
            let new_version = s(payload, "new_version").unwrap_or_default();
            let previous = s(payload, "previous_version").unwrap_or_default();
            env = base_env(domain, "ApplicationUpdate");
            env.push((format!("{p}_update_message"), message.clone()));
            env.push((format!("{p}_update_newversion"), new_version.clone()));
            env.push((format!("{p}_update_previousversion"), previous.clone()));
            title = "Application Updated".into();
            text = message.clone();
            json = json!({"eventType":"ApplicationUpdate","instanceName":INSTANCE_NAME,"applicationUrl":"","message":message,"previousVersion":previous,"newVersion":new_version});
            paths = Vec::new();
            severity = None;
        }
        (EventKind::Test, _) => return Ok(test_message(domain)),
    }
    Ok(Message {
        kind: event.kind,
        domain,
        title,
        text,
        json,
        env,
        paths,
        severity,
    })
}

/// Records an event that no database trigger captures (rename, application update). Callers
/// must hold the transaction that made the change. Returns the new outbox sequence, or `None`
/// when no enabled notification could consume it.
pub async fn emit(
    c: &Connection,
    kind: EventKind,
    scope: Scope,
    subject_id: Option<i64>,
    payload: &Value,
) -> Result<Option<i64>, libsql::Error> {
    let flag = match kind {
        EventKind::Rename => "on_rename",
        EventKind::ApplicationUpdate => "on_application_update",
        EventKind::Grab => "on_grab",
        EventKind::Import => "on_import",
        EventKind::Add => "on_add",
        EventKind::Delete => "on_delete",
        EventKind::FileDelete => "on_file_delete",
        EventKind::HealthIssue => "on_health_issue",
        EventKind::HealthRestored => "on_health_restored",
        EventKind::ManualInteraction => "on_manual_interaction",
        EventKind::Test => return Ok(None),
    };
    // `flag` is chosen from the closed list above, never from input.
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM notification_scopes s JOIN notifications n ON n.id=s.notification_id WHERE n.enabled=1 AND (?1='system' OR s.media_type=?1) AND s.{flag}=1)"
    );
    let mut rows = c.query(&sql, params![scope.as_str()]).await?;
    let consumable: i64 = rows.next().await?.map_or(Ok(0), |r| r.get(0))?;
    if consumable == 0 {
        return Ok(None);
    }
    c.execute(
        "INSERT INTO notification_events(kind,scope,subject_id,payload_json,created_at) VALUES(?,?,?,?,CAST(strftime('%s','now') AS INTEGER))",
        params![
            kind.as_str(),
            scope.as_str(),
            subject_id,
            payload.to_string()
        ],
    )
    .await?;
    let mut rows = c.query("SELECT last_insert_rowid()", ()).await?;
    rows.next().await?.map(|r| r.get(0)).transpose()
}
