//! Deterministic type registry. Only DTOs used by HTTP handlers are roots here.
use std::{
    any::TypeId,
    collections::{BTreeMap, HashSet},
};
use ts_rs::{Config, TS, TypeVisitor};
struct Registry {
    cfg: Config,
    seen: HashSet<TypeId>,
    declarations: BTreeMap<String, String>,
}
impl TypeVisitor for Registry {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        if !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        if T::output_path().is_some() {
            let name = T::ident(&self.cfg);
            let declaration = T::decl(&self.cfg);
            if let Some(old) = self.declarations.insert(name.clone(), declaration.clone()) {
                assert_eq!(old, declaration, "conflicting exported API type {name}");
            }
        }
        T::visit_dependencies(self);
    }
}
/// Configuration never reads environment variables, timestamps, paths or host-specific values.
pub fn render() -> String {
    let mut registry = Registry {
        cfg: Config::new().with_large_int("number"),
        seen: HashSet::new(),
        declarations: BTreeMap::new(),
    };
    macro_rules! roots {($($ty:ty),* $(,)?)=>{$(registry.visit::<$ty>();)*};}
    roots!(
        crate::commands::processing::ProcessingPolicyInput,
        crate::commands::processing::ProcessingPolicy,
        crate::commands::processing::ProcessingInput,
        crate::commands::processing::ProcessingQuery,
        crate::api::ApiPage<crate::commands::processing::DownloadProcessing>,
        crate::commands::rss::RssTarget,
        crate::commands::rss::RssInput,
        crate::api::ApiPage<crate::commands::rss::RssCommand>,
        crate::api::ApiPage<crate::commands::rss::RssCandidate>,
        crate::commands::rss::RssScheduleInput,
        crate::commands::rss::RssSchedule,
        crate::commands::rss::RssCandidateQuery,
        crate::commands::rss::RssScheduleDelete,
        crate::search::ReleaseSearchInput,
        crate::search::ReleaseSearchPage,
        crate::search::ReleasePolicy,
        crate::commands::blocklist::BlocklistClearInput,
        crate::api::ApiPage<crate::commands::blocklist::BlocklistClearCommand>,
        crate::blocklist::BlocklistQuery,
        crate::blocklist::BlocklistRemoval,
        crate::api::ApiPage<crate::blocklist::BlocklistEntry>,
        crate::history::HistoryQuery,
        crate::api::ApiPage<crate::history::HistoryEvent>,
        crate::commands::metadata::MetadataCommandInput,
        crate::commands::metadata::MetadataCommandQuery,
        crate::api::ApiPage<crate::commands::metadata::MetadataCommand>,
        crate::commands::CommandInput,
        crate::commands::CommandQuery,
        crate::api::ApiPage<crate::commands::Command>,
        crate::commands::RefreshScheduleInput,
        crate::commands::RefreshScheduleDelete,
        crate::commands::RefreshSchedule,
        crate::commands::QueueQuery,
        crate::commands::QueueSnapshot,
        crate::remote_paths::MappingInput,
        crate::remote_paths::MappingUpdate,
        crate::remote_paths::MappingRevision,
        crate::remote_paths::MappingQuery,
        crate::remote_paths::ResolveInput,
        crate::remote_paths::Resolution,
        crate::remote_paths::ProviderPathInput,
        crate::remote_paths::ProviderPathPreview,
        crate::api::ApiPage<crate::remote_paths::Mapping>,
        crate::filesystem::FilesystemLookup,
        crate::filesystem::FilesystemPath,
        crate::filesystem::FilesystemMediaQuery,
        crate::filesystem::FilesystemContents,
        crate::filesystem::FilesystemType,
        crate::filesystem::FilesystemMediaFiles,
        crate::root_folders::RootInput,
        crate::root_folders::RootQuery,
        crate::root_folders::RootFolder,
        crate::api::ApiPage<crate::root_folders::RootFolder>,
        crate::providers::indexer::IndexerSearch,
        crate::providers::ProviderSearchResult,
        crate::providers::ProviderTestResult,
        crate::providers::ProviderDownloadResult,
        crate::providers::ProviderFilesResult,
        crate::providers::qbittorrent::DownloadQuery,
        crate::providers::qbittorrent::DownloadFilesQuery,
        crate::providers::administration::ProviderFilter,
        crate::providers::administration::ProviderSchema,
        crate::providers::administration::ProviderSelection,
        crate::providers::administration::ProviderBulkUpdate,
        crate::providers::administration::ProviderBulkResult,
        crate::providers::administration::ProviderBatchResult,
        crate::providers::categories::CategoryDiscoveryInput,
        crate::providers::categories::CategoryDiscoveryResult,
        crate::providers::draft::ProviderDraftInput,
        crate::providers::draft::ProviderDraftResult,
        crate::providers::ProviderInput,
        crate::providers::ProviderUpdate,
        crate::providers::ProviderQuery,
        crate::providers::ProviderRevision,
        crate::api::ApiPage<crate::providers::Provider>,
        crate::api::ApiErrorEnvelope,
        crate::api::LegacyError,
        crate::api::ImportRequest,
        crate::import::ImportInput,
        crate::import::ManualImportRequest,
        crate::import::Mode,
        crate::api::Operation,
        crate::api::SnapshotOptions,
        crate::api::MediaDomain,
        crate::snapshots::Report,
        crate::library::Create,
        crate::library::LookupQuery,
        crate::library::SeriesLookupAdd,
        crate::library::MovieLookupAdd,
        crate::metadata::LookupResult,
        crate::metadata::SeriesDetails,
        crate::metadata::MovieDetails,
        crate::metadata::EpisodeDetails,
        crate::library::Patch,
        crate::library::Bulk,
        crate::library::Editor,
        crate::library::Page,
        crate::library::LibraryItem,
        crate::library::LibraryPage,
        crate::library::LegacySeries,
        crate::episodes::Includes,
        crate::episodes::ListQuery,
        crate::episodes::Monitor,
        crate::episodes::MonitorMany,
        crate::episodes::Episode,
        crate::episodes::LegacyEpisode,
        crate::api::ApiPage<crate::episodes::Episode>,
        crate::qualities::Update,
        crate::qualities::Reset,
        crate::qualities::Definition,
        crate::qualities::Limits,
        crate::quality_profiles::ProfileInput,
        crate::quality_profiles::Profile,
        crate::quality_profiles::ProfilePage,
        crate::quality_profiles::Page,
        crate::media_files::Select,
        crate::media_files::FilePatch,
        crate::media_files::FileBulk,
        crate::media_files::FileEditor,
        crate::media_files::FileResource,
        crate::api::ApiPage<crate::media_files::FileResource>,
    );
    let mut output = String::from(
        "// Generated from the Rust HTTP DTOs by ts-rs. Do not edit.\n// Regenerate: cargo run --locked --bin generate-api\n// JSON i64/u64 remain numbers on the wire. Reject integers outside Number.isSafeInteger\n// at the API boundary before using IDs, sizes or duration ticks; these types are not validators.\n// URL-query properties are omittable scalars; nullability below describes JSON bodies/resources.\n\n",
    );
    for declaration in registry.declarations.into_values() {
        output.push_str("export ");
        output.push_str(&declaration);
        output.push_str("\n\n");
    }
    // Keep declaration spacing internally, but end the artifact with one newline.
    output.truncate(output.trim_end().len());
    output.push('\n');
    output
}
pub fn output_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/src/lib/api.generated.ts")
}
pub fn check() -> Result<(), String> {
    check_file(&output_path())
}
/// Check a supplied artifact without writing it (also useful for isolated drift tests).
pub fn check_file(path: &std::path::Path) -> Result<(), String> {
    let current = std::fs::read_to_string(path).map_err(|_| {
        "Generated API contract is missing; run cargo run --locked --bin generate-api".to_owned()
    })?;
    if current != render() {
        return Err(
            "Generated API contract is stale; run cargo run --locked --bin generate-api".into(),
        );
    }
    Ok(())
}
