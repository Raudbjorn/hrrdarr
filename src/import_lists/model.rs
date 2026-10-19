//! Wire DTOs for import lists. TV and movie lists are one resource qualified by `media_type`; a
//! list's tags, profile, root folder and exclusions are always scoped to that domain. Secrets are
//! write-only.
use crate::{
    api::MediaDomain,
    library::{Availability, SeriesType},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_TAGS: usize = 200;
pub const MAX_LISTS: i64 = 64;
/// Hard cap on one fetched list; a larger list fails with `resource_limit` instead of truncating.
pub const MAX_ITEMS: usize = 5000;
pub const MAX_EXCLUSIONS_PER_REQUEST: usize = 500;

/// A configured non-secret value. Secrets travel only in [`ImportListInput::secrets`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(untagged)]
#[ts(rename = "ImportListSettingValue")]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Text(String),
}

/// TV seasons that start monitored (movies accept only `all` = monitored and `none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "ImportListMonitor")]
pub enum Monitor {
    #[default]
    All,
    None,
    FirstSeason,
    LastSeason,
}
impl Monitor {
    pub fn text(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::None => "none",
            Self::FirstSeason => "first_season",
            Self::LastSeason => "last_season",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "all" => Self::All,
            "none" => Self::None,
            "first_season" => Self::FirstSeason,
            "last_season" => Self::LastSeason,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ImportListInput", optional_fields)]
pub struct ImportListInput {
    pub name: String,
    pub implementation: String,
    pub media_type: MediaDomain,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Off: the list is fetched and its membership recorded, but nothing is added.
    #[serde(default = "yes")]
    pub enable_automatic_add: bool,
    #[serde(default)]
    pub search_on_add: bool,
    #[serde(default)]
    pub monitor: Monitor,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    /// Movies only.
    pub minimum_availability: Option<Availability>,
    /// TV only; defaults to standard.
    pub series_type: Option<SeriesType>,
    /// TV only; defaults to true.
    pub season_folder: Option<bool>,
    /// Minutes between scheduled syncs; defaults to the implementation default.
    pub interval_minutes: Option<i64>,
    #[serde(default)]
    pub tag_ids: Vec<i64>,
    #[serde(default)]
    pub settings: BTreeMap<String, SettingValue>,
    /// On update an absent key keeps the stored value and `null` clears it.
    #[serde(default)]
    pub secrets: BTreeMap<String, Option<String>>,
    /// Required on update; must equal the stored revision.
    pub revision: Option<i64>,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportList")]
pub struct ImportList {
    pub id: Uuid,
    pub name: String,
    pub implementation: String,
    pub media_type: MediaDomain,
    pub enabled: bool,
    pub enable_automatic_add: bool,
    pub search_on_add: bool,
    pub monitor: Monitor,
    pub quality_profile_id: Option<i64>,
    pub root_folder_id: Option<i64>,
    pub minimum_availability: Option<Availability>,
    pub series_type: Option<SeriesType>,
    pub season_folder: Option<bool>,
    pub interval_minutes: i64,
    pub tag_ids: Vec<i64>,
    pub revision: i64,
    pub settings: BTreeMap<String, SettingValue>,
    /// Names of secrets that are configured; the values are never returned.
    pub secrets_configured: Vec<String>,
    pub last_sync_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListCollection")]
pub struct ImportListCollection {
    pub items: Vec<ImportList>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "ImportListFieldKind")]
pub enum FieldKind {
    Text,
    Url,
    Number,
    Boolean,
    Select,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListField")]
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

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListImplementation")]
pub struct ImplementationSchema {
    pub implementation: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub domains: Vec<MediaDomain>,
    pub default_interval_minutes: i64,
    pub min_interval_minutes: i64,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListImplementationCollection")]
pub struct ImplementationList {
    pub items: Vec<ImplementationSchema>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ImportListTestInput", optional_fields)]
pub struct TestInput {
    /// A saved list whose stored secrets fill any secret not supplied in the draft.
    pub existing_id: Option<Uuid>,
    pub draft: ImportListInput,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListTestResult")]
pub struct TestResult {
    pub ok: bool,
    /// Items the provider returned (the test fetches the same way a sync does, without adding).
    pub item_count: Option<i64>,
    /// Stable machine class; never contains remote bodies, URLs or secrets.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename = "ImportListRunStatus")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}
impl RunStatus {
    pub fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            _ => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListRun")]
pub struct Run {
    pub id: Uuid,
    pub list_id: Uuid,
    pub media_type: MediaDomain,
    /// `manual` (Sync now) or `scheduled`.
    pub origin: String,
    pub status: RunStatus,
    /// The membership snapshot was fetched; later claims resume from it without refetching.
    pub fetched: bool,
    pub attempts: i64,
    pub items_fetched: i64,
    pub added: i64,
    pub existing: i64,
    pub excluded: i64,
    pub unresolved: i64,
    pub failed: i64,
    /// Items of the snapshot still waiting to be processed.
    pub pending: i64,
    pub error_code: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListRunCollection")]
pub struct RunCollection {
    pub items: Vec<Run>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListSyncAll")]
pub struct SyncAll {
    pub runs: Vec<Run>,
    /// Lists skipped because they are disabled or already have an active run.
    pub skipped: i64,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListItem")]
pub struct Item {
    pub key: String,
    pub title: String,
    pub year: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    /// pending, added, existing, excluded, unresolved or failed.
    pub state: String,
    pub error_code: Option<String>,
    pub library_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListItemPage")]
pub struct ItemPage {
    pub items: Vec<Item>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ImportListExclusionInput", optional_fields)]
pub struct ExclusionInput {
    pub media_type: MediaDomain,
    /// TVDB id for TV, TMDb id for movies.
    pub external_id: i64,
    pub title: String,
    pub year: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListExclusion")]
pub struct Exclusion {
    pub id: i64,
    pub media_type: MediaDomain,
    pub external_id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(rename = "ImportListExclusionPage")]
pub struct ExclusionPage {
    pub items: Vec<Exclusion>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ImportListExclusionBulkDelete")]
pub struct ExclusionBulkDelete {
    pub ids: Vec<i64>,
}

/// One item as a provider reports it, before resolution to a library identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListItem {
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i64>,
}
