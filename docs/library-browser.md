# Release search and RSS browser checks

Run the existing owned fixture and browser command described in
[library-ui.md](library-ui.md#runnable-isolated-browser-verification).
The fixture now mounts the native release search, policy and quality-profile routes.
Its synthetic indexer returns one scoped release with a private magnet locator.
No public indexer, catalogue, download client or real media is contacted.

The added scenario creates quality profiles through the native API, assigns them
through the library editor in both domains and verifies persisted assignments after
reload. It searches a selected episode and movie through the actual indexer transport,
checks typed release decisions, and saves/reads domain delay policies. RSS tests select
an explicit indexer and client, save disabled schedules, run both domains and inspect
rejected receipts. A lost response after a real accepted RSS request locks further
mutations until explicit status readback; the browser issues only one POST.

The UI polls persisted commands and candidate receipts every five seconds while
visible. Domain changes and unmount discard stale reads. Provider reload clears the
release-search selection; RSS revalidates retained selections by UUID before using
their current revisions. Schedule writes include the observed revision. Search is
read-only and has no grab action. Needs-attention receipts offer no redispatch action.

## Not claimed

Browser checks of rejected RSS receipts do not establish successful submission,
uncertain qBittorrent recovery, scheduling across process restarts, or completed
imports. Backend submission/recovery evidence is separate; the complete automatic
download-to-import pipeline remains incomplete. Full parser, profile,
provider/settings UI and manual grab parity remain incomplete. The external installed
Playwright/Chromium is not a pinned project dependency. No new frontend dependency,
cache framework or persistent browser state was added for these panels.
