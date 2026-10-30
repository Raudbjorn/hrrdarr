//! Validation and persistence of notification definitions.
use super::{
    catalog::{self},
    error::{ApiError, Result},
    model::*,
};
use crate::{api::MediaDomain, db::Database, providers::CredentialKey};
use axum::http::StatusCode;
use libsql::{Connection, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const MAX_SAFE: i64 = 9_007_199_254_740_991;

/// A definition that passed schema validation, with defaults applied.
pub struct Validated {
    pub name: String,
    pub implementation: &'static str,
    pub label: &'static str,
    pub enabled: bool,
    pub settings: BTreeMap<String, SettingValue>,
    /// `Some(value)` sets, `None` clears; only names present in the request.
    pub secret_changes: BTreeMap<String, Option<String>>,
    pub tv: Option<TvScope>,
    pub movies: Option<MovieScope>,
}

fn no_control(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

fn check_flags(flags: Flags, domain: &str) -> Result<()> {
    if flags.on_upgrade && !flags.on_import {
        return Err(ApiError::invalid(format!(
            "{domain}: on_upgrade requires on_import, because an upgrade is an import that replaced a file"
        )));
    }
    if flags.on_file_delete_for_upgrade && !flags.on_file_delete {
        return Err(ApiError::invalid(format!(
            "{domain}: the file-delete-for-upgrade trigger requires the file-delete trigger"
        )));
    }
    Ok(())
}
fn check_tags(ids: &[i64], domain: &str) -> Result<()> {
    if ids.len() > MAX_TAGS_PER_SCOPE
        || ids.iter().any(|id| !(1..=MAX_SAFE).contains(id))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err(ApiError::invalid(format!(
            "{domain}: tags require at most {MAX_TAGS_PER_SCOPE} distinct positive safe IDs"
        )));
    }
    Ok(())
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
        (Text | Url | Path | Select, SettingValue::Text(text)) => {
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
                Url if !text.is_empty() => super::transport::validate_url(text)
                    .map(|_| ())
                    .map_err(|e| {
                        bad(if e == super::transport::SendError::PolicyDenied {
                            "targets a denied address range"
                        } else {
                            "must be an http(s) URL without credentials or fragment"
                        })
                    }),
                Path if !text.is_empty() => super::script::validate_path(text)
                    .map_err(|_| bad("must be an absolute normalized path")),
                _ => Ok(()),
            }
        }
        _ => Err(bad("has the wrong type")),
    }
}

/// `stored_secrets` are the secret names already configured (empty on create).
pub fn validate(input: &NotificationInput, stored_secrets: &BTreeSet<String>) -> Result<Validated> {
    let name = input.name.as_str();
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name != name.trim() || !no_control(name) {
        return Err(ApiError::invalid(
            "Name requires 1 to 128 bytes without control characters or surrounding whitespace",
        ));
    }
    let implementation = catalog::find(&input.implementation).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "unknown_notification_implementation",
            "Unknown notification implementation",
        )
    })?;
    if input.tv.is_none() && input.movies.is_none() {
        return Err(ApiError::invalid(
            "Enable at least one media domain scope (tv or movies)",
        ));
    }
    for (present, domain, label) in [
        (input.tv.is_some(), MediaDomain::Tv, "tv"),
        (input.movies.is_some(), MediaDomain::Movies, "movies"),
    ] {
        if present && !implementation.domains.contains(&domain) {
            return Err(ApiError::invalid(format!(
                "{} does not support the {label} domain",
                implementation.label
            )));
        }
    }
    if let Some(tv) = &input.tv {
        check_flags(Flags::from(&tv.triggers), "tv")?;
        check_tags(&tv.tag_ids, "tv")?;
    }
    if let Some(movies) = &input.movies {
        check_flags(Flags::from(&movies.triggers), "movies")?;
        check_tags(&movies.tag_ids, "movies")?;
    }
    let fields = (implementation.fields)();
    let by_name: BTreeMap<&str, &Field> = fields.iter().map(|f| (f.name, f)).collect();
    // Unknown keys and secrets smuggled into settings (or vice versa) are rejected, not ignored.
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
                    // Secret URLs get the same scheme and host policy as plain URL settings, at
                    // write time rather than as a permanent failure on the first delivery.
                    if f.kind == FieldKind::Url {
                        validate_setting(f, &SettingValue::Text(value.clone()))?;
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
        let value = input
            .settings
            .get(field.name)
            .cloned()
            .or_else(|| field.default.clone());
        match value {
            Some(SettingValue::Text(t)) if t.is_empty() && field.kind != FieldKind::Select => {
                if field.required {
                    return Err(ApiError::invalid(format!(
                        "Setting {} is required",
                        field.name
                    )));
                }
            }
            Some(value) => {
                validate_setting(field, &value)?;
                settings.insert(field.name.to_owned(), value);
            }
            None if field.required => {
                return Err(ApiError::invalid(format!(
                    "Setting {} is required",
                    field.name
                )));
            }
            None => {}
        }
    }
    let secret_changes: BTreeMap<String, Option<String>> = input
        .secrets
        .iter()
        // An empty string means "no value", the same as clearing.
        .map(|(k, v)| (k.clone(), v.clone().filter(|v| !v.is_empty())))
        .collect();
    for field in fields.iter().filter(|f| f.secret && f.required) {
        let after = match secret_changes.get(field.name) {
            Some(change) => change.is_some(),
            None => stored_secrets.contains(field.name),
        };
        if !after {
            return Err(ApiError::invalid(format!(
                "Secret {} is required",
                field.name
            )));
        }
    }
    Ok(Validated {
        name: name.to_owned(),
        implementation: implementation.name,
        label: implementation.label,
        enabled: input.enabled,
        settings,
        secret_changes,
        tv: input.tv.clone(),
        movies: input.movies.clone(),
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| ApiError::invalid("Notification ID must be a UUID"))
}

