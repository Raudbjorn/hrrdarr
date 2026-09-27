# Proper and repack preference settings

`GET /api/v1/{tv|movies}/revision-policy` returns `{media_type, mode, revision}`. `PUT` accepts `{mode, revision}` with the current revision. Modes are `prefer_and_upgrade`, `do_not_upgrade`, and `do_not_prefer`; each domain initially uses `prefer_and_upgrade`. Unknown fields, null modes and unknown queries are rejected. Requests are bounded to4KiB and5seconds; revisions are positive JavaScript-safe integers. Stale or exhausted revisions return409. Storage failures return503 with static errors.

A successful save records local intent even if the mode is unchanged. Changing mode atomically wakes same-domain pending candidates that retain their private payload and active parent RSS commands. Other domains and prepared/submitting/reconciling ownership stay unchanged. Waking is not submission authorization. The revision pipeline must integrate this policy reader into its pre-submission and precommit reevaluations before revision support is enabled; that consumer integration is not delivered here. Existing unsupported guards remain. Settings writers share an immediate transaction; future consumers must read current policy inside their committing transaction rather than trust a prewarm-time setting.

Migration38 adds only policy configuration and a snapshot activation marker. Existing file metadata and immutable import receipt revisions are untouched. Its RSS transition adjustment permits a queued/retry-wait parent deadline to decrease to0 with every other column unchanged; existing claim, attempts and ownership guards remain.

Snapshots recognize `Config.DownloadPropersAndRepacks` in supported Sonarr233 and Radarr206/242 backups. Names are case-insensitive; declared numeric values0/1/2 map to the same three modes. An absent key in a valid Config table uses the source default. Missing Config is reported unavailable; null/duplicate malformed input fails, unknown values stay archived and inactive. Unrelated Config settings remain private raw archives.

Exact policy equality is an idempotent duplicate. A differing policy activates only into an untouched seed with no prior activated same-domain source policy. Native edits—including same-value saves—and earlier snapshot activation prevent overwrite. Explicit repeat upload can activate previously archived supported settings; merely opening the database cannot. Dry runs, conflicts and failures roll back configuration, scheduling and provenance together.

Focused evidence lives in `tests/revision_policy.rs` and `src/db/revision_policy_tests.rs`: both-domain HTTP/CAS, transactional wake failure, protected prepared work, source versions and malformed shapes, replay/local intent/earlier-source fences, dry-run and late rollback, predecessor migration and unchanged factual revision bytes.

## Not claimed

This unit supplies settings and snapshot preservation. Existing unsupported proper/repack guards remain until the complete parser/comparison/import pipeline is integrated. The browser control below covers this singleton only. There is no new dependency, no V3 wire-compatibility claim and no full delay-profile or parity-gate completion claim. Tests use isolated scratch databases and synthetic endpoints; they do not contact providers or access real media.

## Native browser control

Media management → Propers and repacks provides the three domain-scoped modes:
Prefer and upgrade, Prefer without automatic upgrades, and Do not prefer. The
middle mode is not described as a blanket ban on every proper release. The form
uses the saved revision for PUT, including same-value saves; stale conflicts and
unknown outcomes preserve the draft and block blind retry until explicit reload.
Its mounted state survives workspace navigation. This independent singleton does
not share a UI mutex with unrelated quality/delay fields.

`frontend/tests/revision-policy-browser.mjs` exercises both domains, every mode,
same-value revision increments, concurrent-writer conflict, and a held committed
write whose response is lost. The control does not establish runtime consumer
parity by itself or complete Media Management's other missing settings. A full
browser reload still discards local transient state.
