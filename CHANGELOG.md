# Changelog

All notable changes to this project are documented here. Versions follow `0.x.y` semantics:
`0.x.y` for fixes, `0.x.0` for features and `1.0.0` only when stability is promised.

## Unreleased

### Added

- Home screen with five labelled shortcuts (org login, retrieve, deploy, Apex tests, test results)
  and pre-filled project paths/default org.
- Visible startup progress: the UI opens immediately, probes `sf` in the background with short
  timeouts, and caches the command catalog for seven days.
- Key labels and an Actions bar on every screen, plus a simplified one-line catalog.

### Fixed

- Deploy/retrieve flags now match the current CLI (`--source-dir`, `--dry-run`, repeated
  `--metadata`; retrieve no longer calls a non-existent `project retrieve report`).
- Disabled CLI telemetry and update checks in child processes to reduce startup latency.

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
