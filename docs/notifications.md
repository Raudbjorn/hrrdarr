# Notifications (Connect)

Sonarr's "Connections" and Radarr's "Connect", delivered as one domain-aware subsystem: a registry of
notification targets, a durable event outbox fed by database triggers, and a dispatcher with
per-delivery retry. Code lives in `src/notifications/`; migration `0063_notifications.sql`.

## API

All routes are under `/api/v1/notifications` (generated types: `Notification*` in `frontend/src/lib/api.generated.ts`).

| Route | Purpose |
| --- | --- |
| `GET /` , `POST /` | List / create |
| `GET /{id}`, `PUT /{id}`, `DELETE /{id}` | Read / update (revision fenced) / delete |
| `GET /schema` | Implementation catalog: field schema driving validation and the UI form |
| `POST /test` | Test an unsaved draft (`{draft, existing_id?}`); `existing_id` lends stored secrets |
| `POST /{id}/test` | Test one saved notification |
| `POST /testall` | Test every **enabled** saved notification (disabled ones are skipped and not listed in the result) |
| `GET /deliveries?limit=` | Recent delivery journal (class codes only) |

A notification has an `implementation`, non-secret `settings`, write-only `secrets`, and up to two
**scopes**: `tv` and `movies`. Each scope carries its own trigger set and tag IDs. TV and movie
triggers are different types (`TvNotificationTriggers`, `MovieNotificationTriggers`); a movie-only
trigger on a TV scope is rejected. There is no unqualified integer id anywhere in an event.

Triggers: on grab, on import, on upgrade (requires on import: an upgrade is an import that replaced a
file), on rename, series add / movie added, series delete / movie delete, episode file / movie file
delete (+ "for upgrade"), health issue (+ include warnings), health restored, application update,
manual interaction required, and TV-only on import complete. An empty tag list means every series
(or movie); otherwise the subject must carry one of the tags. Health and update events are not
tag-filtered.

### Secrets

Secret fields are stored in one AES-256-GCM envelope bound to the notification id and
implementation, using the same independently stored key as providers (`HRRDARR_PROVIDER_KEY`).
Responses list only `secrets_configured` names. On update an omitted secret is kept, `null` clears
it (not allowed for required secrets). Without the key, creating a notification that has secrets
fails with 503; secretless implementations (for example Kodi without a password) still work.
Errors, logs, the delivery journal and the test results never contain URLs, tokens or response bodies.
Custom-script diagnostics record only the stream name and captured byte count; script paths and
stdout/stderr text are excluded because they can contain private event data or secrets.

### Validation

Strict and bounded: unknown settings/secrets rejected, secrets sent as settings rejected, types and
ranges per schema, control characters rejected, at most 64 notifications, 200 tags per scope, 64 KiB
request bodies, unique case-insensitive names, tag IDs must exist in that scope's domain.

## Events and the outbox

Events are captured by SQL triggers on the tables the real paths already write, so an event commits
or rolls back with the state change that caused it, and nothing is recorded unless an enabled
notification has the matching trigger for that domain.

| Event | Source (real path) |
| --- | --- |
| Grab | `rss_candidates.status` becomes `observed` (the grab journal used by RSS and search grabs) |
| Import / Upgrade | insert into `import_history`; upgrade = the operation has a replaced-file record (`rss_candidate_imports.old_file_json` or `scan_import_replacements`) |
| Series add / Movie added | insert into `series` / `movies` |
| Series delete / Movie delete | delete from `series` / `movies` (title, path, tags snapshotted) |
| File delete | manual deletion of `episode_files` / `movie_files`, or retirement of a replaced original; reason `Manual` (an unfinished `file_deletions` journal: `admitted`, `copied` or `committed`; aborted and completed journals do not count), `Upgrade` (replacement retirement), `NoLinkedEpisodes`, else `MissingFromDisk` |
| Health issue / restored | insert into `health_transitions` with `in_grace=0` (the 1024-row health ring is not an outbox, so the trigger copies what is needed) |
| Manual interaction | `rss_candidates.status` becomes `needs_attention` |
| Rename | a `rename_commands` row settling (`succeeded`, `failed` or `cancelled`) with `items_renamed > 0`; one event per series or movie listing the paths its `rename_events` rows renamed, cut to the 16 KiB event cap (`rename_events` stays the complete record). Items recovered after the command settled are not announced |
| Application update | accepted by `notifications::events::emit` and rendered, but **nothing emits it yet**: there is no updater in the codebase |

