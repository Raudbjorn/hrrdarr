# Download and import path diagnostics

The scoped `remote_path_mapping` Health check observes configured client output and retains evidence from unfinished TV and movie imports. It uses the existing client transport, remote path resolver and import journal. A Health recheck never retries an import or clears its evidence.

A reported path is checked in the hrrdarr filesystem namespace. A missing or incompatible path prompts review of client output settings, mappings and mounts. Container advice uses observed container evidence. A non-loopback endpoint has unknown locality; it is not proof that the client uses a different filesystem. Permission denial is reported only when measured for the executing service identity. Messages omit private paths and endpoints.

A prepared import has an approved source identity. A recorded missing source before commit can therefore be distinguished from a failed lookup before preparation. Source, root, destination, staging and replaced-file cleanup failures retain separate causes. Source absence during cleanup does not establish that a source disappeared before import; a Move operation can intentionally retire its source after committing the library file.

## Recovery

Manual import and completed-download processing display retained diagnostic guidance even when their ordinary error field is empty. An unresolved-attempt marker means an outcome is not confirmed; it does not prove a particular filesystem failure.

The backend supplies `path_failure_code` and `recovery_required` on operation and processing responses. Recovery availability accounts for the current operation lease. The UI offers Resume only when the backend reports recovery is required, and prevents another request while a resume is pending. Recovery reuses the same recorded operation and rechecks its authority on the server.

An untouched preview can still be explicitly executed, including when a lost authorization request left that same preview unstarted. Saved browser receipts from earlier versions are refreshed before enabling another authorization. A failed status refresh does not grant permission to resume.

Repairing a directory or restoring source accessibility does not clear the retained diagnostic. Explicitly retry or resume the same operation after reviewing the reported cause. The marker clears only when the operation, including required cleanup, completes. Completing one operation does not clear another operation's evidence. Cancelling a receipt or disabling its scope changes applicability; it does not prove the filesystem issue was repaired.

## Verification boundaries

The diagnostic implementation has finite record and observer bounds. Corrupt, missing or oversized required evidence remains unevaluated; it must not become a healthy result. Completed-download handling disabled makes the check inactive, without claiming path repair.

Focused Rust and frontend regressions cover the contract. Their execution and real browser acceptance are recorded separately by the integrating verification run; this document does not claim those checks have passed. Source-derived Sonarr/Radarr requirements are not a claim of live provider equivalence, power-loss recovery or complete parity.
