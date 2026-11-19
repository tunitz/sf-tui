# Changelog

All notable changes to this project are documented here. Versions stay in `0.1.x` during
initial development, from `0.1.0` through `0.1.99`. The `v0.2.x` releases were published in error
and are superseded by `v0.1.10`.

## [0.1.25] - 2026-11-20

### Changed

- The Metadata screen is now mode-neutral: search and select components first, then choose Retrieve or Deploy in the confirmation dialog with `[r]`/`[d]`. The same selection can be used for either action without re-searching, and components can still be unchecked in the dialog.
- Org-only components discovered with `F6` are Retrieve-only; selecting Deploy for them is refused with a clear message.

## [0.1.24] - 2026-11-20

### Added

- Genuinely async live fuzzy search with debounced background ranking for Metadata, Apex Tests and the Create Metadata type picker; typing stays responsive while results re-rank off-thread.
- Org-only metadata browsing via `sf org list metadata --metadata-type ... --target-org ... --json`, plus exact-name retrieval of a single component.
- A retry report in the Jobs window, and rerun of a finished job with its original test classes, options and target org.

### Changed

- Retrieve and Deploy are combined into one Metadata screen.
- Retrieving now refreshes the project so the locally retrieved components are picked up immediately.
- Jobs redesigned around summary-first detail, with clear stop/cancel semantics, deploy cancellation and Apex stop tracking.
- Create Metadata wizard redesigned with per-type fields, staged safe writes and explicit messaging that files are created locally only.
- Updated documentation and key hints to match the new screens and controls.

### Fixed

- Apex results when one of several test classes fails: exit-code-100 reports with valid results are now read correctly, and all class and method results are preserved instead of being dropped.

## [0.1.23] - 2026-11-18

### Changed

- A waiting Apex test run shows an amber `◌` instead of a success check, in both the Jobs list and the detail pane.
- The command catalog moved from `F3` to **`F7`**, and Apex test history from `F6` to **`F8`**, to avoid conflicts with VS Code shortcuts. Plain letters and digits reach the search fields without being intercepted.
- The Tests screen no longer shows a coverage pane at the bottom; its body is dedicated to the test-class list. The Apex tests header shows `(1 test class running)` (pluralized) while runs are active and drops the count when they finish.

### Fixed

- The Jobs detail pane now refreshes when an Apex run finishes: the render cache is invalidated after the final result is written, and the selection follows the record as it moves from RUNNING to FINISHED. Timeout and failure always end in a real result instead of leaving a "waiting for org results" placeholder.

## [0.1.22] - 2026-11-17

### Added

- **Persistent state**, stored per project under the config directory and written atomically with owner-only permissions. The org list and org details load at startup; F5 refreshes them. A cached default org is used immediately instead of querying `sf config get target-org`.
- A global cached Salesforce CLI version. With both caches warm, sftui makes **no `sf` calls at startup**; F5 or Setup `r` forces a fresh probe.
- Apex **test run history**: full results from sftui runs are saved (newest 50). On the Tests screen, `F6` opens a popup with a run list and the full result; `d` deletes a run and `c` twice clears all. The hint is shown only in the Tests footer.
- F4's org switcher now has a `d` action to fetch and cache an org's details, with a small detail pane.

### Changed

- Header status no longer claims sf is being probed when both caches are warm.

## [0.1.21] - 2026-11-17

### Added

- A searchable **command catalog** on `F3`, from any screen. It lists commands with a detail pane (summary, flags and examples); Enter shows the exact command before running it. The catalog is a bundled snapshot, so no `sf commands --json` call happens at startup.
- Create metadata now generates files into the correct package directory and shows the exact command for confirmation before running.

### Changed

- Menu shortcuts are numbers: `1` Retrieve, `2` Deploy, `3` Create, `4` Tests. The Organization and Run command menu items were removed; org switching stays on `F4`.
- Headers are left-aligned; on short terminals the status appears at the left of the header so the footer can keep its key hints.
- The `?` help is now a two-column table that stays in sync with the footer from one keymap.

### Fixed

- Org list, detail and action jobs now appear in the F2 Jobs window with their results, and no longer consume a user job slot.
- Catalog selection no longer builds a command from legacy `sfdx force:` examples or unfilled `<placeholder>` tokens.
- Removed the "LATEST RESULT" panel; results are shown only in the F2 Jobs window.

## [0.1.20] - 2026-11-17

### Changed

