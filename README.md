# hrrdarr

hrrdarr is a movie and TV library automation project targeting the combined
feature sets of Radarr (movies) and Sonarr (TV). The current first slice is a
Rust/Axum API with a Svelte UI backed by libSQL/Turso; movie support and full
automation are planned.

The local [combined parity plan](.do-not-commit/research/hrrdarr-parity-plan.md) defines the delivery slices
and acceptance criteria. Research and upstream reference checkouts live in the
ignored `.do-not-commit/` directory.

Run locally with the embedded engine:

```sh
HRRDARR_DATABASE_PATH=/tmp/hrrdarr.db cargo run
```

Run against a Turso/libSQL endpoint by setting `TURSO_DATABASE_URL` and
`TURSO_AUTH_TOKEN`. The API listens on `127.0.0.1:8787` by default; override
with `HRRDARR_BIND`. The default local database filename is `hrrdarr.db`.
When upgrading an existing checkout, set `HRRDARR_DATABASE_PATH` to its existing
database file to preserve the selected library.

The API currently exposes library listing, episode listing, and validated
import previews. The frontend is developed with `npm install && npm run dev`
from `frontend/`; production static serving is the next integration step.
