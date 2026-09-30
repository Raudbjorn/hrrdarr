# Downloading into configured library roots

The `hc.020` requirement is “Downloading into a configured root/library folder
(checks all configured root folders, not just ones in active use)”. A warning
means the configured effective download output directory equals a library root
or is below it by path components. A sibling such as `/media/tv-downloads` is
not below `/media/tv`. The opposite relation (library root below a download
output parent) is not this upstream check's warning condition.

The native health engine compares against **every configured root across TV and
movies**, including unused roots. This union catches a TV download scope writing
inside a movie library. Client categories and remote mappings remain scoped to
the download's domain. Root enumeration, comparison, generation fencing,
publication and mutation invalidation belong to the health engine; they are not
performed by the provider status helper described here.

## Provider facts and protocol

The private provider API is:

```rust
RefreshClient::inspect_download_roots(provider_id, revision, domain)
    -> Result<Vec<String>, AutomationError>
```

It reuses the existing qBittorrent status protocol through a shared private
snapshot helper; the existing `inspect_client_status` locality API is unchanged.
There is no new provider protocol implementation or status cache.

For the currently supported client, qBittorrent, status reports one configured
active-scope output root:

- Read current preferences and global `save_path`.
- For modern APIs, read the category information for this domain's configured
  active category. A rooted category save path replaces the default; a relative
  save path is joined to the default. An absent category or empty category save
  path falls back to the default. The legacy API uses the default path.
- Missing, empty or malformed required global `save_path` fails observation;
  category fallback never substitutes for a missing required root.
- Use existing bounded path validation and protocol compatibility behavior.
- The imported category, individual torrent destination overrides and temporary
  incomplete directories are not this configured-output-root contract. Current
  item/file observations and import checks are separate behavior.

Both pinned reference implementations establish that single-root contract:
Sonarr `src/NzbDrone.Core/Download/Clients/QBittorrent/QBittorrent.cs:415–453` and
Radarr's corresponding file `:372–410`. Each root-folder health check uses all
configured roots and tests equality/parenthood (`src/NzbDrone.Core/HealthCheck/Checks/DownloadClientRootFolderCheck.cs:40–65`).
References are requirements evidence only; implementation reuses native Rust
code rather than copying or translating either reference.

## Mapping and observation validity

The helper reads the saved endpoint host and calls the existing
`remote_paths::resolve` in `RemoteToLocal` direction for the client domain. This
preserves existing host normalization, component boundaries, Windows/UNC matching
and configured mapping precedence. It does not invent a competing path matcher.
Only a normalized absolute POSIX output is returned to the engine; `/` is a valid
observed output directory. Unmapped Windows/UNC paths remain unresolved errors,
never reinterpreted as local POSIX roots. A Windows/UNC path is accepted only
after an explicit applicable mapping produces a valid local POSIX path. A valid unmapped POSIX path follows the
existing identity-mapping contract.

The observation sequence is:

1. Bounded provider/credential snapshot, expected-revision/enabled/client checks.
2. Current authenticated status under the existing bounded HTTP operation.
3. Provider revision and operation-active checks.
4. Scoped mapping on a DB connection acquired **after** network I/O.
5. Another provider revision check; return under the same operation deadline.

No database connection or transaction is held across the remote status request.
No media, provider configuration, category, torrent or status-cache write occurs.
Root/mapping changes must invalidate engine generations before publication;
provider revision checks alone cannot fence changes to those separate tables.

Unknown providers, missing scopes, credentials, failed requests, malformed status,
invalid paths, stale provider revisions, mapping failures and exhausted deadlines
return errors. A failed observation is never an empty successful output list or
proof of safety. The aggregate remains unevaluated when no collision is known and
any client observation failed; a definite collision remains a Warning despite
unrelated client failures. The current successful qBittorrent
result contains exactly one path. Errors expose static codes, never remote paths,
credentials or server response bodies. These private paths are not serialized
as a new public provider API.

## Verification and limits

A focused unit check in `src/providers/mod.rs` exercises POSIX normalization and
rejects unmapped UNC/drive-relative/invalid paths. Existing
`tests/qbittorrent_protocol.rs::client_status_derives_only_active_domain_roots_without_filesystem_authority`
provides status-protocol regression cases and passed in the full regression run.
The five API/worker tests in `tests/download_root_health.rs` establish:

- Both-domain category/default/relative root derivation and mapping.
- Equality/child warnings, sibling nonmatches and unused roots across both domains.
- Failed or stale observations staying unevaluated, including provider/root/mapping
  changes during a probe; no stale warning or healthy publication.
- No side effects, credentials or remote-path leakage.

Paths are lexical observations, not filesystem authorization or a guarantee
against symlink/bind-mount aliases. Identity mapping assumes the operator's
configured path namespace; it does not prove that a remote and local path refer
to the same mount. No filesystem probing or path mutation is added.

The plan acceptance remains “Same client safely processes both media types” and
“Both Add → Search/RSS → Grab → Download → Import → Library flows pass, including restart and failure recovery; original files survive failed replacement”.
This health observation alone does not re-prove either automation gate. qBittorrent
is the currently supported download client; no SABnzbd, NZBGet, Transmission or
other unimplemented provider coverage is claimed. Complete `hc.020` acceptance
is supported by the engine, schema and parent-run verification evidence together.
The required Rust and frontend checks passed, as did the full Rust suite (435
reported passes, zero failures, one existing ignored test). The behavior tests
passed both separately and within that full run.
