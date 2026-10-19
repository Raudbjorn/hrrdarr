//! Validation and persistence of import-list definitions.
use super::{
    catalog,
    error::{ApiError, Result},
    model::*,
    providers::ListRuntime,
};
use crate::{
    api::MediaDomain,
    db::Database,
    library::{Availability, SeriesType},
    providers::CredentialKey,
};
use axum::http::StatusCode;
use libsql::{Connection, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uuid::Uuid;

const MAX_SAFE: i64 = 9_007_199_254_740_991;
const MAX_INTERVAL_MINUTES: i64 = 43_200;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}
pub fn domain_name(d: MediaDomain) -> &'static str {
    match d {
        MediaDomain::Tv => "tv",
        MediaDomain::Movies => "movies",
    }
}
fn enum_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A definition that passed schema validation, with defaults applied.
pub struct Validated {
    pub name: String,
    pub implementation: &'static str,
    pub media: MediaDomain,
    pub enabled: bool,
    pub enable_automatic_add: bool,
    pub search_on_add: bool,
    pub monitor: Monitor,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub minimum_availability: Option<String>,
    pub series_type: Option<String>,
    pub season_folder: Option<bool>,
    pub interval_minutes: i64,
    pub tag_ids: Vec<i64>,
    pub settings: BTreeMap<String, SettingValue>,
    /// `Some(value)` sets, `None` clears; only names present in the request.
    pub secret_changes: BTreeMap<String, Option<String>>,
}