`notification_events` keeps the newest 2048 events. The worker (`notifications::start`, started by
every server process) polls every 2 s: it fans events out to one
`notification_deliveries` row per matching notification (enabled, domain, trigger flag, tag scope),
then claims due deliveries under a 120 s lease and sends them (max 4 concurrent). A heartbeat renews
the lease every 30 s, including time spent waiting for a delivery slot. Each claim has a unique owner
token; renewal, acknowledged-request progress and completion writes require that token and an unexpired
lease. Losing ownership or failing renewal stops the attempt, though a request already sent may still
have arrived. The worker owns no library file, and fan-out is idempotent. Each delivery
records the scope that matched (`media_type`): a system event (health, update) is rendered for that
domain. Retries retain the selected scope even after filters change. A notification whose movies
scope asked for health events gets `radarr_*` variables and
the movie app path even when its TV scope exists. When both scopes match, TV is used. Health
warnings are gated by `include_health_warnings` in both directions: a restored transition carries
the severity of the issue it clears, so an unannounced warning is not announced as restored.

Delivery is **at least once**. Retryable failures (timeout, transport error, 5xx, 429) back off
30 s, 2 min, 8 min, 32 min... capped at 6 h (a server `Retry-After` can lengthen it) up to 5
attempts; permanent failures (4xx, 401/403, redirect, invalid config, script failure) are parked as
`failed` at once. A process crash between send and bookkeeping re-sends after the lease expires.
Custom scripts are never retried automatically (they may not be idempotent).

A delivery that is several requests (ntfy topics, Pushbullet devices and channels, Emby/Jellyfin
notify plus library update) records a 64-bit identity of each acknowledged request in `sent_parts`,
so a retry sends only the requests that were not acknowledged. Requests can carry secrets, so the
identity is an HMAC under a sub-key derived from the credential key: reading the database does not
allow checking guessed secrets against it. Without a credential key no secret can be stored, and a
plain digest is used. Rotating the key changes every identity, so a retrying delivery of a notification without secrets re-sends parts it
had already acknowledged. Rotation also makes every stored secret unreadable: a notification with secrets fails
permanently as `credentials_unavailable` (its pending deliveries are parked as `failed`) until its secrets are
entered again, so rotate the key only together with re-entering them. A request that timed out may still have
arrived, so this narrows duplicates rather than excluding them. Plex refreshes are not tracked: they
follow a fresh section listing and repeating one is harmless.

Disabling a notification **holds** its pending deliveries: they are not claimed, spend no attempt,
and resume when it is re-enabled. Events that occur while it is disabled are not recorded for it.
Deleting a notification discards its deliveries.

## Tests

`POST /test`, `POST /{id}/test` and `POST /testall` send a synthetic Test event to each configured
domain, both domains concurrently, each limited to 40 s. `/testall` covers the **enabled**
notifications only: a disabled definition is not attempted and does not appear in the result.
At most 4 notification tests run at once
across all test requests. One notification's test finishes within 45 s (the client waits 60 s);
`/testall` runs notifications concurrently and finishes within 90 s (the client waits 120 s).
Anything unfinished at the deadline reports `timeout`; such a send may still have been delivered.

## HTTP policy

Same rules as provider transport: http(s) only, no URL credentials or fragments, redirects never
followed, no ambient proxy, 3 s connect / 10 s request timeout, bounded bodies, identity encoding.
Additionally, targets in link-local (incl. cloud metadata `169.254.0.0/16`, `fe80::/10`,
`fd00:ec2::/32`), unspecified, multicast and broadcast ranges are refused, both for literal hosts at
write time and for every resolved address at connect time (DNS rebinding). Loopback and private
ranges stay reachable because LAN targets (Gotify, ntfy, Kodi) are the normal case.

## Implementations

Verified against an in-process fake HTTP server (`tests/notifications_api.rs`): webhook, discord,
slack, telegram, ntfy, gotify, pushover, pushbullet, pushsafer (movies only upstream), simplepush,
prowl, join, pushcut, sendgrid, mailgun, notifiarr, apprise, signal (signal-cli-rest-api), kodi,
media_browser (Emby/Jellyfin), plex, custom_script. Request shape, auth header, and body fields are
asserted per provider.

Not implemented:

* **Email (SMTP)**: no SMTP crate is a dependency and a TLS-capable client is not lightweight. Use
  SendGrid/Mailgun, or a script/webhook bridge.
* **Twitter**: the OAuth1 status API is closed to new use; not attempted.
* **Synology Indexer**: shells out to a Synology-only binary; cannot be tested here.
* **Trakt**: needs an OAuth device flow plus list semantics; not attempted.
* **Plex.tv sign-in**: Plex takes a server token, not a plex.tv login.

