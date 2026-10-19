//! Not implemented yet: registered as unavailable so it is hidden from the schema.
use super::*;

pub fn implementation() -> Implementation {
    Implementation {
        name: "plex",
        label: "plex",
        description: "",
        domains: &[],
        default_interval_minutes: 360,
        min_interval_minutes: 5,
        fields: || Vec::new(),
        available: false,
    }
}
pub async fn fetch(_transport: &Transport, _list: &ListRuntime) -> Fetched {
    Err(FetchError::Unsupported)
}