fn no_control(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

fn validate_setting(field: &Field, value: &SettingValue) -> Result<()> {
    let bad = |what: &str| ApiError::invalid(format!("Setting {} {what}", field.name));
    use FieldKind::*;
    match (field.kind, value) {
        (Boolean, SettingValue::Bool(_)) => Ok(()),
        (Number, SettingValue::Int(n)) => {
            let (min, max) = (field.min.unwrap_or(i64::MIN), field.max.unwrap_or(i64::MAX));
            if (min..=max).contains(n) {
                Ok(())
            } else {
                Err(bad(&format!("must be between {min} and {max}")))
            }
        }
        (Text | Url | Select, SettingValue::Text(text)) => {
            if text.len() > field.max_length.unwrap_or(512) as usize || !no_control(text) {
                return Err(bad("is too long or contains control characters"));
            }
            if text != text.trim() {
                return Err(bad("must not have leading or trailing whitespace"));
            }
            match field.kind {
                Select if !field.options.contains(&text.as_str()) => {
                    Err(bad("is not one of the permitted options"))
                }
                Url if !text.is_empty() => crate::notifications::transport::validate_url(text)
                    .map(|_| ())
                    .map_err(|e| {
                        bad(
                            if e == crate::notifications::transport::SendError::PolicyDenied {
                                "targets a denied address range"
                            } else {
                                "must be an http(s) URL without credentials or fragment"
                            },
                        )
                    }),
                _ => Ok(()),
            }
        }
        _ => Err(bad("has the wrong type")),
    }
}

/// `stored_secrets` are the secret names already configured (empty on create).
pub fn validate(input: &ImportListInput, stored_secrets: &BTreeSet<String>) -> Result<Validated> {
    let name = input.name.as_str();
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name != name.trim() || !no_control(name) {
        return Err(ApiError::invalid(
            "Name requires 1 to 128 bytes without control characters or surrounding whitespace",
        ));
    }
    let implementation = catalog::find(&input.implementation).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "unknown_import_list_implementation",
            "Unknown import list implementation",
        )
    })?;
    let media = input.media_type;
    if !implementation.domains.contains(&media) {
        return Err(ApiError::invalid(format!(
            "{} does not support the {} domain",
            implementation.label,
            domain_name(media)
        )));
    }
    let tv = media == MediaDomain::Tv;
    if !tv && !matches!(input.monitor, Monitor::All | Monitor::None) {
        return Err(ApiError::invalid("Movie lists monitor either all or none"));
    }
    if tv && input.minimum_availability.is_some() {
        return Err(ApiError::invalid("Minimum availability applies to movie lists"));
    }
    if !tv && (input.series_type.is_some() || input.season_folder.is_some()) {
        return Err(ApiError::invalid("Series type and season folder apply to TV lists"));
    }
    let interval = input.interval_minutes.unwrap_or(implementation.default_interval_minutes);
    if !(implementation.min_interval_minutes..=MAX_INTERVAL_MINUTES).contains(&interval) {
        return Err(ApiError::invalid(format!(
            "Interval must be {} to {MAX_INTERVAL_MINUTES} minutes",
            implementation.min_interval_minutes
        )));
    }
    for id in [input.quality_profile_id, input.root_folder_id].into_iter().flatten() {
        if !(1..=MAX_SAFE).contains(&id) {
            return Err(ApiError::invalid("Profile and root folder IDs must be positive"));
        }
    }
    if input.enabled && input.enable_automatic_add
        && (input.quality_profile_id.is_none() || input.root_folder_id.is_none())
    {
        return Err(ApiError::invalid(
            "Automatic add requires a quality profile and a root folder",
        ));
    }
    if input.tag_ids.len() > MAX_TAGS
        || input.tag_ids.iter().any(|id| !(1..=MAX_SAFE).contains(id))
        || input.tag_ids.iter().collect::<BTreeSet<_>>().len() != input.tag_ids.len()
    {
        return Err(ApiError::invalid(format!(
            "Tags require at most {MAX_TAGS} distinct positive IDs"
        )));
    }
    let fields = (implementation.fields)();
    let by_name: BTreeMap<&str, &Field> = fields.iter().map(|f| (f.name, f)).collect();
    for key in input.settings.keys() {
        match by_name.get(key.as_str()) {
            Some(f) if !f.secret => {}
            Some(_) => {
                return Err(ApiError::invalid(format!(
                    "Setting {key} is secret and must be sent in secrets"
                )));
            }
            None => return Err(ApiError::invalid(format!("Unknown setting {key}"))),
        }
    }
    for (key, value) in &input.secrets {
        match by_name.get(key.as_str()) {
            Some(f) if f.secret => {
                if let Some(value) = value {
                    if value.len() > f.max_length.unwrap_or(1024) as usize
                        || !no_control(value)
                        || value != value.trim()
                    {
                        return Err(ApiError::invalid(format!(
                            "Secret {key} is too long or contains control characters or surrounding whitespace"
                        )));
                    }
                }
            }
            Some(_) => {
                return Err(ApiError::invalid(format!(
                    "Setting {key} is not secret and must be sent in settings"
                )));
            }
            None => return Err(ApiError::invalid(format!("Unknown secret {key}"))),
        }
    }
    let mut settings = BTreeMap::new();
    for field in fields.iter().filter(|f| !f.secret) {
        let value = input.settings.get(field.name).cloned().or_else(|| field.default.clone());
        match value {
            Some(SettingValue::Text(t)) if t.is_empty() && field.kind != FieldKind::Select => {
                if field.required {
                    return Err(ApiError::invalid(format!("Setting {} is required", field.name)));
                }
            }
            Some(value) => {
                validate_setting(field, &value)?;
                settings.insert(field.name.to_owned(), value);
            }
            None if field.required => {
                return Err(ApiError::invalid(format!("Setting {} is required", field.name)));
            }
            None => {}
        }
    }
    let secret_changes: BTreeMap<String, Option<String>> = input
        .secrets
        .iter()
        .map(|(k, v)| (k.clone(), v.clone().filter(|v| !v.is_empty())))
        .collect();
    for field in fields.iter().filter(|f| f.secret && f.required) {
        let after = match secret_changes.get(field.name) {
            Some(change) => change.is_some(),
            None => stored_secrets.contains(field.name),
        };
        if !after {
            return Err(ApiError::invalid(format!("Secret {} is required", field.name)));
        }
    }
    Ok(Validated {
        name: name.to_owned(),
        implementation: implementation.name,
        media,
        enabled: input.enabled,
        enable_automatic_add: input.enable_automatic_add,
        search_on_add: input.search_on_add,
        monitor: input.monitor,
        quality_profile_id: input.quality_profile_id,
        root_folder_id: input.root_folder_id,
        minimum_availability: if tv {
            None
        } else {
            Some(enum_text(&input.minimum_availability.clone().unwrap_or(Availability::Released)))
        },
        series_type: tv.then(|| enum_text(&input.series_type.clone().unwrap_or(SeriesType::Standard))),
        season_folder: tv.then(|| input.season_folder.unwrap_or(true)),
        interval_minutes: interval,
        tag_ids: input.tag_ids.clone(),
        settings,
        secret_changes,
    })
}