Fixed-endpoint providers (Pushover, Telegram, Pushbullet, ...) point at the real vendor hosts; tests
rewrite them with a test-only origin map. Their wire formats follow the vendors' public documentation
as remembered and were **not** exercised against the real services. Notifiarr in particular is
unverified.

## Custom script contract

`custom_script` runs `path` (absolute, normalized, an executable regular file) with `arguments`
split on whitespace (double quotes group; no escapes, expansion or shell). Environment is cleared
to `PATH=/usr/local/bin:/usr/bin:/bin` plus the event variables; cwd `/`, stdin closed, 30 s
timeout, output capped at 8 KiB per stream. Exit status 0 is success. When the script exits or
times out its whole process group is killed, so work it backgrounds (`cmd &`) does not outlive the
event; a script that must leave a daemon behind has to detach it (`setsid`) and close its output.
Output still open 0.5 s after that is cut off.
Only captured byte counts are logged: stdout at DEBUG and stderr at WARN for both domains.
The script path and captured text never enter runtime logs.

TV events use `sonarr_*`, movie events `radarr_*`. Every event sets `<app>_eventtype`,
`<app>_instancename` (`hrrdarr`) and `<app>_applicationurl` (empty). Implemented subset of the
community contract (`research/wiki/custom-scripts.md`):

* **Grab** (`Grab`): series/movie id, title, year, tvdbid/tmdbid/imdbid, tags (`|`), `release_title`,
  `release_indexer`, `download_client`, `download_id`; TV adds `release_episodecount`,
  `release_seasonnumber`, `release_episodenumbers`, `release_episodetitles`, `release_episodeairdates`.
* **Import/Upgrade** (`Download`): `isupgrade`, identity fields, `series_path`/`movie_path`,
  `episodefile_*` (id, path, relativepath, episodecount, episodeids, seasonnumber, episodenumbers,
  episodetitles, sourcepath, sourcefolder) or `moviefile_*` (id, path, relativepath, sourcepath,
  sourcefolder).
* **File delete** (`EpisodeFileDelete` / `MovieFileDelete`): `*_deletereason`, identity, file id/path/relativepath.
* **Add/Delete** (`SeriesAdd`, `SeriesDelete`, `MovieAdded`, `MovieDelete`): identity fields (+ path on delete/movie add).
* **Health** (`HealthIssue`/`HealthRestored`): `<app>_health_issue_level|message|type|wiki` (or `restored`).
* **ManualInteractionRequired**: identity plus release/client fields as for Grab.
* **Rename**: `*_episodefile_paths` / `*_moviefile_paths` and the `previous` variants, `|`-joined. **ApplicationUpdate**: rendered, but no emitter exists yet.
* **Test**: only the three base variables.

Not provided: quality, quality version, release group, scene name, indexer flags, custom format
fields, media info fields, deleted-file lists (`*_deleted*`), `series_titleslug`, `tvmazeid`,
`originallanguage`, genres, the batch "Import Complete" event, `*_series_deletedfiles` and
`radarr_movie_folder_size` (the delete trigger does not know whether files were removed).
`sonarr_series_path` is present only on events with a path. The health payload is the wire shape of
the health-check catalog (`type` = the check's compatibility type).

## Library updaters

`kodi` (GUI notification, `VideoLibrary.Scan`/`Clean` over JSON-RPC), `media_browser`
(`/Library/Media/Updated` with changed paths, or `/Library/Refresh`; optional admin notification),
and `plex` (lists sections, refreshes only sections whose location contains the changed path) react
only to events that change files; other events are skipped.

## Not claimed

* No real third-party service was contacted; vendor wire formats are unverified against live APIs.
* The Application-update trigger is stored and renderable but never fired.
* Import-complete (TV batch) is stored but never fired.
* Delivery is at-least-once, not exactly-once; there is no per-target ordering guarantee.
* Event capture was exercised with fixture rows and the real trigger paths for series/movie
  add/delete, file delete (including the upgrade retirement of a replaced original), health, import
  receipts; the live RSS grab path was not run end to end here.
* Sonarr/Radarr V3 wire compatibility (`/api/v3/notification`) is not provided.
* The outbox drops its oldest events beyond 2048; a long outage can lose undelivered events.
* A health check that escalates from warning to error emits no new transition (health engine
  behaviour), so a notification that excludes warnings is not told about the escalation.
* Held deliveries of a disabled notification resume on re-enable however old they are, bounded only
  by the 2048-event outbox.
* The dispatcher runs in every server process, including on a remote database. Fan-out is
  idempotent and delivery leases are renewed and owner-fenced. Tests use competing dispatchers over
  one local database; two live processes over a remote database were not exercised. Restart all
  dispatchers when upgrading to migration 0076: older binaries do not honor the new owner token.