- Retrieve now runs `sf project retrieve start --ignore-conflicts` without `--json`, matching a direct CLI call and avoiding the extra time the structured-output mode takes. Results render from the CLI's human-readable output.
- Deploy now runs `sf project deploy start --ignore-conflicts` without `--json`; the deploy id is parsed from human output, and the follow-up `sf project deploy report` still uses `--json` for structured status.
- In a short terminal, the single footer row always shows key hints; transient status moves to the header.
- Background `sf` probes are aborted and joined on exit, so no CLI process is left running when sftui quits, errors out or is shut down.

### Fixed

- Removed the stale comment claiming retrieve runs with `--wait`.

## [0.1.19] - 2026-11-17

### Added

- Rendered result and job-detail lines are memoized with a bounded cache, and component-lookup and suggestion-filtering use indexes, so large outputs and large projects stay responsive.
- Visible notices when streamed output or JSON rows are dropped by the retention caps.

### Changed

- The UI event channel is bounded and streamed CLI lines are batched at the sender, replacing unbounded per-line events with backpressure.
- Footer keycaps and the `?` help now come from one keymap table; `Enter` shows a single, state-aware hint (`search` or `confirm`).
- Notices and errors expire after five seconds, so key hints return in narrow terminals.
- Truncation widths now follow the terminal width. Chip colors come from structured result state rather than label text, and `red`/`yellow` are no longer selectable accents (they fall back to cyan).
- The Tests screen guarantees the test list a usable minimum and only shows coverage when there is room.

### Fixed

- Shutdown closes the event channel before joining jobs, preventing a full channel from deadlocking exit.
- `active_job_label` prefers a running user job instead of an arbitrary highest-id job.

## [0.1.18] - 2026-11-17

### Added

- `F4` switches the default org from any screen: a small picker marks the current default, Enter
  sets `target-org` for the project, F5 refreshes the list and `l` starts a login. The header
  updates immediately.

### Changed

- Key hints now live only in the footer (and inside dialogs). Screen headings, the Orgs detail
  pane, the coverage pane title and the Home body no longer repeat command lists; navigation no
  longer writes hints into the status line.
- The footer carries up to seven keys where they fit, ordered by what matters on that screen;
  narrower terminals drop the least important ones instead of wrapping.

## [0.1.17] - 2026-11-17

### Fixed

- Per-class Apex coverage now parses the CLI's real result shape: the class table under
  `result.coverage.coverage` (name, totalLines, totalCovered, coveredPercent) and the per-method
  records that carry covered/uncovered line numbers. Previously only a flat `codecoverage` array
  was read, so a successful run showed the coverage percentage with no class rows.
- A terminal test summary that arrives before the org has aggregated coverage is polled up to
  three more times instead of finalizing with an empty table; if rows still never arrive, the
  result says so instead of silently omitting them.
- The header no longer shows `sf detecting…` after startup, or the doubled `org no default org`.
  The CLI version moved out of the header, the configured org is resolved with longer Windows
  timeouts (20 s version, 15 s org), and a missing org reads as `no default org` once.
- Screen titles no longer repeat the project and org that the header already shows.

### Changed

- The Jobs window shows counts in its title (`Jobs — 1 running · 2 finished`), marks the focused
  pane with an accent border, and uses one keycap bar for the whole window. `Enter` opens the
  selected job (`Tab` only moves focus), and the hint text now says so.
- On terminals narrower than 100 columns the job list takes only the height it needs, leaving the
  result pane as much room as possible.

## [0.1.16] - 2026-11-17

### Added

- A single **Jobs** window on `F2`, available from every screen: running, queued and finished jobs
  in one list with task, command, elapsed time and outcome. The selected job's live output follows
  new lines until you scroll up, finished jobs show their full result, `x` cancels a running job or
  removes a queued one, and `c` clears the finished list.
- **Run command** on the Menu for an `sf` invocation that has no dedicated screen, with the exact
  argv shown before it runs and secrets redacted from history and output.
- Per-class **coverage tables** on the Tests screen (`Tab` to focus and scroll) and in the full
  result view: one row per class with percent, covered/total lines and a bar.
- Jobs show how long they actually took, so a slow command can be compared with the same command
  in a terminal.

### Changed

- Apex test runs now start immediately and poll `sf apex get test --test-run-id`, instead of
  blocking on `--wait`. Besides freeing the job slot at once, this avoids the CLI behaviour where
  `--code-coverage` with `--wait` can return partial or zero coverage.
- Metadata search ranks concrete components above bare metadata types; typing the full type name
  still offers the whole type first. The confirmation dialog always shows the exact `--metadata`
  targets, because a bare type retrieves every component of that kind.