fn enum_of<T: serde::de::DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|t| serde_json::from_value(serde_json::Value::String(t)).ok())
}
fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| ApiError::invalid("Import list ID must be a UUID"))
}
fn name_conflict() -> ApiError {
    ApiError::conflict(
        "import_list_name_conflict",
        "An import list with this name already exists in this media domain",
    )
}

pub struct Store {
    pub db: Arc<Database>,
    pub key: Option<Arc<CredentialKey>>,
}
struct Sealed {
    names: Vec<String>,
    blob: Option<Vec<u8>>,
}

impl Store {
    pub async fn list(&self) -> Result<Vec<ImportList>> {
        let c = self.db.connect().await?;
        let mut ids = Vec::new();
        let mut rows = c
            .query("SELECT id FROM import_lists ORDER BY media_type,lower(name),id", ())
            .await?;
        while let Some(r) = rows.next().await? {
            ids.push(r.get::<String>(0)?);
        }
        let mut out = Vec::new();
        for id in ids {
            if let Some(list) = read(&c, &id).await? {
                out.push(list);
            }
        }
        Ok(out)
    }
    pub async fn get(&self, id: &str) -> Result<ImportList> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn create(&self, input: &ImportListInput) -> Result<ImportList> {
        let v = validate(input, &BTreeSet::new())?;
        let id = Uuid::new_v4().to_string();
        let sealed = self.seal(&id, &v, &BTreeMap::new())?;
        let c = self.db.connect().await?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).await?;
        let count = scalar(&tx, "SELECT count(*) FROM import_lists", ()).await?;
        if count >= MAX_LISTS {
            return Err(ApiError::conflict(
                "import_list_limit",
                format!("At most {MAX_LISTS} import lists may be configured"),
            ));
        }
        let taken = scalar(
            &tx,
            "SELECT count(*) FROM import_lists WHERE media_type=? AND lower(name)=lower(?)",
            params![domain_name(v.media), v.name.clone()],
        )
        .await?;
        if taken > 0 {
            return Err(name_conflict());
        }
        check_references(&tx, &v).await?;
        let t = now();
        tx.execute(
            "INSERT INTO import_lists(id,media_type,name,implementation,enabled,enable_automatic_add,search_on_add,monitor,quality_profile_id,root_folder_id,minimum_availability,series_type,season_folder,interval_minutes,revision,settings_json,secret_names,credentials,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,1,?,?,?,?,?)",
            params![
                id.clone(),
                domain_name(v.media),
                v.name.clone(),
                v.implementation,
                i64::from(v.enabled),
                i64::from(v.enable_automatic_add),
                i64::from(v.search_on_add),
                v.monitor.text(),
                v.quality_profile_id,
                v.root_folder_id,
                v.minimum_availability.clone(),
                v.series_type.clone(),
                v.season_folder.map(i64::from),
                v.interval_minutes,
                serde_json::to_string(&v.settings).map_err(|_| ApiError::invalid("Settings cannot be encoded"))?,
                serde_json::to_string(&sealed.names).unwrap_or_else(|_| "[]".into()),
                sealed.blob,
                t,
                t
            ],
        )
        .await?;
        write_tags(&tx, &id, &v).await?;
        tx.commit().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn update(&self, id: &str, input: &ImportListInput) -> Result<ImportList> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate).await?;
        let (implementation, media, revision, names_json, blob) = {
            let mut rows = tx
                .query(
                    "SELECT implementation,media_type,revision,secret_names,credentials FROM import_lists WHERE id=?",
                    [id.clone()],
                )
                .await?;
            let r = rows.next().await?.ok_or_else(ApiError::not_found)?;
            (
                r.get::<String>(0)?,
                r.get::<String>(1)?,
                r.get::<i64>(2)?,
                r.get::<String>(3)?,
                r.get::<Option<Vec<u8>>>(4)?,
            )
        };
        if input.implementation != implementation || domain_name(input.media_type) != media {
            return Err(ApiError::invalid(
                "The implementation and media domain of an import list cannot change",
            ));
        }
        let Some(expected) = input.revision else {
            return Err(ApiError::invalid("Revision is required"));
        };
        if expected != revision {
            return Err(ApiError::conflict(
                "import_list_revision_conflict",
                "The import list changed; reload it and retry",
            ));
        }
        let stored: BTreeSet<String> = serde_json::from_str::<Vec<String>>(&names_json)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let v = validate(input, &stored)?;
        let existing = match (&blob, v.secret_changes.is_empty()) {
            (_, true) => None,
            (Some(blob), false) => Some(self.open(&id, &implementation, blob)?),
            (None, false) => Some(BTreeMap::new()),
        };
        let (names, new_blob): (Vec<String>, Option<Vec<u8>>) = if v.secret_changes.is_empty() {
            (stored.into_iter().collect(), blob)
        } else {
            let sealed = self.seal(&id, &v, &existing.unwrap_or_default())?;
            (sealed.names, sealed.blob)
        };
        let taken = scalar(
            &tx,
            "SELECT count(*) FROM import_lists WHERE media_type=? AND lower(name)=lower(?) AND id!=?",
            params![media.clone(), v.name.clone(), id.clone()],
        )
        .await?;
        if taken > 0 {
            return Err(name_conflict());
        }
        check_references(&tx, &v).await?;
        tx.execute(
            "UPDATE import_lists SET name=?,enabled=?,enable_automatic_add=?,search_on_add=?,monitor=?,quality_profile_id=?,root_folder_id=?,minimum_availability=?,series_type=?,season_folder=?,interval_minutes=?,revision=revision+1,settings_json=?,secret_names=?,credentials=?,updated_at=? WHERE id=?",
            params![
                v.name.clone(),
                i64::from(v.enabled),
                i64::from(v.enable_automatic_add),
                i64::from(v.search_on_add),
                v.monitor.text(),
                v.quality_profile_id,
                v.root_folder_id,
                v.minimum_availability.clone(),
                v.series_type.clone(),
                v.season_folder.map(i64::from),
                v.interval_minutes,
                serde_json::to_string(&v.settings).map_err(|_| ApiError::invalid("Settings cannot be encoded"))?,
                serde_json::to_string(&names).unwrap_or_else(|_| "[]".into()),
                new_blob,
                now(),
                id.clone()
            ],
        )
        .await?;
        tx.execute("DELETE FROM import_list_tags WHERE list_id=?", [id.clone()]).await?;
        write_tags(&tx, &id, &v).await?;
        tx.commit().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        let n = c.execute("DELETE FROM import_lists WHERE id=?", [id]).await?;
        if n == 0 {
            return Err(ApiError::not_found());
        }
        Ok(())
    }

    /// Stored definition with decrypted secrets, for fetching.
    pub async fn runtime(&self, c: &Connection, id: &str) -> Result<Option<(ImportList, ListRuntime)>> {
        let Some(list) = read(c, id).await? else {
            return Ok(None);
        };
        let mut rows = c
            .query("SELECT credentials FROM import_lists WHERE id=?", [id.to_owned()])
            .await?;
        let blob = match rows.next().await? {
            Some(r) => r.get::<Option<Vec<u8>>>(0)?,
            None => return Ok(None),
        };
        let secrets = match blob {
            Some(blob) => self.open(id, &list.implementation, &blob)?,
            None => BTreeMap::new(),
        };
        let runtime = ListRuntime {
            id: list.id,
            media: list.media_type,
            implementation: list.implementation.clone(),
            settings: list.settings.clone(),
            secrets,
        };
        Ok(Some((list, runtime)))
    }

    /// A draft merged with the stored secrets of `existing` (for test-before-save).
    pub async fn draft_runtime(&self, input: &ImportListInput, existing: Option<Uuid>) -> Result<ListRuntime> {
        let c = self.db.connect().await?;
        let (mut secrets, stored, id) = match existing {
            Some(id) => {
                let (list, rt) = self
                    .runtime(&c, &id.to_string())
                    .await?
                    .ok_or_else(ApiError::not_found)?;
                if list.implementation != input.implementation || list.media_type != input.media_type {
                    return Err(ApiError::invalid(
                        "The implementation and media domain of an import list cannot change",
                    ));
                }
                let names: BTreeSet<String> = rt.secrets.keys().cloned().collect();
                (rt.secrets, names, id)
            }
            None => (BTreeMap::new(), BTreeSet::new(), Uuid::nil()),
        };
        let v = validate(input, &stored)?;
        for (name, change) in &v.secret_changes {
            match change {
                Some(value) => {
                    secrets.insert(name.clone(), value.clone());
                }
                None => {
                    secrets.remove(name);
                }
            }
        }
        Ok(ListRuntime {
            id,
            media: v.media,
            implementation: v.implementation.to_owned(),
            settings: v.settings,
            secrets,
        })
    }

    fn seal(&self, id: &str, v: &Validated, existing: &BTreeMap<String, String>) -> Result<Sealed> {
        let mut merged = existing.clone();
        for (name, change) in &v.secret_changes {
            match change {
                Some(value) => {
                    merged.insert(name.clone(), value.clone());
                }
                None => {
                    merged.remove(name);
                }
            }
        }
        let names: Vec<String> = merged.keys().cloned().collect();
        if merged.is_empty() {
            return Ok(Sealed { names, blob: None });
        }
        let key = self.key.as_ref().ok_or_else(ApiError::key_unavailable)?;
        let plain = serde_json::to_vec(&merged).map_err(|_| ApiError::invalid("Secrets cannot be encoded"))?;
        let blob = key
            .seal_import_list(id, v.implementation, &plain)
            .map_err(|_| ApiError::key_unavailable())?;
        Ok(Sealed { names, blob: Some(blob) })
    }
    fn open(&self, id: &str, implementation: &str, blob: &[u8]) -> Result<BTreeMap<String, String>> {
        let key = self.key.as_ref().ok_or_else(ApiError::key_unavailable)?;
        let plain = key
            .open_import_list(id, implementation, blob)
            .map_err(|_| ApiError::locked())?;
        serde_json::from_slice(&plain).map_err(|_| ApiError::locked())
    }
}

