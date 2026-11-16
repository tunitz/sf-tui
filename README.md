# sftui

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A single-binary terminal UI for the Salesforce CLI. `sftui` wraps the `sf` executable already
installed on your machine: it does not store credentials, and all authentication stays with `sf`
in its own home directory.

The interface has two layers:

- a **generic command catalog** covering the full `sf` CLI surface (fuzzy search, favorites,
  preset flags, exact invocation and streamed output);
- **focused screens** for org operations and metadata deploy/retrieve.

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

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab`, `1`–`6` | switch screens |
| `?` | keyboard help |
| `q` | quit |
| `Ctrl+C` | cancel the newest running command, or quit when idle |
| `F5` | refresh orgs, project or catalog |

| Screen | Keys |
| --- | --- |
| Catalog | `/` search · `↑↓` move · `Enter` load live `--help` · `r` run with flags · `f` favorite · `e` preset flags · `y` copy |
| Favorites | `Enter` run · `Ctrl+↑↓` reorder · `e` preset · `x` remove |
| Orgs | `↑↓` move · `Enter` details · `o` open browser · `d` project default · `D` dev hub · `x` logout · `l` web login · `c` scratch org |
| Deploy/Retrieve | `Space` select metadata · `m` payload mode · `v` check-only · `t` test level · `w` ignore warnings · `o` target org · `r` toggle retrieve · `Enter` run · `p` poll now |
| Output | `↑↓` history · `j`/`k` scroll · `z` toggle JSON table · `y` copy command · `Y` copy output |
| Setup | `↑↓` installer · `Enter` run installer · `r` re-probe |

## Walkthroughs

### Work with orgs

Open **Orgs** and press `F5` to run `sf org list --json`. The table shows alias, username, type,
default markers and connection status. Select an org and press `Enter` for `sf org display --json`;
tokens and auth URLs are filtered out. From the same screen you can open the org in a browser, set
the project default org or dev hub, log out with confirmation, log in via the web flow or create a
scratch org with an alias, definition file and duration.

### Deploy or retrieve metadata

Open **Deploy/Retrieve**. `sftui` walks upward from the current directory to find
`sfdx-project.json` and lists package directories, manifests and source files. Choose a payload
mode with `m`, select paths with `Space`, set validation and test options, then press `Enter`. The
exact command is shown and streamed in **Output**. An asynchronous job id triggers periodic
`sf project deploy report --json --job-id …` polling with a spinner and component counts; failures
and their problems are shown in the results panel. Retrieve uses the same flow with manifests,
source paths or tracked changes.

## Configuration

Configuration lives in a per-user directory chosen by the `directories` crate:

- Linux: `~/.config/sf-tui/config.toml`
- macOS: `~/Library/Application Support/sf-tui/config.toml`
- Windows: `%APPDATA%\sf-tui\config\config.toml`

Set `SFTUI_CONFIG_DIR` to use a different directory. The TOML file contains `favorites`
(command id plus preset flags), `recent_runs` and `theme.accent` (`cyan`, `green`, `yellow`,
`blue`, `magenta`, `red`). It is created on first exit.

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
