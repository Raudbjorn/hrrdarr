# Native provider configuration

The first provider foundation stores Torznab, Newznab and qBittorrent configuration for TV, movies, or both. Saving it does not contact the endpoint. Full provider controllers, searches, downloads, capability negotiation and network tests remain incomplete.

## HTTP contract

| Method and route | Contract |
| --- | --- |
| `GET /api/v1/providers` | `ProviderQuery`: limit defaults to 50 (1–100), offset defaults to 0, optional `media_type=tv|movies`. Returns `ApiPage<Provider>`, ordered by priority, name, UUID. |
| `POST /api/v1/providers` | `ProviderInput`; creates revision 1, returns 201 and the stored public provider. |
| `GET /api/v1/providers/{uuid}` | Returns the public provider or 404. |
| `PUT /api/v1/providers/{uuid}` | `ProviderUpdate`: full configuration plus current revision. Returns the stored next revision; stale revision or changed implementation returns 409. |
| `DELETE /api/v1/providers/{uuid}?revision=N` | Explicit configuration deletion with revision check; returns 204. This does not delete media or client downloads. |
| `POST /api/v1/providers/{uuid}/test` | Returns 501 `provider_test_not_implemented` for an existing configuration. No network action or status transition. |

`ProviderSettings` is a closed implementation-tagged union. `endpoint`, `tv` and `movies` are required; absent scopes are explicit `null`, and at least one scope must exist. Torznab and Newznab TV settings contain `categories` and `anime_categories`; movies contain `categories`. Each list holds at most 64 distinct positive signed-32-bit IDs. Movie categories and the combined TV categories must be nonempty. These are configured category IDs, not negotiated capabilities.

qBittorrent scopes contain `category`, nullable `imported_category`, `recent_priority` and `older_priority`. Category names are 1–64 UTF-8 bytes without control characters, backslashes, doubled slashes or leading/trailing slashes; nested single slashes are supported. Active and imported categories must differ within a scope and between TV/movie scopes. Priorities are exactly 0 (Last) or 1 (First), matching the pinned references' `QBittorrentPriority` enum. Other provider priority concepts are not interchangeable. The top-level priority is 1–100, name 1–128 UTF-8 bytes and enabled is explicit. Disabling preserves settings and credentials.

Endpoints are HTTP(S) URLs, at most 2048 UTF-8 bytes, with a host and no user info, query, fragment, control characters, raw backslashes or surrounding whitespace. The configured endpoint path is public: put credentials in the credential field, never in a path. No TLS bypass, redirects, proxy or network reachability is inferred by saving a URL.

Requests are capped at 32 KiB; responses at 1 MiB. Bounded scopes and pages also bound in-memory reads. Reads use a consistent database transaction. Mutations atomically update the provider, encrypted credentials and scopes, construct/check the public response before commit, and explicitly roll back errors. Stored implementation, identity and settings version cannot change. Revision increments exactly once; update rejects unsafe JavaScript integer revisions and exhaustion. SQL failures log a random correlation ID and error variant only, without parameters or error text. Public validation/database/credential errors are static and never include submitted JSON or provider responses.

Public responses contain `has_credentials`, `test_supported: false` and `test_status: "never_tested"`. They never contain credential ciphertext or plaintext. These values describe the shipped implementation, not successful validation against a remote service. Settings and queries are generated from Rust in `frontend/src/lib/api.generated.ts`; unknown input fields are rejected.

## Credentials and key recovery

Credentials are write-only `{"kind":"api_key","api_key":"..."}` or, for qBittorrent only, `{"kind":"username_password","username":"...","password":"..."}`. The alternatives cannot be combined. Strings are nonempty, have no control characters and are at most 4096 UTF-8 bytes each; the serialized secret also must fit the encrypted envelope. Omitted credentials preserve the existing ciphertext, explicit `null` clears it, and an object replaces it. A configuration without credentials can be stored; this is not an assertion that anonymous authentication works.

Supply `HRRDARR_PROVIDER_KEY` through the service's protected secret facility as exactly 64 hexadecimal characters encoding 32 cryptographically random bytes. Generate it once using an OS cryptographic random source; never use a password or commit it. The application never generates, persists, returns or logs this key. A malformed supplied environment value stops startup with a static error. An absent key allows redacted reads and credential-free configuration, but cannot save a new secret or mutate/delete any credential-bearing record. A wrong key or corrupted envelope likewise prevents mutation, including replacement and clearing, preserving recoverability with the correct key.

Existing ring 0.17.14 AES-256-GCM encrypts the complete typed credential value with a fresh random 96-bit nonce. Envelope version 1 is `version-byte || nonce || ciphertext-and-tag`, bounded to 16 KiB. Authenticated associated data binds the envelope to its provider UUID, implementation and credential format version. Copying ciphertext to another provider or changing implementation therefore cannot unlock it. Key material and plaintext necessarily exist in process memory; this is encryption at rest, not memory-dump protection or protection from the running service account.

Back up the deployment key independently using the operator's protected secret backup process. A database backup contains encrypted provider credentials but **does not contain this key**. Restore requires the matching key. Keep a recovery copy before changing service environment settings; on key mismatch restore the original key rather than recreating provider definitions. There is no key rotation/re-encryption endpoint in this foundation: changing the environment key is not rotation and locks existing rows. Any future rotation must decrypt and re-encrypt all rows transactionally and retain independent recovery material. Existing archived snapshot provider settings are not activated or rewritten by this API; those private archives may contain plaintext and still require their existing storage protections.

## Evidence and limits

`src/providers/credentials.rs` tests fresh nonces, decrypt roundtrip, wrong-key failure, tampering, unknown envelope versions and provider/implementation binding. `tests/provider_config_api.rs` exercises real temporary HTTP endpoints and databases for both domains, credential omission/replacement/clearing, static error redaction, revision conflicts, failed-write rollback and key-loss behavior. Migration tests cover schema8 upgrade/reopen and SQL constraints. No live services, accounts or real media are used.

This does not prove remote libSQL transport recovery, production backup/key custody, authentication of hrrdarr itself, protocol compatibility, browser workflows or full Sonarr/Radarr parity. Endpoint configuration is local validation only. There is no scheduled health engine, successful remote test, factory for unavailable providers, snapshot provider activation or unused family trait.

Dependencies: existing [ring AEAD API](https://docs.rs/ring/0.17.14/ring/aead/) and newly pinned [url 2.5.8](https://docs.rs/url/2.5.8/url/) for standard URL parsing. Cargo.lock adds url and its IDNA/Unicode dependency graph (25 packages); existing locked package versions remain unchanged. No HTTP client dependency is introduced before a concrete transport consumes it.
