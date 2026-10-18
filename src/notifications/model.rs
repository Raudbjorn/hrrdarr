//! Wire DTOs for the notification registry. TV and movie scopes are distinct types: a trigger or
//! tag set never applies to an unqualified domain, and secrets are write-only.
use crate::api::MediaDomain;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_TAGS_PER_SCOPE: usize = 200;
pub const MAX_NOTIFICATIONS: usize = 64;

/// A configured non-secret value. Secrets travel only as strings in [`NotificationInput::secrets`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(untagged)]
#[ts(rename = "NotificationSettingValue")]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Text(String),
}

/// Sonarr trigger set. `on_import` is Sonarr's "On Import / On Download"; an upgrade is an
/// import that replaced a file and is delivered only when both `on_import` and `on_upgrade` are on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "TvNotificationTriggers")]
pub struct TvTriggers {
    #[serde(default)]
    pub on_grab: bool,
    #[serde(default)]
    pub on_import: bool,
    #[serde(default)]
    pub on_upgrade: bool,
    #[serde(default)]
    pub on_import_complete: bool,
    #[serde(default)]
    pub on_rename: bool,
    #[serde(default)]
    pub on_series_add: bool,
    #[serde(default)]
    pub on_series_delete: bool,
    #[serde(default)]
    pub on_episode_file_delete: bool,
    #[serde(default)]
    pub on_episode_file_delete_for_upgrade: bool,
    #[serde(default)]
    pub on_health_issue: bool,
    #[serde(default)]
    pub include_health_warnings: bool,
    #[serde(default)]
    pub on_health_restored: bool,
    #[serde(default)]
    pub on_application_update: bool,
    #[serde(default)]
    pub on_manual_interaction_required: bool,
}

/// Radarr trigger set (no batch import-complete event exists for movies).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "MovieNotificationTriggers")]
pub struct MovieTriggers {
    #[serde(default)]
    pub on_grab: bool,
    #[serde(default)]
    pub on_import: bool,
    #[serde(default)]
    pub on_upgrade: bool,
    #[serde(default)]
    pub on_rename: bool,
    #[serde(default)]
    pub on_movie_added: bool,
    #[serde(default)]
    pub on_movie_delete: bool,
    #[serde(default)]
    pub on_movie_file_delete: bool,
    #[serde(default)]
    pub on_movie_file_delete_for_upgrade: bool,
    #[serde(default)]
    pub on_health_issue: bool,
    #[serde(default)]
    pub include_health_warnings: bool,
    #[serde(default)]
    pub on_health_restored: bool,
    #[serde(default)]
    pub on_application_update: bool,
    #[serde(default)]
    pub on_manual_interaction_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "TvNotificationScope")]
pub struct TvScope {
    #[serde(default)]
    pub triggers: TvTriggers,
    /// TV tag IDs. Empty means every series; otherwise a series must carry one of them.
    #[serde(default)]
    pub tag_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "MovieNotificationScope")]
pub struct MovieScope {
    #[serde(default)]
    pub triggers: MovieTriggers,
    /// Movie tag IDs. Empty means every movie; otherwise a movie must carry one of them.
    #[serde(default)]
    pub tag_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "NotificationInput", optional_fields)]
pub struct NotificationInput {
    pub name: String,
    pub implementation: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Non-secret settings keyed by the implementation schema field name.
    #[serde(default)]
    pub settings: BTreeMap<String, SettingValue>,
    /// Write-only secrets. On update an absent key keeps the stored value and `null` clears it.
    #[serde(default)]
    pub secrets: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub tv: Option<TvScope>,
    #[serde(default)]
    pub movies: Option<MovieScope>,
    /// Required on update; must equal the stored revision.
    #[serde(default)]
    pub revision: Option<i64>,
}
fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "Notification")]
pub struct Notification {
    pub id: Uuid,
    pub name: String,
    pub implementation: String,
    pub enabled: bool,
    pub revision: i64,
    pub settings: BTreeMap<String, SettingValue>,
    /// Names of secrets that are configured; the values are never returned.
    pub secrets_configured: Vec<String>,
    pub tv: Option<TvScope>,
    pub movies: Option<MovieScope>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationList")]
pub struct NotificationList {
    pub items: Vec<Notification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "NotificationFieldKind")]
