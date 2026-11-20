# sftui

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A single-binary terminal UI for the Salesforce CLI. `sftui` wraps the `sf` executable already
installed on your machine: it does not store credentials, and all authentication stays with `sf`
in its own home directory.

The interface opens on a **Menu**: Retrieve/Deploy Metadata, Create Metadata and Apex Tests.
Retrieve and Deploy share one Metadata screen; you choose the action in the confirmation dialog
after selecting components. The default org is pre-filled so you do not retype it. `Esc` returns to
the previous screen and `h` returns to the Menu; there is no tab bar.

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
