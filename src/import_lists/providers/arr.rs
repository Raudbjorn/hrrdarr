//! Not implemented yet: registered as unavailable so it is hidden from the schema.
use super::*;

fn stub(name: &'static str) -> Implementation {
    Implementation {
        name,
        label: name,
        description: "",
        domains: &[],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: || Vec::new(),
        available: false,
    }
}
pub fn sonarr() -> Implementation {
    stub("sonarr")
}
pub fn radarr() -> Implementation {
    stub("radarr")
}
pub async fn fetch(_transport: &Transport, _list: &ListRuntime) -> Fetched {
    Err(FetchError::Unsupported)
}
