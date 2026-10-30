# Import policy settings

Media management → Import policy edits separate TV and movie configuration through
`GET /api/v1/{tv|movies}/import-policy` and `PUT` on the same route. Responses include
`media_type`, `revision`, `skip_free_space_check_when_importing`,
`minimum_free_space_mib`, and `download_client_working_folders`. Updates include the
saved revision and all three settings, without a media-type member.

Minimum free space is an integer from 100 through 2,147,483,647 **MiB**. One MiB is
1,048,576 bytes. Enabling “Skip free-space check when importing” retains the minimum
value; it does not remove validation of that value. Working-folder prefixes are
case-sensitive literals separated by `|`. Whitespace and empty segments are retained
exactly. Only an entirely empty value restores `_UNPACK_|_FAILED_`. The limit is
4096 UTF-8 bytes, rather than 4096 characters. The editor submits the empty value
unchanged and displays the server's normalized response after saving.

The panel retains its draft and pending request across workspace navigation.
Changing media domain or explicitly reloading a dirty draft asks before discarding
it. Writes use the saved revision. A stale revision retains the draft and requires
reload; a missing, unreadable, or server-error response likewise locks further
writes until explicit readback succeeds. Failed readback leaves that lock in place.
There is no automatic write retry. Loading and saving responses are guarded by the
component lifetime and request epoch. A response for another domain is not adopted.

This singleton uses the existing settings-write states independently of unrelated
revision, naming, and quality settings. No new dependency is introduced.

## Verification

`frontend/tests/import-policy.test.mjs` covers numeric bounds, UTF-8 byte limits,
exact prefix preservation, domain-scoped request serialization, and conflict or
uncertain write results without automatic retries.
`frontend/tests/import-policy-browser.mjs` uses the owned loopback library fixture
and real policy routes for both-domain isolation, dirty-draft confirmation,
navigation retention, normalized empty values, concurrent-writer conflicts, and a
committed write whose response is lost. An intercepted failed readback checks that
another write remains blocked until successful readback. Run with `UI_URL` and the
existing Playwright installation as for `revision-policy-browser.mjs`.

## Not claimed

Downloaded-folder scans consume the saved domain policy: observation rejects
working-folder prefixes and insufficient free space before discovery admits a file.
The settings-editor tests do not establish enforcement in every other import path,
filesystem safety under low disk space, full Media Management parity,
or Sonarr/Radarr runtime equivalence. Browser tests use isolated local state, not
production providers or media. Adding tests does not itself establish that they ran;
record execution results separately.