pub async fn scalar(c: &Connection, sql: &str, p: impl libsql::params::IntoParams) -> Result<i64> {
    let mut rows = c.query(sql, p).await?;
    Ok(rows.next().await?.map_or(Ok(0), |r| r.get::<i64>(0))?)
}

/// Profile, root folder and tags must exist in the list's own domain.
async fn check_references(c: &Connection, v: &Validated) -> Result<()> {
    let media = domain_name(v.media);
    if let Some(profile) = v.quality_profile_id {
        let n = scalar(
            c,
            "SELECT count(*) FROM quality_profiles WHERE id=? AND media_type=?",
            params![profile, media],
        )
        .await?;
        if n == 0 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "quality_profile_not_found",
                "The quality profile does not exist in this media domain",
            ));
        }
    }
    if let Some(root) = v.root_folder_id {
        let n = scalar(
            c,
            "SELECT count(*) FROM root_folders WHERE id=? AND media_type=?",
            params![root, media],
        )
        .await?;
        if n == 0 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "root_folder_not_found",
                "The root folder does not exist in this media domain",
            ));
        }
    }
    if !v.tag_ids.is_empty() {
        let mut values = vec![libsql::Value::Text(media.to_owned())];
        values.extend(v.tag_ids.iter().map(|t| libsql::Value::Integer(*t)));
        let known = scalar(
            c,
            &format!(
                "SELECT count(*) FROM tags WHERE media_type=? AND id IN ({})",
                v.tag_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            ),
            libsql::params_from_iter(values),
        )
        .await?;
        if known != v.tag_ids.len() as i64 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "tag_not_found",
                "A tag does not exist in this media domain",
            ));
        }
    }
    Ok(())
}

