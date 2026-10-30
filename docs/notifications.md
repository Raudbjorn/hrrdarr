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
| `POST /{id}/test`, `POST /testall` | Test saved notifications |
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
| File delete | delete from `episode_files` / `movie_files`; reason `Manual` (file_deletions journal), `Upgrade` (replacement journals), `NoLinkedEpisodes`, else `MissingFromDisk` |
| Health issue / restored | insert into `health_transitions` with `in_grace=0` (the 1024-row health ring is not an outbox, so the trigger copies what is needed) |
| Manual interaction | `rss_candidates.status` becomes `needs_attention` |
| Rename, Application update | accepted by `notifications::events::emit` and rendered, but **nothing emits them yet**: there is no rename executor or updater in the codebase |

`notification_events` keeps the newest 2048 events. The worker (`notifications::start`, run only by
the process that owns the database) polls every 2 s: it fans events out to one
`notification_deliveries` row per matching notification (enabled, domain, trigger flag, tag scope),
then claims due deliveries under a 120 s lease and sends them (max 4 concurrent).
Migration `0079_notification_delivery_scope.sql` records the matching domain on each delivery,
including system events. Retries retain that choice even if trigger or warning filters change.
Legacy rows without a recorded domain select a currently matching scope once before sending;
if no scope matches, they fail with `notification_scope_disabled`.

Delivery is **at least once**. Retryable failures (timeout, transport error, 5xx, 429) back off
30 s, 2 min, 8 min, 32 min... capped at 6 h (a server `Retry-After` can lengthen it) up to 5
attempts; permanent failures (4xx, 401/403, redirect, invalid config, script failure) are parked as
`failed` at once. A process crash between send and bookkeeping re-sends after the lease expires.
Custom scripts are never retried automatically (they may not be idempotent).

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
timeout (process group killed), output capped at 8 KiB per stream. Exit status 0 is success.
stdout is logged at DEBUG and stderr at WARN for both domains (Sonarr logs stderr as an error,
Radarr as information; hrrdarr deliberately uses one middle level).

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
* **Rename / ApplicationUpdate**: variable sets are rendered but no emitter exists yet.
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
* Rename and Application-update triggers are stored and renderable but never fired.
* Import-complete (TV batch) is stored but never fired.
* Delivery is at-least-once, not exactly-once; there is no per-target ordering guarantee.
* Event capture was exercised with fixture rows and the real trigger paths for series/movie
  add/delete, file delete, health, import receipts; the live RSS grab path was not run end to end here.
* Sonarr/Radarr V3 wire compatibility (`/api/v3/notification`) is not provided.
* The outbox drops its oldest events beyond 2048; a long outage can lose undelivered events.
