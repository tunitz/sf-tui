# sftui

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A single-binary terminal UI for the Salesforce CLI. `sftui` wraps the `sf` executable already
installed on your machine: it does not store credentials, and all authentication stays with `sf`
in its own home directory.

The interface opens on a **Menu**: Retrieve/Deploy Metadata, Create Metadata and Apex Tests.
Retrieve and Deploy share one Metadata screen — `F3` switches its mode — with project paths and the
default org pre-filled so you do not retype them. `Esc` returns to the previous screen and `h`
returns to the Menu; there is no tab bar.

- a **unified Metadata screen** for retrieve and deploy, with org browsing for components that are
  not local yet;
- **focused screens** for Apex tests and org operations;
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
[1] Retrieve/Deploy Metadata   [2] Create Metadata   [3] Apex Tests
```

Global: `h` Menu · `Esc` previous screen · `?` help · `y` copy · `F5` refresh ·
`F2` Jobs · `F4` switch the default org · `q` quit (asks while a job runs).
`Ctrl+X` cancels the newest job; `Ctrl+C` cancels it or quits when idle.
`Ctrl+Y` copies the current selection while a search field has focus.

Create Metadata opens a three-step wizard: choose a grouped metadata type (type to filter,
`↑↓` move, `Enter` selects the highlighted row), configure the per-type fields and pick a package
source root from `sfdx-project.json` (or an explicit project-relative path), then review the exact
argv, destination and expected paths. It only creates local files; it never deploys. Generators run
into a private staging directory and are published with exclusive creation, so an existing file is
never overwritten. Custom Object is interactive in the Salesforce CLI, so sftui offers a Copy CLI
command instead of running it. A failed job keeps its draft: press `e` in `F2` Jobs to edit it.

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

On the Metadata/Test screens, type the query, press `Enter` to search, `↑↓` move,
`Space` check/uncheck, then `Enter` again to confirm. Search runs only on `Enter`; typing keeps
your existing selection, and returning to the screen keeps it too. `Esc` goes back.

On the Metadata screen, `F3` switches between Retrieve and Deploy; checked items never carry over.
Components that exist only in an org can be pulled in first: press `F6` (Retrieve mode), pick an
org and one of ApexClass, ApexTrigger, Flow, PermissionSet, CustomObject or
LightningComponentBundle, then `Enter` to list it. `sftui` runs `sf org list metadata` in the
background and marks each row **Local** or **Org only**; `Space` checks exact components and
`F9` adds an exact `Type:FullName` when the name is not listed. Bare types, wildcards and blanks
are rejected. The retrieve uses the org's username and exact `--metadata Type:FullName` arguments,
and a successful retrieve rescans the project so new files appear in Deploy and Apex tests.
Listings are cached per org and type for the session; `F5` refreshes them, and a failed refresh
never replaces a good listing with an empty one.

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