- The footer shows a few colour-coded keycaps for the current screen; the full map stays on `?`.
- Startup probes only the CLI version and the configured target org, so the first command no
  longer waits behind a catalog refresh or an org list.

### Removed

- The Command Catalog screen and its startup `sf commands --json` refresh, plus the
  `catalog-cache.json` file.
- Favorites and preset flags, which existed only for the catalog.
- The per-task history modals and the persisted `test-runs.json` history. Jobs are session-only
  for running/queued/finished output, while Apex results remain readable in the result view.

## [0.1.15] - 2026-11-17

### Added

- Retrieve, deploy and Apex test runs now execute in parallel: up to four commands run at once
  and further requests wait in a queue that starts them automatically. Each screen keeps its own
  job history on `F2`, with `Enter` for the full result and `x` to cancel a running or queued job.
  Finishing jobs are announced in the status line and never change the screen you are on.
- Every retrieve/deploy result lists each requested metadata item with a `[✓]`/`[✗]` row, even
  when the CLI reports no component-level status, so the requested set is always visible.
- Apex test runs always pass `--code-coverage`, and results group into one row per test class with
  its method outcomes and per-class coverage underneath.
- The latest finished result for a task also stays on that task's own screen.

### Changed

- The tab bar is gone. The Home screen is a plain Menu (Retrieve Metadata, Deploy Metadata,
  Create Metadata, Apex Tests, Organization, Command Catalog); `h` opens the Menu, `Esc` returns
  to the previous screen, and the `Tab`/`1-9` shortcuts were removed.
- The header shows the resolved default org (alias, else username) instead of the words
  "default org"; the default org is resolved as soon as `sf` is detected.
- The Home screen no longer lists recent commands; history lives with the task that produced it.
- Typing is responsive: terminal input is read on a dedicated thread so keystrokes no longer wait
  for the animation tick, and the component index is built off-thread instead of on the UI thread.
- The deprecated `sfdx` CLI is no longer detected or translated; only `sf` is used.
- Corrected the CLI environment variable to `SF_DISABLE_AUTOUPDATE`.
- Result screens no longer advertise keys that are not implemented.

## [0.1.14] - 2026-11-17

### Added

- `sftui update` checks GitHub for the newest release and installs it in place, with
  `--check` to report only and `--force` to reinstall the latest version.
- The updater downloads the platform archive, verifies the published `SHA256SUMS`, extracts the
  binary and replaces it atomically on Unix. On Windows it stages a small helper that waits for
  the running process to exit before replacing the executable.
- The `--help` text now lists the update command.

## [0.1.13] - 2026-11-17

### Added

- The Output screen keeps a selectable record rail (or a record counter in narrow layouts) and
  scrolls long results with `↑↓`, `PgUp`/`PgDn`, `Home`/`End`; raw output and JSON are modes.
- A dedicated saved-test-run view, reachable with `v`, with scrolling instead of a six-row cap.
- Home now surfaces `recent_runs`, project/org context and the actions it advertises
  (`c` create, `a` login, `g` results).
- Configuration, cache and test-history writes are atomic and owner-only on Unix.
- Release archives now ship `SHA256SUMS`, verified by both installers when present.

### Changed

- The header and footer use the full frame density: a two-row header with a real tab bar and a
  two-row footer that always shows key hints. Large terminals no longer waste rows on borders.
- `Esc` always navigates back; `Ctrl+X` cancels the newest running job. `q` asks before quitting
  while a job is running, and shutdown cancels and joins tracked jobs.
- Search results are scoped per screen kind, typing no longer clears the selected components,
  and queries are compared after trimming.
- Project discovery runs in the background; duplicate startup indexing was removed, test-class
  discovery is byte-bounded, and process events are drained before each frame.
- Result views unwrap the `result` envelope, recursively redact sensitive fields, and no longer
  keep a second full stdout copy per record.
- Orgs loads on entry, clears its loading state on failure, and refreshes the target after a
  switch; the org detail pane follows the selected row.
- Colors use terminal defaults instead of bright white, selection uses reverse video, failure
  icons are `✗` while checked rows use `✓`, and `NO_COLOR` strips color while keeping emphasis.
- Documentation and the keymap modal reflect the actual controls.

### Fixed

- UTF-8 prompt editing no longer panics on non-ASCII characters.
- A search reply for an edited query can no longer lock a screen on "Search still running".
- Deploy reports include `--target-org`, stop after repeated failures, and are not respawned by
  navigation or cancellation.