pub enum FieldKind {
    Text,
    Url,
    Number,
    Boolean,
    Select,
    Path,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationField")]
pub struct Field {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    /// Secret fields are submitted through `secrets`, stored encrypted and never returned.
    pub secret: bool,
    pub help: &'static str,
    pub default: Option<SettingValue>,
    pub options: Vec<&'static str>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub max_length: Option<u32>,
}

/// The delivery verbs an implementation reacts to, so the UI can hide pointless triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "NotificationImplementationKind")]
pub enum ImplementationKind {
    /// Delivers a human-readable message for every event.
    Message,
    /// Delivers the full structured event to an HTTP endpoint.
    Structured,
    /// Executes a local program with the event in its environment.
    Script,
    /// Asks a media server to rescan after library changes (ignores other events).
    LibraryUpdate,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationImplementation")]
pub struct ImplementationSchema {
    pub implementation: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: ImplementationKind,
    pub domains: Vec<MediaDomain>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationImplementationList")]
pub struct ImplementationList {
    pub items: Vec<ImplementationSchema>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "NotificationTestStatus")]
pub enum TestStatus {
    Delivered,
    Failed,
    /// The implementation has nothing to send for a test event (for example library updaters).
    Skipped,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationDomainTestResult")]
pub struct DomainTestResult {
    pub media_type: MediaDomain,
    pub status: TestStatus,
    /// Stable machine class; never contains remote bodies, URLs or secrets.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationTestResult")]
pub struct TestResult {
    pub id: Option<Uuid>,
    pub name: String,
    pub results: Vec<DomainTestResult>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationTestAllResult")]
pub struct TestAllResult {
    pub items: Vec<TestResult>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "NotificationTestInput", optional_fields)]
pub struct TestInput {
    /// A saved notification whose stored secrets fill any secret not supplied in the draft.
    #[serde(default)]
    pub existing_id: Option<Uuid>,
    pub draft: NotificationInput,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationDelivery")]
pub struct Delivery {
    pub id: i64,
    pub event_sequence: i64,
    pub notification_id: Uuid,
    pub event_kind: String,
    pub media_type: Option<MediaDomain>,
    pub state: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "NotificationDeliveryList")]
pub struct DeliveryList {
    pub items: Vec<Delivery>,
}

/// Compact flag form used by storage and fan-out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub on_grab: bool,
    pub on_import: bool,
    pub on_upgrade: bool,
    pub on_import_complete: bool,
    pub on_rename: bool,
    pub on_add: bool,
    pub on_delete: bool,
    pub on_file_delete: bool,
    pub on_file_delete_for_upgrade: bool,
    pub on_health_issue: bool,
    pub on_health_restored: bool,
    pub include_health_warnings: bool,
    pub on_application_update: bool,
    pub on_manual_interaction: bool,
}
impl From<&TvTriggers> for Flags {
    fn from(t: &TvTriggers) -> Self {
        Self {
            on_grab: t.on_grab,
            on_import: t.on_import,
            on_upgrade: t.on_upgrade,
            on_import_complete: t.on_import_complete,
            on_rename: t.on_rename,
            on_add: t.on_series_add,
            on_delete: t.on_series_delete,
            on_file_delete: t.on_episode_file_delete,
            on_file_delete_for_upgrade: t.on_episode_file_delete_for_upgrade,
            on_health_issue: t.on_health_issue,
            on_health_restored: t.on_health_restored,
            include_health_warnings: t.include_health_warnings,
            on_application_update: t.on_application_update,
            on_manual_interaction: t.on_manual_interaction_required,
        }
    }
}
impl From<&MovieTriggers> for Flags {
    fn from(t: &MovieTriggers) -> Self {
        Self {
            on_grab: t.on_grab,
            on_import: t.on_import,
            on_upgrade: t.on_upgrade,
            on_import_complete: false,
            on_rename: t.on_rename,
            on_add: t.on_movie_added,
            on_delete: t.on_movie_delete,
            on_file_delete: t.on_movie_file_delete,
            on_file_delete_for_upgrade: t.on_movie_file_delete_for_upgrade,
            on_health_issue: t.on_health_issue,
            on_health_restored: t.on_health_restored,
            include_health_warnings: t.include_health_warnings,
            on_application_update: t.on_application_update,
            on_manual_interaction: t.on_manual_interaction_required,
        }
    }
}
impl From<Flags> for TvTriggers {
    fn from(f: Flags) -> Self {
        Self {
            on_grab: f.on_grab,
            on_import: f.on_import,
            on_upgrade: f.on_upgrade,
            on_import_complete: f.on_import_complete,
            on_rename: f.on_rename,
            on_series_add: f.on_add,
            on_series_delete: f.on_delete,
            on_episode_file_delete: f.on_file_delete,
            on_episode_file_delete_for_upgrade: f.on_file_delete_for_upgrade,
            on_health_issue: f.on_health_issue,
            include_health_warnings: f.include_health_warnings,
            on_health_restored: f.on_health_restored,
            on_application_update: f.on_application_update,
            on_manual_interaction_required: f.on_manual_interaction,
        }
    }
}
impl From<Flags> for MovieTriggers {
    fn from(f: Flags) -> Self {
        Self {
            on_grab: f.on_grab,
            on_import: f.on_import,
            on_upgrade: f.on_upgrade,
            on_rename: f.on_rename,
            on_movie_added: f.on_add,
            on_movie_delete: f.on_delete,
            on_movie_file_delete: f.on_file_delete,
            on_movie_file_delete_for_upgrade: f.on_file_delete_for_upgrade,
            on_health_issue: f.on_health_issue,
            include_health_warnings: f.include_health_warnings,
            on_health_restored: f.on_health_restored,
            on_application_update: f.on_application_update,
            on_manual_interaction_required: f.on_manual_interaction,
        }
    }
}
