# Secret-safe process diagnostics

The server emits structured, allowlisted diagnostics. Do not format credentials,
configuration objects, request URLs/bodies, remote error messages, error source
chains or panic payloads. Escaping a string as JSON is not secret redaction.

`src/main.rs` consumes startup/server errors before Rust's `Result` termination
handler can print their raw `Debug` representation. Failures retain a nonzero
exit, an operation phase (`database_open`, `provider_key`, `metadata_client`,
`host_configuration`, `listener_bind`, `command_worker`, `http_server`) and a safe error
class. Database classes distinguish connection/protocol/SQL/other failures; I/O
errors report `ErrorKind`. Unknown errors never fall back to their message.
The backup-created notice omits the configured database path; recovery artifacts
remain beside that database with their existing manifest. No backup is removed.

Known storage compatibility failures also retain the safe capability and validated
engine identity from `StorageCompatibilityError`. Existing migration/ownership
failures represented only as generic boxed strings retain phase/class, not their
text: typed migration errors would permit more precise diagnostics later.

## Current source audit

Inspected both executables and every `print!`, `eprint!`, `println!`, `eprintln!`,
`dbg!`, `log::` and `tracing::` occurrence under `src/`. There is no configured
trace subscriber, HTTP trace middleware, SQL parameter trace, log file/database
writer or remote log exporter. Dependency trace calls have no installed collector.

| Sink | Emitted information / disposition |
| --- | --- |
| Server entry point | Safe phase/class; backup-created event; parsed socket address; fixed disabled-worker reason |
| `generate-api` entry point | Static missing/stale/usage messages from `api_contract`; OS errors for generated artifact directory/write. Does not open application DB, read provider credentials or print supplied artifact path/content. Retained unchanged. |
| Providers | Database enum discriminant, generated correlation ID; numeric provider/revision, static test status/code. Transport bodies/credentials are not formatted. |
| History, blocklist, command API, naming API | Database discriminants; counts and generated correlation IDs |
| Command worker, metadata, manual-import, quality-reset, blocklist, RSS | Fixed recovery/storage codes, numeric/UUID command/candidate IDs, typed media/status and allowlisted outcome codes |
| Import / filesystem | SQL numeric codes/discriminants, OS numeric codes/ErrorKind, generated operation UUIDs, fixed conditions; import `diagnostic` is built from those types, not raw DB errors |
| Snapshot cleanup | Fixed owner-only staging cleanup instruction |
| Naming destination / owned import | Fixed internal conditions; typed static naming field and parse/render failure class; no template or substituted media text |
| Scratch cleanup messages in command unit tests | Raw I/O errors are under `#[cfg(test)]`, excluded from production binaries |

`DestinationError::Render` carries only static field/class identifiers. Invalid stored
naming templates can bypass normal PUT validation (for example after corruption),
so parser `Display`/`Debug` text is discarded at the destination resolver. The
worker still blocks the item with `naming_render_failed`; it logs the affected
field and `invalid_template` or `render_failed`, without changing naming behavior.

The audit traced values into each sink: provider credentials/API keys, indexer
parameters and qBittorrent session cookies remain in transport/configuration code;
HTTP error bodies become typed error codes. SQL diagnostics use numeric codes or
enum discriminants, filesystem diagnostics use `ErrorKind`/OS codes, and stored
worker error codes have closed database constraints. Operation identifiers are
UUIDs created or parsed by the operation/command boundaries. No secret-bearing
production panic path was found in those credential/transport paths; test-only
`unwrap`/fixture output is excluded from the production sink inventory.

## Executable evidence

Run:

```sh
cargo test --locked --test startup_logging --test runtime_logging --bin hrrdarr
```

`remote_failure_after_engine_preflight_never_logs_upstream_secrets` runs the real
server executable with a cleared environment and an owned `127.0.0.1:0` Hrana
fixture. Valid engine/JSON replies pass preflight; the later foreign-key setup
returns an upstream secret sentinel. The child must fail, identify database-open
phase and omit both that sentinel and the synthetic auth token from combined
stdout/stderr. The same test against the original entry point failed specifically
because the upstream secret reached process diagnostics. A second fixture branch
reports an old engine version and verifies the safe compatibility capability and
engine identity survive while connection setup remains unattempted.

`invalid_environment_values_have_safe_phase_diagnostics` covers malformed provider
key, malformed bind address, malformed allowed-host list and inaccessible local database path without printing
the supplied values. All subprocess captures have time/output bounds and cleanup; no live
services or real credentials are used.

`runtime_provider_and_background_diagnostics_exclude_secret_values` launches the
actual server with a cleared environment, scratch database/files and owned HTTP
peers. The ephemeral listener notice reports its actual bound address. The peer
asserts the synthetic indexer API key, client username/password and session cookie
are really used. The captured combined stdout/stderr covers:

- Successful and rejected-authentication provider tests, including an upstream
  error-body sentinel, with both success/failure structured events retained.
- Real TV and movie RSS → grab → completed-download workers, each blocked by a
  corrupted template containing a secret sentinel, with the correct naming field
  and safe error class retained.
- Successful and failed TV/movie download-refresh commands, with persisted terminal
  outcomes and process settlement events.

The runtime test failed against the old naming diagnostic because the template
sentinel reached actual process output; it passes with source classification.
The whole exercise has a 90-second deadline, bounded HTTP requests and a 64-KiB
capture assertion; child kill/reap, mock cancellation and scratch cleanup are RAII.
This evidence plus the complete current sink/source audit supports `per.005` for
the implemented application. New providers, log sinks, tracing collectors or auth
settings must preserve the same boundary and add relevant capture evidence.

## Not claimed

The runtime capture is a representative exercised path, not exhaustive fault
injection into every sink. The complete sink inventory is verified from source;
the scenarios above are observed process behavior. The default Rust panic hook
remains unchanged; this is not proof against arbitrary future panic payloads,
memory dumps, hostile process instrumentation or a separately installed dependency
trace collector. No live Turso service or real provider is covered. Unimplemented
providers and future authentication/host settings retain their separate parity
requirements. No new dependencies or persistent application state were introduced.