- JSON extraction tolerates bracketed warning lines before the document.
- Non-ASCII command categories no longer panic.
- Test-confirm `g` cycles orgs instead of always resetting to the default.
- Validation errors keep the prompt open instead of discarding typed input.
- Catalog rows are cached until the query, catalog or favorites change.
- Auth/secret flags (`--sfdx-url`, `--access-token`, `--password`, `--client-secret`) are masked
  before display, copy and history; auth-command output is not retained.

## [0.1.12] - 2026-11-17

### Changed

- `Esc` now cancels the active job or goes back to the previous screen. `Backspace` is no longer
  global navigation.
- Result views are more visual: banner, colored metric chips, `[✓]`/`[x]` rows and section headers.
- Apex coverage rows now show progress bars and full numbers: percent, covered/uncovered lines,
  and total lines, including nested `coverage.coveredLines` shapes.

[0.1.12]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.12

## [0.1.11] - 2026-11-17

### Changed

- Component/test search no longer runs while typing. Type the query, press `Enter` to search,
  `Space` to select, then `Enter` again to confirm. Filtering runs in `spawn_blocking` with a
  generation guard so stale results are ignored.
- Every command now gets a human-readable result view with status, counts, `[✓]`/`[x]` rows and
  sections. Raw stdout/JSON remains behind `o`.

[0.1.11]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.11

## [0.1.10] - 2026-11-17

### Changed

- Restored the `0.1.x` version line. This release contains the same application as `v0.2.2`.
- All future releases increment only the patch number until `0.1.99`.

[0.1.10]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.10

## [0.2.2] - 2026-11-17

### Changed

- Output now shows a human-readable result immediately for retrieve, deploy and Apex tests.
  Raw stdout/JSON is behind `o`.
- Results are rendered as separate rows with `[✓]` / `[x]` icons instead of one compressed line.
- Apex test output lists every test method with pass/fail state, then per-class coverage, including
  nested `perClassCoverage` and `codecoverage` shapes from Salesforce.

[0.2.2]: https://github.com/tunitz/sf-tui/releases/tag/v0.2.2

## [0.2.1] - 2026-11-17

### Fixed

- Removed the automatic catalog probe on startup; no more "catalog refresh timeout" message.
  The bundled snapshot is instant, and `F5` performs an explicit refresh that now updates the cache.
- Blank org now displays as `default org` instead of an empty label.
- Apex test results open expanded as soon as the run finishes.
- Corrected Apex coverage parsing to Salesforce's actual `result.codecoverage` and
  `summary.testRunCoverage` shape.
- Test history and result details are formatted as multi-line sections, not one compressed row.

[0.2.1]: https://github.com/tunitz/sf-tui/releases/tag/v0.2.1

## [0.2.0] - 2026-11-17

### Added

- Apex Tests screen searches only real test classes (`*Test.cls` or `@IsTest`) with checkbox
  multi-select and a confirmation dialog.
- Apex test runs are saved locally (`test-runs.json`, last 50 runs) and viewable with `v` on the
  result screen.
- Apex test results are formatted with highlights first — outcome, failures, coverage — followed
  by per-class coverage.
- Org menu on Home: check current org, switch org, login, delete/logout, reauth.

### Removed

- Get test results shortcut; past runs are now available from Apex Tests results via `v`.

[0.2.0]: https://github.com/tunitz/sf-tui/releases/tag/v0.2.0

## [0.1.9] - 2026-11-17

### Fixed

- Catalog cache is now written from the extracted JSON document, so warning text before
  `sf commands --json` no longer prevents caching.
- Added a regression test proving the second launch reuses the cache instead of showing
  "Loading command catalog".

[0.1.9]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.9

## [0.1.8] - 2026-11-17

### Added

- Test runs now use the same async search screen and checkbox multi-select as retrieve/deploy,
  with a confirmation dialog showing the exact `sf apex run test --class-names …` command.

### Fixed

- Removed the stray `[` character from the component search input.

[0.1.8]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.8

## [0.1.7] - 2026-11-17

### Changed

- Redesigned Retrieve and Deploy as separate, identical, plain search screens with an async,
  debounced component search and checkbox multi-select.
- Selection now goes straight to a confirmation dialog with checkbox toggles, advanced options,
  org cycling, command preview and one-key run — no intermediate path picker.
- Home is reduced to the menu plus the current org.
- Output is a result screen (`SUCCESS`/`FAILED`) with details collapsed behind `o`; it stays open
  until you press a key.
- Fixed input routing: search and modal screens consume all keys, so digits, arrows and hotkeys no
  longer trigger global actions while typing or confirming.

