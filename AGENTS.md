# Repository instructions

## Purpose

`sftui` is a single-binary, cross-platform terminal UI that wraps an installed Salesforce CLI
(`sf`; the deprecated `sfdx` CLI is not detected, invoked or translated). It provides focused
retrieve, deploy, Apex test and org screens, a Run command prompt for anything else, and one
global Jobs window for every command. It never stores credentials; all authentication remains
with the Salesforce CLI in its own home directory.

## Layout

```text
src/main.rs            tokio binary entry point, named `sftui`
src/lib.rs             crate root and layering guard test
src/app.rs             application state, key dispatch, background job ownership, event loop
src/ui.rs              all ratatui rendering
src/view.rs            pure result-view construction
src/update.rs           self-update from GitHub releases with checksum verification
src/config.rs          TOML config and theme
src/fsutil.rs          atomic, owner-only state-file writes
src/sf/metadata.rs     component/template suggestion candidates (fuzzy completion)
src/sf/commands.rs     argv builders for invocations the TUI constructs itself
src/json_view.rs       JSON-to-table rendering
src/clipboard.rs       OS clipboard helpers
src/sf/                CLI detection, process runner, org/project/deploy/test-result parsing
tests/                 integration tests and synthetic CLI stub fixtures
.github/workflows/     CI and release automation
install.sh/.ps1        user-level installers (no admin rights)
.agents/skills/        reusable task notes for any harness or human contributor
```

## Development commands

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release --locked
cargo run --bin sftui              # requires an installed sf CLI, or shows the Setup screen
SFTUI_SF_PATH=/path/to/sf cargo run --bin sftui
SFTUI_CONFIG_DIR=/tmp/sftui-config cargo run --bin sftui
```

To debug the interface, run the release binary inside a terminal at least 100x30 cells and use
`?` for the key map. Rendering is also covered by `TestBackend` smoke tests that work headlessly.

## Conventions

- **Error handling:** domain modules return typed `thiserror` errors. `anyhow` is reserved for the
  binary boundary and the terminal lifecycle. Never panic on user input, files or CLI output.
- **Async boundaries:** every spawned task has an owner. Process tasks return through a tracked
  job map and an application event channel; cancellation is explicit. Up to four user commands run
  in parallel and the rest wait in a FIFO queue (`pending_jobs`). A single F2 Jobs window lists
  running, queued and finished commands from every screen, with live output and per-job durations.
  Apex test runs start immediately and are polled with `sf apex get test` until the org reports a
  terminal outcome; the poll is a separate internal job and never holds a user slot. The UI thread
  never awaits a long-running CLI command directly. CLI discovery, probing, project scanning and
  component-index building happen off-thread so a slow Windows `sf` process is visible as
  progress, never as a blank screen. Terminal input is read by a dedicated thread so keystrokes
  are never gated on the animation tick. Process output is drained in batches before each frame.
- **JSON parsing:** treat all CLI output as untrusted. Parse defensively with `serde_json::Value`
  helpers, accept both `result`-wrapped and bare shapes, and never print tokens, auth URLs,
  passwords or secrets. Fixture data must be synthetic only.
- **Layering:** modules under `src/sf`, `src/config`, `src/fsutil`, `src/json_view` and
  `src/clipboard` must not import ratatui or crossterm. The layering test in `src/lib.rs`
  enforces this.
- **Version:** one source of truth, `Cargo.toml`. Stay on `0.1.x` until `0.1.99`; increment only
  the patch number. A `v*` tag must match it exactly.
- **Repository hygiene:** the public repository contains production-ready files only: source,
  tests, CI/release workflows, installers, docs, scripts listed above, skills and license. No
  scratch files, personal data, real org names, editor/OS junk or development-only snippets.

## Release checklist

1. Bump the version in `Cargo.toml` and add a `CHANGELOG.md` entry.
2. Run formatting, clippy, all tests and a locked release build locally.
3. Review `git ls-files` against the hygiene rule above.
4. Commit, tag `v<version>`, and push the commit and tag.
5. Confirm CI is green and the release workflow publishes all five expected binaries.
6. Verify the installers on a clean user account without sudo or elevation.

See `.agents/skills/` for task-specific notes.
