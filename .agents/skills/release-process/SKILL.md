---
name: release-process
description: The version bump, changelog, tag and published-asset verification procedure for sftui. Use when preparing a release or investigating a failed release workflow.
---

# Release process

## Single source of truth

`Cargo.toml` contains the version. A tag `vX.Y.Z` must equal that version exactly. The release
workflow reads both and fails before building on drift; never edit a tag to work around it.

Semantics during initial development: `0.x.y` fixes, `0.x.0` features, `1.0.0` only when stability
is promised.

## Steps

1. Bump `version` in `Cargo.toml`; commit `Cargo.lock` if dependency resolution changed.
2. Add a dated `CHANGELOG.md` section with Added/Changed/Fixed items for that version.
3. Run locally: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test --all-targets`, `cargo build --release --locked`.
4. Review `git ls-files` against the repository hygiene rule in `AGENTS.md`.
5. Commit, then create and push tag `v<version>`.
6. Watch the release workflow. It builds five stripped binaries, compresses them into one asset
   each (`sftui-<version>-<target>.tar.gz` or `.zip`), publishes them and verifies all five exist.

## Expected assets

- `sftui-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `sftui-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `sftui-<version>-x86_64-apple-darwin.tar.gz`
- `sftui-<version>-aarch64-apple-darwin.tar.gz`
- `sftui-<version>-x86_64-pc-windows-msvc.zip`

Each archive contains only the `sftui` executable (or `sftui.exe`). No README, license file,
fixture or extra helper is shipped inside a binary asset.

## Verification

After publishing, confirm the release is discoverable by both installers:

```sh
curl -fsSL https://api.github.com/repos/tunitz/sf-tui/releases/latest | grep tag_name
# Then, on a clean machine without admin rights:
curl -fsSL https://raw.githubusercontent.com/tunitz/sf-tui/main/install.sh | sh
# On Windows PowerShell: irm https://raw.githubusercontent.com/tunitz/sf-tui/main/install.ps1 | iex
```

A fresh shell must resolve `sftui` and launch it. If the CLI is absent, the Setup screen must
appear rather than a crash.