async fn write_tags(c: &Connection, id: &str, v: &Validated) -> Result<()> {
    for tag in &v.tag_ids {
        c.execute(
            "INSERT INTO import_list_tags(list_id,media_type,tag_id) VALUES(?,?,?)",
            params![id, domain_name(v.media), *tag],
        )
        .await?;
    }
    Ok(())
}

pub async fn read(c: &Connection, id: &str) -> Result<Option<ImportList>> {
    let mut rows = c
        .query(
            "SELECT name,implementation,media_type,enabled,enable_automatic_add,search_on_add,monitor,quality_profile_id,root_folder_id,minimum_availability,series_type,season_folder,interval_minutes,revision,settings_json,secret_names,last_sync_at,last_success_at,last_error,created_at,updated_at FROM import_lists WHERE id=?",
            [id.to_owned()],
        )
        .await?;
    let Some(r) = rows.next().await? else {
        return Ok(None);
    };
    let media = MediaDomain::parse(&r.get::<String>(2)?).map_err(|_| ApiError::not_found())?;
    let mut list = ImportList {
        id: Uuid::parse_str(id).map_err(|_| ApiError::not_found())?,
        name: r.get(0)?,
        implementation: r.get(1)?,
        media_type: media,
        enabled: r.get::<i64>(3)? == 1,
        enable_automatic_add: r.get::<i64>(4)? == 1,
        search_on_add: r.get::<i64>(5)? == 1,
        monitor: Monitor::parse(&r.get::<String>(6)?).unwrap_or_default(),
        quality_profile_id: r.get(7)?,
        root_folder_id: r.get(8)?,
        minimum_availability: enum_of::<Availability>(r.get::<Option<String>>(9)?),
        series_type: enum_of::<SeriesType>(r.get::<Option<String>>(10)?),
        season_folder: r.get::<Option<i64>>(11)?.map(|v| v == 1),
        interval_minutes: r.get(12)?,
        tag_ids: Vec::new(),
        revision: r.get(13)?,
        settings: serde_json::from_str(&r.get::<String>(14)?).unwrap_or_default(),
        secrets_configured: serde_json::from_str(&r.get::<String>(15)?).unwrap_or_default(),
        last_sync_at: r.get(16)?,
        last_success_at: r.get(17)?,
        last_error: r.get(18)?,
        created_at: r.get(19)?,
        updated_at: r.get(20)?,
    };
    let mut tags = c
        .query(
            "SELECT tag_id FROM import_list_tags WHERE list_id=? ORDER BY tag_id",
            [id.to_owned()],
        )
        .await?;
    while let Some(row) = tags.next().await? {
        list.tag_ids.push(row.get::<i64>(0)?);
    }
    Ok(Some(list))
}
