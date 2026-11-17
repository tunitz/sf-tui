# sftui

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A single-binary terminal UI for the Salesforce CLI. `sftui` wraps the `sf` executable already
installed on your machine: it does not store credentials, and all authentication stays with `sf`
in its own home directory.

The interface opens on a **Home** screen ordered by frequency: retrieve and deploy first, Apex
test run/results second, metadata creation third, and org login last (you only need it occasionally).
Behind it are:

- **focused screens** for org operations and metadata deploy/retrieve, with project paths and the
  default org pre-filled so you do not retype them;
- a **generic command catalog** covering the full `sf` CLI surface (fuzzy search, favorites,
  preset flags, exact invocation and streamed output).

Startup is non-blocking: the UI appears immediately with a visible progress line while `sf` is
probed in the background. The bundled catalog is usable during the probe, and a successful command
refresh is cached for seven days.

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

## Requirements

Install the [Salesforce CLI](https://developer.salesforce.com/tools/salesforcecli) first. If `sf`
is not found, `sftui` opens a Setup screen with per-platform, no-admin install commands, an
installer runner and a re-probe key. A legacy `sfdx` installation is detected and common commands
are translated automatically.

For a nonstandard location, set `SFTUI_SF_PATH` to the full path of the executable.

## Keybindings

Home is just the menu:

```text
[1] Retrieve        [2] Deploy        [3] Run tests
[4] Org
```

Global: `Tab` next screen · `h` Home · `Backspace` back · `?` help · `q` quit.

Apex Tests follows the same pattern: type to find real test classes (`*Test.cls` or `@IsTest`),
`Space` check/uncheck, `Enter` confirm. Results are saved locally (last 50 runs) and shown with
highlights first — outcome, failures, coverage — then per-class coverage. Press `v` on the result
screen to view previous runs. The Org menu replaces the old login shortcut: check current org,
switch, login, delete/logout or reauth.

On the Retrieve/Deploy/Test screens, type the query, press `Enter` to search, `↑↓` move,
`Space` check/uncheck, then `Enter` again to confirm. Search runs only on `Enter`. The search is async and debounced; selected components keep their source
paths and become `--metadata` entries in the command.

The confirmation dialog shows every selected component as a checkbox: `Space` toggles, `↑↓` moves,
`c` shows the exact command, `v/t/w` toggle deploy options, `g` cycles orgs, `Enter` runs and `Esc`
goes back.

Every command result is rendered human-readable immediately: status, counts, per-item pass/fail
rows with `[✓]`/`[x]` icons, and sections for errors/values. Raw stdout/JSON is behind `o`. Apex
test results also include per-class coverage; press `v` to view saved runs.

## Configuration

Configuration lives in a per-user directory chosen by the `directories` crate:

- Linux: `~/.config/sf-tui/config.toml`
- macOS: `~/Library/Application Support/sf-tui/config.toml`
- Windows: `%APPDATA%\sf-tui\config\config.toml`

Set `SFTUI_CONFIG_DIR` to use a different directory. The TOML file contains `favorites`
(command id plus preset flags), `recent_runs` and `theme.accent` (`cyan`, `green`, `yellow`,
`blue`, `magenta`, `red`). It is created on first exit. A `catalog-cache.json` file beside it
stores the last successful `sf commands --json` response for seven days.

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
