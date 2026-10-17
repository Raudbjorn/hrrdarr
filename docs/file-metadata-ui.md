# File metadata editing

The library Files and metadata panel consumes the existing [file metadata API](file-api.md) for TV series and movies. It lists 25 files at a time, displays stored file facts and media information, and supports editing one file or applying a common patch to selected files on the visible page. A TV file shared by episodes remains one file record. The current schema permits one file per movie, so a movie detail selection contains at most one file; selecting files across different movies remains a separate workflow.

Editable fields are quality with its revision, concrete languages, release group and indexer flags, plus TV release type or movie edition. Each field starts at **Keep unchanged**. **Clear to unknown** sends null; **Set value** sends the selected value. Empty language arrays, Unknown language 0, empty strings and zero flags are different from null. A quality replacement includes an explicit unknown revision or a complete version/real/repack revision; it never silently claims to preserve a nested revision omitted from the request. Revision version 0 is a known value, distinct from an unknown revision.

Quality choices come from the current domain quality definitions using `quality.id`. Language, flag and release-type choices reuse the current custom-format schema. Negative Original/Any language policy choices are excluded from concrete file languages. A catalog failure disables editing and remains visible. These metadata requests do not move, rename, delete or probe media files. Stored size and media information are observations, not current filesystem measurements. Unsupported scene-name editing and unavailable custom-format/score/cutoff projections are not offered as completed features.

Every save sends one explicit mutation and reads back every attempted file. A network error, malformed success or server failure retains the edit and locks mutation until explicit complete readback. A missing or differently owned record cannot satisfy that readback. After reconciliation the panel shows current values alongside the retained edit and requires finishing the review before another edit. It never automatically retries a write. A normal successful readback replaces displayed file facts and notifies the parent to invalidate dependent filename previews.

The API applies a submitted batch atomically but has no revision CAS. The panel sends only fields the user chose to change; another client can still overwrite the same field. Matching readback values establish current stored values, not which writer supplied them.

## Integration contract

`MediaFilesPanel.svelte` takes:

- `target: {domain: MediaDomain, id: number, title: string} | null`;
- `active`, `relatedWrite: SettingsWriteState`, and `discardVersion`;
- `onwrite(state)`, `ondirty(dirty)` and `onsaved({domain,id})` callbacks.

Mount one instance and retain it across workspace navigation. A null target during a library refresh does not discard the retained owner or draft. Before changing to another owner, the parent blocks busy/uncertain state and confirms discarding a dirty draft, then increments `discardVersion`. The panel also refuses an unapproved owner change defensively. Page changes and closing an edit have local discard confirmation. The beforeunload hook protects dirty/busy/uncertain state; forced browser termination does not guarantee draft recovery.

Parent library write locks include the panel's reported write state. Conversely `relatedWrite` must reflect **other** related writes, excluding this panel's own report to avoid a self-lock during recovery. Callback results and reads are fenced by owner and request generation. A same-owner parent refresh must not unmount the editor. Filename previews are hidden while a file write is busy or uncertain. A confirmed save/readback invalidates the current movie filename preview without inventing changes to file counts or paths.

## Validation boundary

Draft/catalog/readback helpers and transport contracts have separate focused tests. Required parent acceptance uses actual owned APIs and scratch files for both-domain single/bulk edits, null/empty distinctions, shared TV files and colliding IDs, rejected atomic batches, accepted-response loss with complete readback, owner/navigation fences, reload persistence, and unchanged filesystem bytes/identity/associations. Browser overlays alone cannot establish persistence or transaction rollback.

This document describes the implemented consumer contract. Runtime browser, generated-contract/Svelte verification and build results are recorded by the parent after integration; none are claimed from source review alone. Full file-controller parity, physical file operations, scene validation, external-writer conflict prevention and power-loss durability remain outside this delivery.
