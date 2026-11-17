# Changelog

All notable changes to this project are documented here. Versions follow `0.x.y` semantics:
`0.x.y` for fixes, `0.x.0` for features and `1.0.0` only when stability is promised.

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