[0.1.7]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.7

## [0.1.6] - 2026-11-17

### Added

- Multi-select checkboxes in the component search: `Space` checks/unchecks, `Enter` confirms.
- Confirmation dialog after selecting components, showing the exact command with one key to run or
  cancel; the intermediate Deploy/Retrieve path picker is no longer part of the quick flow.
- Project suggestion index is cached per project and reused across prompts.

### Changed

- Search uses a fast substring pass over precomputed lowercase keys and only falls back to fuzzy
  matching for remaining slots; typing and pasting stay responsive on large projects.
- Debounce remains 90 ms for component and catalog search.

[0.1.6]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.6

## [0.1.5] - 2026-11-17

### Fixed

- Apex classes are indexed by file extension and the project scan now covers up to 50,000 files
  at greater depth, so long names such as `C3S_TRG26_FeedBackLoopHandler` are found.
- Search input now debounces fuzzy work by 90 ms for both component suggestions and the catalog;
  pasted text no longer triggers a full re-rank per character.
- Suggestion ranking precomputes lowercase keys and uses an unstable sort for large projects.

[0.1.5]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.5

## [0.1.4] - 2026-11-17

### Fixed

- Apex classes are suggested by name even when they live outside declared package directories;
  type detection now also uses file extensions such as `.cls` and `.trigger`.
- The current Salesforce org is probed at startup and shown in the header, Home context and the
  Org field of retrieve/deploy prompts.
- Suggestion source paths are muted gray text with no background highlight.

[0.1.4]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.4

## [0.1.3] - 2026-11-16

### Added

- `h` returns Home and `Backspace` returns to the previous screen from anywhere; successful
  commands show those keys in the status bar.
- Fuzzy suggestions now show the source path beneath each item in muted color, so matching an Apex
  class by name shows `force-app/.../classes/Name.cls` with it.
- The chosen component is preselected on the transfer screen; its heading reads
  `Selected: ApexClass:Name — Enter runs` instead of a generic picker label.

### Changed

- Removed the “Most used …” and other explanatory labels; prompts and headers now use short names
  such as `Retrieve metadata`, `Component` and `Org`.
- Component ranking prefers Apex classes and other real project components over generic metadata
  types when a name is typed.
- Suggestion results and catalog fuzzy matching are cached/reuse a single matcher, and the render
  ticker was reduced from 100 ms to 250 ms, fixing paste lag in the search input.

[0.1.3]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.3

## [0.1.1] - 2026-11-16

### Added

- Home screen reordered around real usage: retrieve and deploy first, Apex test run/results second,
  metadata creation third, org login last.
- Fuzzy component autocomplete for retrieve/deploy: type part of a component name and pick from
  matches built from local source files, `package.xml` manifests and common metadata types.
- Create metadata shortcut with fuzzy template selection (Apex class/trigger, LWC, FlexiPage,
  CustomObject, custom metadata type and more), followed by a name prompt.
- Responsive layout for VS Code/Zed terminal panels (compact below 105x30, single-column below
  72x20; verified down to 48x12).

### Changed

- Startup now draws the UI immediately and shows staged progress while `sf` is probed with short
  timeouts; successful catalog refreshes are cached for seven days.
- Deploy/retrieve flags now match the current CLI (`--source-dir`, `--dry-run`, repeated
  `--metadata`; retrieve no longer calls a non-existent `project retrieve report`).
- Telemetry and update checks are disabled in child `sf` processes to reduce startup latency.

[0.1.1]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.1

## [0.1.0] - 2026-11-16

### Added

- First release of the `sftui` single binary for Windows, Linux and macOS.
- CLI detection across PATH, user install directories and legacy `sfdx`, with `SFTUI_SF_PATH`
  override and a Setup screen when the CLI is missing.
- Fuzzy-searchable command catalog with the full CLI snapshot, live `sf commands --json`
  refresh, per-command `--help`, favorites, preset flags, copy and run with streamed output.
- Output pane with command history, live stdout/stderr, parsed JSON tables, scrollback and
  clipboard copy.
- Org screen with list/display, browser open, default org/dev hub, logout confirmation, web login
  and scratch org creation.
- Deploy/Retrieve screen with project discovery, metadata picker, validation/test flags, job
  polling, component results and retrieve equivalents.
- User-level installers for Unix and Windows without sudo or elevation.
- CI on three operating systems and a tag-based release workflow publishing five binaries.

[0.1.0]: https://github.com/tunitz/sf-tui/releases/tag/v0.1.0
