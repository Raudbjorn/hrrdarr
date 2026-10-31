pub mod api;
pub mod autotagging;
pub mod backup;
pub mod collections;
pub mod credits;
pub mod db;
pub mod episodes;
pub mod library;
pub mod media_files;
pub mod qualities;
pub mod quality_profiles;
pub mod snapshots;

pub mod api_contract;

pub mod import;

pub mod providers;

pub mod root_folders;

pub mod remote_paths;

pub mod filesystem;

pub mod commands;

pub mod history;

pub mod metadata;

pub mod blocklist;
pub mod search;

pub mod naming;

pub mod parse;

pub mod languages;

pub mod custom_formats;

pub mod tags;

pub mod completed_download_handling;
pub mod delay_profiles;
pub mod import_policy;
pub mod recycle_bin;
pub mod release_profiles;
pub mod revision_policy;

pub mod release_profile_terms;

pub mod health;
pub mod health_detectors;

pub mod host;

pub mod wanted;

pub mod calendar;

pub mod notifications;
pub mod rename;

/// Upper bound for a test waiting on background work (a worker restart, a probe entering a
/// blocked request) that takes about a second on an idle host. It is a hang detector, not a
/// performance assertion: the lib suite runs one thread per core and every test opens a migrated
/// database, so a window sized for an idle host expires under the starvation the suite creates
/// for itself (tracked in issue #38). A passing test never waits for it.
#[cfg(test)]
pub(crate) const TEST_HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
