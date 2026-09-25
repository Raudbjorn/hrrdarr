# Native filesystem API

All three read-only endpoints operate on the local Linux filesystem visible to hrrdarr. They do not create directories, import files, modify media, inspect codecs or alter database state. Paths describe observations; they are not handles that authorize later imports and are not a consistent filesystem snapshot.

## Directory lookup

`GET /api/v1/filesystem` accepts:

- `path`: optional absolute UTF-8 path; absent, empty or whitespace-only selects `/` on Linux.
- `include_files`: boolean, default `false`.
- `allow_folders_without_trailing_slashes`: boolean, default `false`.

A trailing slash selects that directory. Otherwise the final component is treated as an unfinished name and the parent directory is listed, without filtering names by that prefix. With `allow_folders_without_trailing_slashes=true`, an existing directory selects itself; an absent path or a file still selects its parent. An inaccessible directory is an explicit error.

The response contains `path`, nullable `parent`, sorted `directories` and sorted `files`. Directory/parent paths end in `/`; `/` has no parent. Each entry includes `type` (`folder` or `file`), `name`, `path`, nullable `last_modified` in UTC, nullable `extension`, and nullable `size`. File extensions retain their case and leading dot; a file without an extension has `""`. Directories have no extension or size. File size is null when it cannot be represented exactly as a JavaScript integer. Sorting is deterministic, case-sensitive Unicode/UTF-8 order rather than host-culture order.

Only regular files are included when requested. Symlinks, sockets and other special entries are omitted. Browsing excludes the pinned system/recycle/cache directory names (`SpecialFolders` in both references); hidden directories in general are not excluded.

## Entity classification

`GET /api/v1/filesystem/type?path=/absolute/path` returns `{ "type": "file" }` for a regular file, otherwise `{ "type": "folder" }`. A trailing slash expresses directory intent and returns the folder hint. Missing paths and symlink/special leaf entries are also classified as folder.

**This is a UI hint, not an existence or accessibility assertion.** Invalid input still returns 400, and a failed parent observation can return 503. Symlink ancestors are never traversed. The endpoint does not read file contents.

## Recursive media enumeration

`GET /api/v1/filesystem/media-files?path=/absolute/folder&media_type=tv` returns `path`, `media_type` and `files`, with each file carrying its absolute `path`, root-relative `relative_path`, and `name`. `media_type=movies` selects the movie inventory. It is required because movie recognition additionally includes `.mk3d`.

Recognition is case-insensitive extension membership, including the references' legacy video, stream and disk-image extensions. It does not establish playable content, quality or matching. Enumeration recursively includes recognized regular files; it does **not** apply the browse-only special-folder filter or import-decision exclusion rules. Symlinks are never followed. Results sort by relative path.

## Bounds and failures

Queries reject unknown properties and invalid booleans/media types. Input paths are at most 4096 UTF-8 bytes, absolute and normalized, permitting one trailing slash. Relative paths, NUL, `.`/`..` components and redundant separators are rejected. The byte limit is checked before blank-path defaulting. Returned child paths have the same byte ceiling.

Each traversal visits at most 10,000 entries, including nonmatching entries, and descends at most 32 directory levels below its root. Descriptor-relative traversal holds at most that bounded ancestor stack. Symlink-free directory opens reuse the import service's existing descriptor walk; directory identities are checked when descending and the root identity is rechecked at completion. Concurrent mutation can still make returned names stale after observation.

Responses have a 1 MiB serialized ceiling. During collection, conservative accounting reserves six bytes per string byte plus 256 bytes per entry against a 512 KiB working budget. Consequently a request can reach the response limit before producing 1 MiB of JSON. Results that exceed a limit are discarded; there are no silently truncated successful arrays.

Filesystem reads share the existing two-worker observation pool with root-folder and remote-path checks. There is no unbounded pending-job queue: exhaustion returns 503. A five-second response timeout and cooperative worker deadline apply. Timeout or client cancellation does not stop a blocked kernel call; the actual worker retains its permit until it ends. The worker also checks its deadline after observation, before returning success.

Errors use `{ "error": { "code": "...", "message": "..." } }`:

| HTTP | Meaning / code |
| --- | --- |
| 400 | `invalid_filesystem_query` |
| 409 | `filesystem_changed` (identity/cycle conflict) |
| 413 | `filesystem_entry_limit`, `filesystem_depth_limit`, `filesystem_path_limit`, or `filesystem_response_limit` |
| 503 | `filesystem_unavailable`, `filesystem_non_utf8_name`, or `filesystem_busy` |
| 504 | `filesystem_timeout` |
| 500 | `filesystem_worker_failed` |

Missing/inaccessible media roots return explicit 503 rather than the upstream controller's empty array. The native API also makes bounds and symlink handling explicit instead of claiming V3/V5 wire compatibility. POSIX root browsing is implemented; Windows drive/network-share enumeration is not claimed.

## Evidence

Requirements were inspected independently in both pinned V3 `FileSystem/FileSystemController.cs` controllers, Sonarr V5's corresponding controller/resources, both `NzbDrone.Common/Disk/FileSystemLookupService.cs`/`SpecialFolders.cs`, and both `NzbDrone.Core/MediaFiles/DiskScanService.cs`/`MediaFileExtensions.cs`. Sonarr is pinned to `76c684e097f16ac216e6213845e5cac372774995`; Radarr to `c90668a520664ad0c91812cfee57c41928ad2148`. Source inspection is not upstream runtime equivalence.

`tests/filesystem_api.rs` uses a real localhost HTTP server and owned temporary fixtures for lookup flags, metadata, classification, recursive TV/movie differences, symlink escape refusal, permission/missing-root errors and non-UTF-8 names. Unit tests in `src/filesystem.rs` exercise bounded traversal/error paths, root replacement, default selection without browsing the host root, and actual worker-slot retention after cancellation/timeout. The root-observer unit test shares a test-only lock so parallel tests cannot consume each other's worker slots. Generated TypeScript comes from the actual Rust DTO registry.

These checks do not prove Windows behavior, network-filesystem reliability, consistent snapshots under concurrent changes, codec validity, or any import/automation outcome. No tests enumerate live media or the host root.
