# Local library rescans

TV and movie rescan commands adopt existing media in place. They do not move, rename, overwrite or delete source bytes. Existing root availability, symlink, hidden-directory, sample, depth, entry and walk-time guards apply. Admission continues to exclude conflicting imports and download processing; durable interrupted commands recover through the existing retry path.

TV local files resolve ordinary season/episode numbering, exact air dates and absolute episode numbers against current catalog metadata. Ordinary multi-episode names require every episode. Daily names are allowed for daily and anime series, but not standard or unconfigured series. An exact date shared by several episodes excludes season-zero specials and succeeds only if one regular episode remains. A sole matching special is valid. Absolute numbers require one ordinary absolute-number match; missing metadata and duplicates remain unmatched, including on standard series. Season packs remain unsupported.

Local files use ordinary numbering even when the series enables scene numbering. This follows the pinned Sonarr local-scan contract (`DiskScanService` → `ImportDecisionMaker` with `sceneSource=false`), not downloaded-release admission. Downloaded-file matching and release decisions now share daily/absolute identity resolution: daily-on-anime and absolute numbering on any configured series type are admitted. Downloaded matching still uses its native scene-first absolute-number policy when scene numbering is enabled; source-origin mapping remains a separate unfinished contract.

Each TV candidate resolves current metadata and checks the captured series path inside the same Immediate transaction that clears an old association and binds the new one. A failure preserves the old file record, metadata and associations. Pending replacement receipts remain protected. Stale-record cleanup checks the current series path and file identity before clearing rows. Deterministic path ordering and refusal to displace another present file remain unchanged. Movie matching is unchanged.

Scratch tests exercise daily/anime and standard/type mismatch, regular/special and absolute ambiguity, plain-number precedence over scene values, multi-episode matching, mutable path/type/metadata, injected rebinding failure, durable reopen/recovery and preserved media bytes/inodes. Existing movie, filesystem and admission tests remain part of the suite.

## Not claimed

Full scene-name mapping, special aliases, daily-part parsing, absolute ranges, season-pack interpretation, media probing, live library behavior and complete scanner parity remain unfinished. Filesystem observations can change after the bounded walk; database writer checks do not turn that walk into a filesystem snapshot. No schema, dependency or new durable command state is introduced.
