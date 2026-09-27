# Secret-safe process diagnostics

The server emits structured, allowlisted diagnostics. Do not format credentials,
configuration objects, request URLs/bodies, remote error messages, error source
chains or panic payloads. Escaping a string as JSON is not secret redaction.

`src/main.rs` consumes startup/server errors before Rust's `Result` termination
handler can print their raw `Debug` representation. Failures retain a nonzero
exit, an operation phase (`database_open`, `provider_key`, `metadata_client`,
`bind_address`, `listener_bind`, `command_worker`, `http_server`) and a safe error
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
| Naming destination / owned import | Fixed internal conditions; exception below |
| Scratch cleanup messages in command unit tests | Raw I/O errors are under `#[cfg(test)]`, excluded from production binaries |

An unresolved boundary is `commands/processing.rs`'s `naming_render_failed` detail:
`naming/destination.rs` reads stored naming templates and formats parser/render
errors. Invalid-token diagnostics can contain template text. Normal naming PUT
validates templates before persistence, so this is not a demonstrated provider
credential leak through a supported update. Corrupt/imported stored templates
still need a dedicated log-capture test and typed error classification before
claiming all diagnostic values safe. No naming/search behavior was changed here.

## Executable evidence

Run:

```sh
cargo test --locked --test startup_logging --bin hrrdarr
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
key, malformed bind address and inaccessible local database path without printing
the supplied values. All subprocess captures have time/output bounds and cleanup; no live
services or real credentials are used.

## Not claimed

This is startup-boundary evidence, not full `per.005` closure. Existing
provider response and encrypted-storage tests do not themselves capture every
production provider/background log path. The default Rust panic hook remains unchanged; no production panic carrying a
credential was reproduced. Those runtime captures and the stored
naming-template diagnostic remain outstanding. No live Turso service, remote log
collector, unimplemented provider or future authentication/host setting is covered.
No new dependencies or persistent application state were introduced.
