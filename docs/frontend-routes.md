# Frontend routes

The UI uses dependency-free hash routing (`#/...`), implemented by the pure table in `frontend/src/lib/routes.ts` and wired in `frontend/src/App.svelte`. There is no history API use and no server fallback: the backend serves one document and the fragment never reaches it, so every URL below works as a deep link.

| Hash | View |
| --- | --- |
| `#/` (also empty, `#`) | Library |
| `#/settings` | Settings index |
| `#/settings/providers` | Providers (indexers and download clients) |
| `#/settings/quality` | Quality definitions |
| `#/settings/profiles` | Profiles |
| `#/settings/naming` | Naming |
| `#/settings/mediamanagement` | Media management |
| `#/settings/customformats` | Custom formats |
| `#/settings/tags` | Tags |
| `#/settings/general` | General |
| `#/activity/queue` | Activity |
| `#/activity/blocklist` | Blocklist |
| `#/activity/history` | History |
| `#/system/health` | Health |
| `#/system/rss` | RSS |
| `#/add/import` | Import existing library |

Parsing rules: one trailing slash and any `?query` are ignored; paths are case-sensitive; each segment is percent-decoded separately (malformed encoding or an encoded `/` is not-found, never an exception); hashes over 256 characters are rejected. Anything else shows the not-found page, which displays the path as bounded text with control characters replaced (Svelte text interpolation only, never HTML).

The settings index links only sections that exist. Import lists, metadata, connect/notifications, UI settings, backup, updates and logs are listed as "Not available in this build" without links.

## Verification status

`npm --prefix frontend run test` covers the pure route table. Browser behavior (back/forward, deep-link focus, panel regressions) was not verified when Playwright Chromium is unavailable; no browser evidence is claimed here.
