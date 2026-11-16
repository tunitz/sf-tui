---
name: ratatui-conventions
description: How this repository structures its ratatui/crossterm event loop, application state, rendering and headless UI tests. Use when touching src/app.rs, src/ui.rs or terminal behaviour.
---

# Ratatui conventions

## Event loop

- Use the pinned ratatui version (0.30.x). It ships `try_init()` / `try_restore()` and a panic
  hook must restore the terminal before delegating to the previous hook.
- The binary probes the Salesforce CLI *before* entering the alternate screen.
- The loop waits on `tokio::select!` between a 100 ms ticker and the application event channel.
  Terminal events are drained with `crossterm::event::poll(Duration::ZERO)` and `read()`, so the
  event loop never blocks waiting for a key.
- Only handle `KeyEventKind::Press` to avoid double handling on Windows key-repeat.
- Long-running child processes never run in the UI thread. They stream `SfEvent`s into `AppEvent`s
  through a bounded-lifetime channel, with one `RunningJob` per child tracked in the app.
- `Ctrl+C` cancels the newest job; with no job it quits. `Esc` clears status/cancels a transfer.

## Application state

- `App` owns all screens, config, modal state, job handles and the event receiver. Widgets are
  built fresh from that state on every frame; there is no retained widget tree.
- Screen state structs live in `src/app.rs`; rendering in `src/ui.rs` reads but never mutates them.
  Navigation helpers that need mutation belong on `App`.
- Represent mutually exclusive states as enums (`Modal`, `JobPurpose`, `CatalogRow`) rather than
  loose booleans and options.
- Pure input parsing (shell-like flag splitting, JSON extraction, report parsing) lives in domain
  modules and is unit tested there.

## Rendering discipline

- One top-level `draw()` splits the frame into header, active screen and status/footer, then draws
  a centered `Clear` overlay when a modal is open.
- Never render outside the frame area. Derive all widths from `Rect` and clamp scroll offsets and
  list selections before use.
- Highlight with one accent color from config; use dim gray for secondary information and reserve
  red for errors and yellow for warnings. Keep stderr in a distinct color.
- Do not display credentials or tokens even when the CLI returns them; filter them in the parser.
- Keep a useful hint line on the footer so the interface is discoverable without external docs.

## Headless tests

- Use `ratatui::backend::TestBackend` with `Terminal::new` and draw every screen at a realistic
  size (at least 100x30). Assert the buffer is non-empty, and exercise modal overlays too.
- Construct `App` directly from a synthetic `Startup` and `Config`; do not require a real CLI.
- Narrow terminals are a separate concern: test 80x24 at minimum before claiming small-screen
  support.

## crossterm and Windows pitfalls

- Windows terminals may not have ANSI/UTF-8 enabled by default. Prefer plain ASCII in critical
  status text, and avoid relying on emoji or box-drawing glyphs for meaning.
- `KeyEventKind::Press` filtering avoids duplicate actions from key-repeat events on Windows.
- Never leave raw mode enabled: restore explicitly on normal exit and from the panic hook.
- Spawning `.cmd`/`.bat` helpers needs the OS shell (`cmd.exe /C`), not a direct process spawn.
- Clipboard access has no cross-platform crate here: `pbcopy`, `xclip`/`xsel`/`wl-copy` and
  PowerShell `Set-Clipboard` are used, and a missing helper is reported, not fatal.
