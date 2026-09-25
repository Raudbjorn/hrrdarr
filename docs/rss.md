# Durable RSS and submission receipts

The native RSS consumer fetches Torznab/Newznab release facts, evaluates the current TV or movie library policy, persists delayed decisions, and submits accepted torrent releases through qBittorrent. It uses the existing single local command worker, provider transport limits and database ownership lock. A feed fetch alone is not a completed RSS pass.

`POST /api/v1/rss/commands` takes `{target:{media_type,indexer_id,indexer_revision,client_id,client_revision},priority}`. Both provider revisions and the domain are explicit. Commands share the global 1,024-row admission cap and priority ordering with download refresh, metadata refresh and blocklist clear. One active RSS command is admitted per provider pair/domain. `GET /rss/commands` supports the existing bounded command query; detail, cancellation and terminal-history deletion use `/rss/commands/{id}` and `/cancel`.

`GET /rss/candidates` returns paginated receipts with optional `media_type`, `command_id` and closed `status` filters (limit 1–100, offset at most 10,000). Responses contain safe titles, typed targets, decision reasons, timestamps and errors. Locators, GUIDs, attributes, credentials and prepared transport payloads are private. `DELETE /rss/candidates/{id}` removes only unclaimed rejected/cancelled receipts. Receipt deletion does not remove a remote download.

`GET/POST /rss/schedules` manages at most 64 persisted schedules. Intervals are 60–86,400 seconds. Creates omit `revision`; updates supply the exact returned revision. `DELETE /rss/schedules/{id}?revision=N` also fences concurrent changes. Provider edits disable stale schedules visibly; provider deletion removes its schedules but retains command and submission history. Missed intervals coalesce into one due run. Admission failures are recorded and the next due time advances.

All paths above have the `/api/v1` prefix. Request bodies are at most 8 KiB, responses at most 1 MiB, and API I/O has a five-second deadline. A timeout is an unknown request outcome: read back commands/schedules before deciding whether to issue another request.

## Execution and recovery

Each pass captures at most 1,000 releases across ten pages, with a 4 MiB aggregate private encoding limit and a 35-second capture deadline. Malformed-page/item warnings currently fail the pass visibly. The entire feed capture and decision checkpoint commits together; retries reuse that checkpoint. Rejected or cancelled, unclaimed releases may be evaluated again on a later feed pass as availability, monitoring or policy changes. Existing owned submissions are never replaced by that reevaluation.

Pending payloads use bounded AES-GCM envelopes with a separate purpose and candidate/provider/revision/domain associated data. A configured provider key is required; there is no plaintext fallback. Each envelope is at most 65,565 bytes, all envelopes together at most 16 MiB, and receipts at most 1,024 rows. Rejection/cancellation releases encrypted payload storage. Lost keys cause a visible failure, never a new submission.

A delayed candidate retains its typed target. When due, the evaluator checks current policy again. A changed target is rejected rather than silently retargeted. Immediately after read-only preparation, an immediate transaction rechecks current local policy, provider revisions and target/hash ownership. It records the immutable safe submission identity and claims every prepared torrent hash before transitioning to `submitting`. Only then may the network mutation run. No database transaction spans a network call.

The `submitting` boundary is irrevocable. A restart or uncertain result permits only read-only reconciliation, at most three observations. `NotObserved` never authorizes another POST. Presence discovered during reconciliation becomes `needs_attention/presence_unconfirmed`: it cannot establish that this process submitted the torrent or completed its configured options. An already-present torrent becomes `needs_attention/preexisting_download`, without a trusted association. Only a direct successful submitted-and-observed outcome becomes `observed`. The receipt and its typed target form the native grab history for this unit; this is not an import event.

Client/hash ownership and typed target ownership prevent another release from acquiring the same target, even through another client. Owned receipts and hash claims are retained. There is currently no import-driven retirement or operator resolution route, so reaching this finite capacity requires later resolution functionality rather than automatic deletion of uncertainty evidence.

Cancellation stops work before dispatch. It cannot undo a dispatched torrent; existing uncertain receipts continue read-only reconciliation. Local storage failures after dispatch return to recovery without resubmitting. Due delayed/reconciliation work joins the existing priority/creation ordering, so a stream of newer normal commands cannot indefinitely defer older receipts.

Command `succeeded` means the current feed/decision pass ended, **not** that downloads completed. Delayed candidates can later fail while command status remains terminal. Command progress counters read current receipt states; candidate receipts are authoritative for later outcomes. Uncertain immediate submissions fail their pass visibly. Queue association is populated only from an exact client UUID, revision and validated returned remote torrent ID with a trusted `observed` receipt. That remote ID can differ from the prepared v1/v2 hashes; it commits atomically with the observed status while all original hash ownership claims remain retained. Unknown, pre-existing and uncertain downloads remain unassociated.

## Scope and evidence limits

This consumer dispatches single-episode TV releases and movies. Packs are parsed but rejected explicitly before preparation; they are not reduced to an arbitrary episode. Usenet cannot be sent to qBittorrent. Unequal recent/older client priorities are explicitly unsupported until the domain-specific recent-release policy is implemented. The evaluator exposes other unsupported policies as reasons rather than accepting them silently.

Unified History grabbed events, completed-download import, failed-download handling, automatic removal, release breadth, additional clients and full upstream RSS parity remain incomplete. `observed` does not prove download completion, import success, filesystem layout or recovery of every follow-up option. No live service equivalence is claimed.

Runnable regressions are in [rss_commands.rs](../tests/rss_commands.rs); provider protocol coverage remains in [qbittorrent_protocol.rs](../tests/qbittorrent_protocol.rs). Fixtures use scratch databases and owned loopback servers.
