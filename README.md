# Ultrasonic

The first slice is a Rust/Axum API with a Svelte UI backed by libSQL/Turso.

Run locally with the embedded engine:

```sh
ULTRASONIC_DATABASE_PATH=/tmp/ultrasonic.db cargo run
```

Run against a Turso/libSQL endpoint by setting `TURSO_DATABASE_URL` and
`TURSO_AUTH_TOKEN`. The API listens on `127.0.0.1:8787` by default.

The API currently exposes library listing, episode listing, and validated
import previews. The frontend is developed with `npm install && npm run dev`
from `frontend/`; production static serving is the next integration step.