pub struct Store {
    pub db: std::sync::Arc<Database>,
    pub key: Option<std::sync::Arc<CredentialKey>>,
}

/// Runtime view with decrypted secrets; never serialized.
pub struct Runtime {
    pub notification: Notification,
    pub secrets: BTreeMap<String, String>,
}

impl Store {
    pub async fn list(&self) -> Result<Vec<Notification>> {
        let c = self.db.connect().await?;
        let mut out = Vec::new();
        let mut ids = Vec::new();
        let mut rows = c
            .query("SELECT id FROM notifications ORDER BY lower(name),id", ())
            .await?;
        while let Some(r) = rows.next().await? {
            ids.push(r.get::<String>(0)?);
        }
        for id in ids {
            if let Some(n) = read(&c, &id).await? {
                out.push(n);
            }
        }
        Ok(out)
    }
    pub async fn get(&self, id: &str) -> Result<Notification> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn create(&self, input: &NotificationInput) -> Result<Notification> {
        let v = validate(input, &BTreeSet::new())?;
        let id = Uuid::new_v4().to_string();
        let sealed = self.seal(&id, &v, &BTreeMap::new())?;
        let c = self.db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let count: i64 = scalar(&tx, "SELECT count(*) FROM notifications", ()).await?;
        if count >= MAX_NOTIFICATIONS as i64 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "notification_limit",
                format!("At most {MAX_NOTIFICATIONS} notifications may be configured"),
            ));
        }
        let taken: i64 = scalar(
            &tx,
            "SELECT count(*) FROM notifications WHERE lower(name)=lower(?)",
            [v.name.clone()],
        )
        .await?;
        if taken > 0 {
            return Err(name_conflict());
        }
        let t = now();
        tx.execute(
            "INSERT INTO notifications(id,name,implementation,enabled,revision,settings_json,secret_names,credentials,created_at,updated_at) VALUES(?,?,?,?,1,?,?,?,?,?)",
            params![
                id.clone(),
                v.name.clone(),
                v.implementation,
                i64::from(v.enabled),
                serde_json::to_string(&v.settings).map_err(|_| ApiError::invalid("Settings cannot be encoded"))?,
                serde_json::to_string(&sealed.names).unwrap_or_else(|_| "[]".into()),
                sealed.blob,
                t,
                t
            ],
        )
        .await?;
        write_scopes(&tx, &id, &v).await?;
        tx.commit().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn update(&self, id: &str, input: &NotificationInput) -> Result<Notification> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let (implementation, revision, names_json, blob) = {
            let mut rows = tx
                .query(
                    "SELECT implementation,revision,secret_names,credentials FROM notifications WHERE id=?",
                    [id.clone()],
                )
                .await?;
            let r = rows.next().await?.ok_or_else(ApiError::not_found)?;
            (
                r.get::<String>(0)?,
                r.get::<i64>(1)?,
                r.get::<String>(2)?,
                r.get::<Option<Vec<u8>>>(3)?,
            )
        };
        if input.implementation != implementation {
            return Err(ApiError::invalid(
                "The implementation of a notification cannot change",
            ));
        }
        let Some(expected) = input.revision else {
            return Err(ApiError::invalid("Revision is required"));
        };
        if expected != revision {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "notification_revision_conflict",
                "The notification changed; reload it and retry",
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
        let taken: i64 = scalar(
            &tx,
            "SELECT count(*) FROM notifications WHERE lower(name)=lower(?) AND id!=?",
            [v.name.clone(), id.clone()],
        )
        .await?;
        if taken > 0 {
            return Err(name_conflict());
        }
        tx.execute(
            "UPDATE notifications SET name=?,enabled=?,revision=revision+1,settings_json=?,secret_names=?,credentials=?,updated_at=? WHERE id=?",
            params![
                v.name.clone(),
                i64::from(v.enabled),
                serde_json::to_string(&v.settings).map_err(|_| ApiError::invalid("Settings cannot be encoded"))?,
                serde_json::to_string(&names).unwrap_or_else(|_| "[]".into()),
                new_blob,
                now(),
                id.clone()
            ],
        )
        .await?;
        tx.execute(
            "DELETE FROM notification_scopes WHERE notification_id=?",
            [id.clone()],
        )
        .await?;
        write_scopes(&tx, &id, &v).await?;
        tx.commit().await?;
        read(&c, &id).await?.ok_or_else(ApiError::not_found)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = parse_id(id)?.to_string();
        let c = self.db.connect().await?;
        let n = c
            .execute("DELETE FROM notifications WHERE id=?", [id])
            .await?;
        if n == 0 {
            return Err(ApiError::not_found());
        }
        Ok(())
    }

    /// Stored definition with decrypted secrets, for delivery and saved-notification tests.
    pub async fn runtime(&self, c: &Connection, id: &str) -> Result<Option<Runtime>> {
        let Some(notification) = read(c, id).await? else {
            return Ok(None);
        };
        catalog::find(&notification.implementation)
            .ok_or_else(|| ApiError::invalid("Unknown notification implementation"))?;
        let mut rows = c
            .query(
                "SELECT credentials FROM notifications WHERE id=?",
                [id.to_owned()],
            )
            .await?;
        let blob = match rows.next().await? {
            Some(r) => r.get::<Option<Vec<u8>>>(0)?,
            None => return Ok(None),
        };
        let secrets = match blob {
            Some(blob) => self.open(id, &notification.implementation, &blob)?,
            None => BTreeMap::new(),
        };
        Ok(Some(Runtime {
            notification,
            secrets,
        }))
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
        let plain = serde_json::to_vec(&merged)
            .map_err(|_| ApiError::invalid("Secrets cannot be encoded"))?;
        let blob = key
            .seal_notification(id, v.implementation, &plain)
            .map_err(|_| ApiError::key_unavailable())?;
        Ok(Sealed {
            names,
            blob: Some(blob),
        })
    }

    fn open(
        &self,
        id: &str,
        implementation: &str,
        blob: &[u8],
    ) -> Result<BTreeMap<String, String>> {
        let key = self.key.as_ref().ok_or_else(ApiError::key_unavailable)?;
        let plain = key.open_notification(id, implementation, blob).map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "notification_credentials_locked",
                "Notification credentials cannot be unlocked; check the independently stored key",
            )
        })?;
        serde_json::from_slice(&plain).map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "notification_credentials_locked",
                "Notification credentials cannot be unlocked; check the independently stored key",
            )
        })
    }
}

