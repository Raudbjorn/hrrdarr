# General settings panel (read-only)

The "General" workspace tab renders the effective host configuration from
`GET /api/v1/config/host` (`HostSettings`, see [host-api.md](host-api.md)).

Shown: authentication (none), configured and bound bind address with source,
allowed hosts and trusted networks with sources, and the documented limits:
allowed hosts validate the HTTP authority and do not restrict client IPs;
settings are deployment environment variables, immutable, and applied by an
external restart. An empty allowed-hosts list means filtering is disabled.

Deliberately absent: any editable control or save action (there is no PUT).
API key, authentication credentials, SSL/TLS, URL base, log level, update and
backup settings are listed under "Not available in this build"; they are not
implemented.

Pure presentation logic lives in `frontend/src/lib/general-settings.ts`
(tests: `frontend/tests/general-settings.test.mjs`). Responses that do not match
the contract (unknown enum values, wrong types) render an error state, not
partial data. Loading and failure states use `role="status"` / `role="alert"`
with a reload button. No browser-level verification exists for this panel.
