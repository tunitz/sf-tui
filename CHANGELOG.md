# Changelog

All notable changes to this project are documented here. Versions follow `0.x.y` semantics:
`0.x.y` for fixes, `0.x.0` for features and `1.0.0` only when stability is promised.

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
