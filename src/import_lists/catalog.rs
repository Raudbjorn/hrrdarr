//! Registry of implementations and the single fetch dispatch.
use super::{
    model::{ImplementationSchema, MAX_ITEMS},
    providers::{self, FetchError, Fetched, Implementation, ListRuntime},
};
use crate::{api::MediaDomain, notifications::transport::Transport};
use std::time::Duration;

/// Wall-clock bound on one whole fetch (all pages), on top of the transport's per-request limits.
pub const FETCH_DEADLINE: Duration = Duration::from_secs(90);

/// Every implementation, available or not.
fn registry() -> Vec<Implementation> {
    vec![
        providers::custom::implementation(),
        providers::rss::implementation(),
        providers::trakt::implementation(),
        providers::tmdb::implementation(),
        providers::imdb::implementation(),
        providers::stevenlu::implementation(),
        providers::plex::implementation(),
        providers::mal::implementation(),
        providers::anilist::implementation(),
        providers::simkl::implementation(),
        providers::arr::sonarr(),
        providers::arr::radarr(),
    ]
}

pub fn all() -> Vec<Implementation> {
    registry().into_iter().filter(|i| i.available).collect()
}
pub fn find(name: &str) -> Option<Implementation> {
    all().into_iter().find(|i| i.name == name)
}
pub fn schemas() -> Vec<ImplementationSchema> {
    all()
        .into_iter()
        .map(|i| ImplementationSchema {
            implementation: i.name,
            label: i.label,
            description: i.description,
            domains: i.domains.to_vec(),
            default_interval_minutes: i.default_interval_minutes,
            min_interval_minutes: i.min_interval_minutes,
            fields: (i.fields)(),
        })
        .collect()
}

/// Fetches the complete list. The result is never longer than [`MAX_ITEMS`] and never partial.
pub async fn fetch(transport: &Transport, list: &ListRuntime) -> Fetched {
    let supported = find(&list.implementation).is_some_and(|i| i.domains.contains(&list.media));
    if !supported {
        return Err(FetchError::Unsupported);
    }
    let work = async {
        match list.implementation.as_str() {
            "custom" => providers::custom::fetch(transport, list).await,
            "rss" => providers::rss::fetch(transport, list).await,
            "trakt" => providers::trakt::fetch(transport, list).await,
            "tmdb" => providers::tmdb::fetch(transport, list).await,
            "imdb" => providers::imdb::fetch(transport, list).await,
            "stevenlu" => providers::stevenlu::fetch(transport, list).await,
            "plex" => providers::plex::fetch(transport, list).await,
            "mal" => providers::mal::fetch(transport, list).await,
            "anilist" => providers::anilist::fetch(transport, list).await,
            "simkl" => providers::simkl::fetch(transport, list).await,
            "sonarr" | "radarr" => providers::arr::fetch(transport, list).await,
            _ => Err(FetchError::Unsupported),
        }
    };
    let items = tokio::time::timeout(FETCH_DEADLINE, work)
        .await
        .map_err(|_| FetchError::Timeout)??;
    if items.len() > MAX_ITEMS {
        return Err(FetchError::ResourceLimit);
    }
    Ok(items)
}

pub fn supports(name: &str, media: MediaDomain) -> bool {
    find(name).is_some_and(|i| i.domains.contains(&media))
}
