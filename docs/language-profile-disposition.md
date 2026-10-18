# Deprecated TV language-profile disposition

The pinned TV LanguageProfile surface is accounted for as an obsolete compatibility facade, not an active native configuration entity. hrrdarr does not introduce a LanguageProfile table, a placeholder native CRUD API, or a V3 compatibility facade for it. This follows operator decision 5 (plain language fields/lists) and the root API boundary that V3 wire compatibility requires a separate explicit decision. It does not remove working language selection or matching.

## Source evidence

At Sonarr `76c684e097f16ac216e6213845e5cac372774995`:

- `src/Sonarr.Api.V3/Profiles/Languages/LanguageProfileController.cs:12–80` marks the controller obsolete. List and detail return the fixed ID 1 “Deprecated” English placeholder; POST and PUT return Accepted with the supplied resource; DELETE does nothing. These mutations do not persist a policy or change matching behavior.
- `src/Sonarr.Api.V3/Profiles/Languages/LanguageProfileSchemaController.cs:13–37` is also obsolete and returns the same fixed placeholder.
- `src/NzbDrone.Core/Datastore/Migration/175_language_profiles_to_custom_formats.cs:11–34` changes file/history/blocklist language storage to lists and deletes the series/import-list LanguageProfileId columns and LanguageProfiles table.
- `src/NzbDrone.Core/CustomFormats/Specifications/LanguageSpecification.cs` is the active language-condition surface. It is separate from those compatibility controllers.

These paths refer to the pinned reference tree retained locally under `.do-not-commit/Sonarr`; they are requirements evidence, not implementation copied into hrrdarr. Radarr `c90668a520664ad0c91812cfee57c41928ad2148` has no corresponding separate TV profile entity.

## Native behavior and remaining scope

Accepted native source at `adc86ba` already represents language as scoped values and evidence: `src/languages.rs`, `src/metadata.rs` (original_language), `src/custom_formats.rs:382` (language schema), and `src/custom_formats/matching.rs:318` (language/except-language/original-language matching). `frontend/src/lib/CustomFormatPanel.svelte` exposes native custom-format conditions, including Except language. Existing file language evidence is read in `src/custom_formats/matching.rs:589–620`. None depends on a LanguageProfile entity.

This accounts only for ledger api.043's obsolete facade. Full language parser, custom-format, cutoff, provider, snapshot and lookup behavior remains governed by its own rows, especially grb.004. Their incompleteness is not hidden by disposing of the facade, and this document does not promote them. No third-party client compatibility is promised. If V3 compatibility is explicitly commissioned later, its placeholder response and inert write contracts need separate contract tests; they are not a prerequisite to native language matching.

## Not claimed

Source-based disposition, not a runtime test result. No new endpoint, schema, language semantics, dependencies or migration is introduced. No claim of complete language/CF parity, historical pre-migration profile conversion, V3 compatibility or a slice gate.
