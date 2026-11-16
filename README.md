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

Every screen shows a compact **Actions** bar with keys and labels. `Tab` cycles screens, digits
`1`–`7` jump directly, `?` opens the full key map and `q` quits.

The layout adapts to the terminal: below 105 columns or 30 rows it collapses side-by-side panes
and trims the header; below 72 columns or 20 rows it becomes a single column with a one-line
header and status bar, aimed at VS Code and Zed terminal panels. Rendering is tested down to
48x12.

| Screen | Keys |
| --- | --- |
| Home | `r` retrieve · `d` deploy · `t` run tests · `g` results · `c` create metadata · `a` login |
| Catalog | `/` search · `↑↓` move · `Enter` live `--help` · `r` run with flags · `f` favorite |
| Favorites | `Enter` run · `Ctrl+↑↓` reorder · `e` preset flags · `x` remove |
| Orgs | `↑↓` move · `Enter` details · `o` open browser · `d` project default · `D` dev hub · `x` logout · `l` login · `c` scratch org |
| Deploy/Retrieve | `Space` select · `m` payload mode · `v` validate · `t` test level · `w` ignore warnings · `o` target org · `r` toggle retrieve · `Enter` run |
| Output | `↑↓` history · `j`/`k` scroll · `z` JSON table · `y` copy command · `Y` copy output |
| Setup | `↑↓` installer · `Enter` run · `r` re-probe |

## Walkthroughs

### Create metadata

Press `c` on Home. Type part of a template name — `apex class`, `lwc`, `flexi`, `custom object` —
and pick from the fuzzy list, then enter the component name. `sftui` builds the matching
`sf template generate`, `sf schema generate` or `sf cmdt generate` command and streams it in Output.

### Work with orgs

Open **Orgs** and press `F5` to run `sf org list --json`. The table shows alias, username, type,
default markers and connection status. Select an org and press `Enter` for `sf org display --json`;
tokens and auth URLs are filtered out. From the same screen you can open the org in a browser, set
the project default org or dev hub, log out with confirmation, log in via the web flow or create a
scratch org with an alias, definition file and duration.

### Deploy or retrieve metadata

From Home press `r` or `d`, then type part of a component name — for example `apex demo` or
`flow`. `sftui` fuzzy-searches local source files, `package.xml` manifests and common metadata
types, showing suggestions such as `ApexClass:Demo`. Use `↑↓` to pick a suggestion and `Enter` to
accept; leave the field blank to use all package directories from `sfdx-project.json`. The target
org defaults to your CLI default. The command is prepared on the Deploy/Retrieve screen; press
`Enter` there to run. You can still pick a manifest or individual paths with `Space`.

Deploy uses `sf project deploy start --source-dir …` with `--dry-run` for validation, and an async
job id triggers `sf project deploy report --json --job-id …` polling with a spinner and component
counts. Retrieve uses `sf project retrieve start` and shows its final result directly, matching the
current CLI (there is no `project retrieve report` command). Failures and their problems are shown
in the results panel and the raw stream stays in Output.

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
