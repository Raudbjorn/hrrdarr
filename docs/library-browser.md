# Release search, grab, RSS and completed-import browser checks

Run the existing owned fixture and browser command described in
[library-ui.md](library-ui.md#runnable-isolated-browser-verification).
The fixture now mounts the native release search, policy and quality-profile routes.
Its synthetic indexer returns scoped releases with private magnet locators. Its owned qBittorrent mock records actual add requests and supplies completed file/details observations.
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
their current revisions. Schedule writes include the observed revision. “Find releases” remains read-only. “Search and download” retains offers and uses explicit automatic or interactive selection. Needs-attention receipts offer no redispatch action.

The completed-download scenario additionally exercises actual observed RSS receipts, explicit processing policies, path mapping and imports: a new TV episode via hardlink and replacement of the earlier manual movie fixture via copy. A failed history commit retains the movie original; explicit resume completes the same operation and preserves original bytes in its recorded recovery artifact. Policy CAS, missing-mapping feedback, unknown accepted responses and mobile layout are checked without automatic write replay. Exactly two qBittorrent add requests remain after that retry/recovery scenario.

A separate targeted-search scenario adds distinct series and movies (catalogue IDs 202/203) through the browser and assigns native quality profiles. Both domains exercise automatic ranking and explicit selection of a lower-ranked accepted offer. Rejected wrong-category offers remain visible. Interactive listing sends no download; lost accepted command/grab responses require status readback and do not replay POSTs. Reload retains command/receipt history, and mobile layout is checked. Four additional actual client submissions use independent hashes, leaving the completed-import scenario unchanged. These targets have future availability dates, so successful user-invoked handoff also exercises the availability exception.

## Not claimed

Browser checks do not establish uncertain qBittorrent submission recovery, scheduling across process restarts, or the full Slice 2/4 gate. Backend submission/recovery evidence is separate. Full parser, profile,
provider/settings UI and manual grab parity remain incomplete. The external installed
Playwright/Chromium is not a pinned project dependency. No new frontend dependency,
cache framework or persistent browser state was added for these panels.
