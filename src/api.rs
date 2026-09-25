//! Shared wire DTOs; serialized by the actual HTTP handlers and exported to TypeScript.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Serialize, TS)]
pub struct ApiErrorDetail {
    pub code: &'static str,
    pub message: &'static str,
}
#[derive(Debug, Serialize, TS)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorDetail,
}
impl ApiErrorEnvelope {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            error: ApiErrorDetail { code, message },
        }
    }
}
#[derive(Debug, Serialize, TS)]
pub struct LegacyError {
    pub error: String,
}
#[derive(Debug, Serialize, TS)]
pub struct ApiPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: u16,
    pub offset: u32,
}
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub episode_id: i64,
    pub source: String,
    pub mode: String,
    pub destination: String,
}
#[derive(Debug, Serialize, TS)]
pub struct Operation {
    pub id: Uuid,
    pub target: crate::db::MediaTarget,
    pub status: String,
    pub message: String,
    pub error_code: Option<String>,
}
#[derive(Deserialize, TS)]
pub struct SnapshotOptions {
    pub application: crate::snapshots::Application,
    #[serde(default = "default_dry_run")]
    #[ts(as = "Option<bool>", optional)]
    pub dry_run: bool,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub import_providers: bool,
}
fn default_dry_run() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum MediaDomain {
    Tv,
    Movies,
}
impl MediaDomain {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "tv" => Ok(Self::Tv),
            "movies" => Ok(Self::Movies),
            _ => Err("Media domain must be tv or movies"),
        }
    }
}