struct Sealed {
    names: Vec<String>,
    blob: Option<Vec<u8>>,
}

fn name_conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "notification_name_conflict",
        "A notification with this name already exists",
    )
}

async fn scalar(c: &Connection, sql: &str, p: impl libsql::params::IntoParams) -> Result<i64> {
    let mut rows = c.query(sql, p).await?;
    Ok(rows.next().await?.map_or(Ok(0), |r| r.get::<i64>(0))?)
}

fn tag_params(media: &str, tags: &[i64]) -> Vec<libsql::Value> {
    let mut v = vec![libsql::Value::Text(media.to_owned())];
    v.extend(tags.iter().map(|t| libsql::Value::Integer(*t)));
    v
}

async fn write_scopes(c: &Connection, id: &str, v: &Validated) -> Result<()> {
    let mut scopes: Vec<(&str, Flags, &[i64])> = Vec::new();
    if let Some(tv) = &v.tv {
        scopes.push(("tv", Flags::from(&tv.triggers), &tv.tag_ids));
    }
    if let Some(movies) = &v.movies {
        scopes.push(("movies", Flags::from(&movies.triggers), &movies.tag_ids));
    }
    for (media, f, tags) in scopes {
        c.execute(
            "INSERT INTO notification_scopes(notification_id,media_type,on_grab,on_import,on_upgrade,on_import_complete,on_rename,on_add,on_delete,on_file_delete,on_file_delete_for_upgrade,on_health_issue,on_health_restored,include_health_warnings,on_application_update,on_manual_interaction) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                id,
                media,
                i64::from(f.on_grab),
                i64::from(f.on_import),
                i64::from(f.on_upgrade),
                i64::from(f.on_import_complete),
                i64::from(f.on_rename),
                i64::from(f.on_add),
                i64::from(f.on_delete),
                i64::from(f.on_file_delete),
                i64::from(f.on_file_delete_for_upgrade),
                i64::from(f.on_health_issue),
                i64::from(f.on_health_restored),
                i64::from(f.include_health_warnings),
                i64::from(f.on_application_update),
                i64::from(f.on_manual_interaction)
            ],
        )
        .await?;
        if !tags.is_empty() {
            let known = scalar(
                c,
                &format!(
                    "SELECT count(*) FROM tags WHERE media_type=? AND id IN ({})",
                    tags.iter().map(|_| "?").collect::<Vec<_>>().join(",")
                ),
                libsql::params_from_iter(tag_params(media, tags)),
            )
            .await?;
            if known != tags.len() as i64 {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "tag_not_found",
                    format!("{media}: a tag does not exist in this media domain"),
                ));
            }
            for tag in tags {
                c.execute(
                    "INSERT INTO notification_tags(notification_id,media_type,tag_id) VALUES(?,?,?)",
                    params![id, media, *tag],
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn read(c: &Connection, id: &str) -> Result<Option<Notification>> {
    let mut rows = c
        .query(
            "SELECT name,implementation,enabled,revision,settings_json,secret_names,created_at,updated_at FROM notifications WHERE id=?",
            [id.to_owned()],
        )
        .await?;
    let Some(r) = rows.next().await? else {
        return Ok(None);
    };
    let settings: BTreeMap<String, SettingValue> =
        serde_json::from_str(&r.get::<String>(4)?).unwrap_or_default();
    let secrets_configured: Vec<String> =
        serde_json::from_str(&r.get::<String>(5)?).unwrap_or_default();
    let mut n = Notification {
        id: Uuid::parse_str(id).map_err(|_| ApiError::not_found())?,
        name: r.get(0)?,
        implementation: r.get(1)?,
        enabled: r.get::<i64>(2)? == 1,
        revision: r.get(3)?,
        settings,
        secrets_configured,
        tv: None,
        movies: None,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    };
    let mut rows = c
        .query(
            "SELECT media_type,on_grab,on_import,on_upgrade,on_import_complete,on_rename,on_add,on_delete,on_file_delete,on_file_delete_for_upgrade,on_health_issue,on_health_restored,include_health_warnings,on_application_update,on_manual_interaction FROM notification_scopes WHERE notification_id=?",
            [id.to_owned()],
        )
        .await?;
    while let Some(r) = rows.next().await? {
        let f = flags_from(&r)?;
        let media: String = r.get(0)?;
        let mut tags = Vec::new();
        let mut t = c
            .query(
                "SELECT tag_id FROM notification_tags WHERE notification_id=? AND media_type=? ORDER BY tag_id",
                params![id, media.clone()],
            )
            .await?;
        while let Some(row) = t.next().await? {
            tags.push(row.get::<i64>(0)?);
        }
        match media.as_str() {
            "tv" => {
                n.tv = Some(TvScope {
                    triggers: f.into(),
                    tag_ids: tags,
                })
            }
            _ => {
                n.movies = Some(MovieScope {
                    triggers: f.into(),
                    tag_ids: tags,
                })
            }
        }
    }
    Ok(Some(n))
}

fn flags_from(r: &libsql::Row) -> Result<Flags> {
    let b = |i: i32| -> Result<bool> { Ok(r.get::<i64>(i)? == 1) };
    Ok(Flags {
        on_grab: b(1)?,
        on_import: b(2)?,
        on_upgrade: b(3)?,
        on_import_complete: b(4)?,
        on_rename: b(5)?,
        on_add: b(6)?,
        on_delete: b(7)?,
        on_file_delete: b(8)?,
        on_file_delete_for_upgrade: b(9)?,
        on_health_issue: b(10)?,
        on_health_restored: b(11)?,
        include_health_warnings: b(12)?,
        on_application_update: b(13)?,
        on_manual_interaction: b(14)?,
    })
}
