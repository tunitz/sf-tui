# sftui

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A single-binary terminal UI for the Salesforce CLI. `sftui` wraps the `sf` executable already
installed on your machine: it does not store credentials, and all authentication stays with `sf`
in its own home directory.

The interface opens on a **Menu**: Retrieve Metadata, Deploy Metadata, Create Metadata, Apex
Tests, Organization and Run command. Each entry opens a focused screen, with project paths and the
default org pre-filled so you do not retype them. `Esc` returns to the previous screen and `h`
returns to the Menu; there is no tab bar.

- **focused screens** for retrieve, deploy, Apex tests and org operations;
- one **Jobs window** (`F2`) for every running, queued and finished command, available from any
  screen;
- **Run command** for the occasional `sf` invocation that has no dedicated screen.

Startup is non-blocking: the UI appears immediately with a visible progress line while `sf` is
probed in the background. Nothing else runs at launch, so your first retrieve or deploy does not
queue behind a background refresh.

## Install

No administrator rights or sudo are required. The installers put the binary in a user-writable
directory and register it in the user-level PATH.

**Linux / macOS**

```sh
curl -fsSL https://raw.githubusercontent.com/tunitz/sf-tui/main/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/tunitz/sf-tui/main/install.ps1 | iex
```

Open a new terminal and type `sftui`. Manual installation: download the archive for your target
from [Releases](https://github.com/tunitz/sf-tui/releases), extract the single executable and put
it anywhere on your PATH. An optional version argument selects a specific release, e.g.
`curl -fsSL .../install.sh | sh -s 0.1.0`.

You can override the install directory on Unix with `SFTUI_INSTALL_DIR`.

## Update

```sh
sftui update           # install the newest release
sftui update --check   # report whether a newer release exists
sftui update --force   # reinstall the newest release even if versions match
```

The updater reads the latest GitHub release, downloads the archive for the current platform,
verifies `SHA256SUMS`, and replaces the executable. If the install directory is not writable,
re-run the installer instead.

## Requirements

Install the [Salesforce CLI](https://developer.salesforce.com/tools/salesforcecli) first. If `sf`
is not found, `sftui` opens a Setup screen with per-platform, no-admin install commands, an
installer runner and a re-probe key. The deprecated `sfdx` CLI is not used or translated: install
`sf` instead.

For a nonstandard location, set `SFTUI_SF_PATH` to the full path of the executable.

## Keybindings

All key hints live on the footer line; screens and the header carry context only.

The menu is the whole navigation:

```text
[r] Retrieve Metadata   [d] Deploy Metadata   [c] Create Metadata
[t] Apex Tests          [o] Organization      [e] Run command
```

Global: `h` Menu · `Esc` previous screen · `?` help · `y` copy · `F5` refresh ·
`F2` Jobs · `F4` switch the default org · `q` quit (asks while a job runs).
`Ctrl+X` cancels the newest job; `Ctrl+C` cancels it or quits when idle.
`Ctrl+Y` copies the current selection while a search field has focus.

Retrieve, deploy and test runs execute in the background. Up to four commands run in parallel and
anything beyond that waits in a queue, starting automatically as slots free up. Leaving a screen
never stops a job: start a retrieve, go back, start a deploy and a test run, and keep browsing.
Finishing jobs are announced in the status line instead of stealing the screen.

`F2` opens the **Jobs** window from any screen. It lists every job in three sections — running,
queued and finished — with its task, command, elapsed time and outcome. The selected job's live
output, or its full result when finished, is shown beside the list and follows new output until you
scroll up (`End` resumes). `Enter` opens the selected job and focuses its result, `Tab` moves focus
between the list and the result, `x` cancels a running job or removes a queued one, `c` clears the
finished list and `Esc` steps back.

Apex Tests follows the same pattern: type to find real test classes (`*Test.cls` or `@IsTest`),
`Space` check/uncheck, `Enter` confirm. A run returns a test run ID immediately and sftui polls the
result, so a long org run never holds a job slot. Runs use `--code-coverage`, so results show one
row per test class with `[✓]`/`[✗]`, the method outcomes underneath, and a coverage table with
every class the CLI returned: percent, covered/total lines and a bar. The table is on the Tests
screen (`Tab` focuses it and `↑↓` scrolls) and in the full result view. Per-class rows come from
the org; to include classes the run did not touch, turn off *Store Only Aggregate Code Coverage* in
Setup › Apex Test Execution. The Organization screen lists orgs and their detail, and handles login, logout and the default
org. `F4` opens the same choice from any screen: pick an org, Enter sets it as `target-org` for
the project and the header updates immediately.

On the Retrieve/Deploy/Test screens, type the query, press `Enter` to search, `↑↓` move,
`Space` check/uncheck, then `Enter` again to confirm. Search runs only on `Enter`; typing keeps
your existing selection, and returning to the screen keeps it too. `Esc` goes back.

The confirmation dialog shows every selected component as a checkbox: `Space` toggles, `↑↓` moves,
`c` shows the exact command, `v/t/w` toggle deploy options, `g` cycles orgs, `Enter` runs and `Esc`
goes back.

Every command result is rendered human-readable immediately: status, counts, one `[✓]`/`[✗]` row
per requested metadata item, color-coded metric chips, progress bars and section headers. The
latest finished result also stays visible at the bottom of its own screen (`…` means still
running, `✓` succeeded, `✗` failed). Long results scroll with `↑↓`/`PgUp`/`PgDn`; raw stdout/JSON
is behind `o`/`z`. Search results rank concrete components above bare metadata types, and the
confirmation dialog always shows the exact `--metadata` targets, because a bare type retrieves
every component of that kind.

The interface uses terminal default colors for body text and reverse video for selection, and
honors `NO_COLOR` by stripping color while keeping bold/reverse emphasis.

## Configuration

Configuration lives in a per-user directory chosen by the `directories` crate:

- Linux: `~/.config/sf-tui/config.toml`
- macOS: `~/Library/Application Support/sf-tui/config.toml`
- Windows: `%APPDATA%\sf-tui\config\config.toml`

Set `SFTUI_CONFIG_DIR` to use a different directory. The TOML file contains `theme.accent`
(`cyan`, `green`, `yellow`, `blue`, `magenta`, `red`) and is written atomically with owner-only
permissions on Unix.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release --locked
cargo run --bin sftui
```

The test suite uses a synthetic CLI stub and never contacts an org. See [AGENTS.md](AGENTS.md) for
layout and conventions, and `.agents/skills/` for focused task notes.

## License

MIT © 2026 tunitz. See [LICENSE](LICENSE) and [CHANGELOG.md](CHANGELOG.md).
