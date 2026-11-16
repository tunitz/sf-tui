//! `sftui`: a terminal UI for the Salesforce CLI.
//!
//! Domain modules (`sf`, `config`, `json_view`, `clipboard`) contain no terminal-framework
//! types. The `ui` and `app` modules adapt that domain to ratatui and crossterm.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod json_view;
pub mod sf;
pub mod ui;

use anyhow::Context;
use config::Config;

/// Entry point used by the binary: probe the CLI before entering the alternate screen.
pub async fn run() -> anyhow::Result<()> {
    let config = Config::load().context("loading configuration")?;
    let startup = sf::startup().await;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        previous_hook(info);
    }));

    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let result = app::run(&mut terminal, startup, config).await;
    let _ = ratatui::try_restore();
    result
}

#[cfg(test)]
mod layering_tests {
    /// Domain modules must stay free of UI-framework types so they can be tested headlessly
    /// and reused by a future shell without pulling ratatui or crossterm along.
    #[test]
    fn domain_modules_do_not_import_terminal_frameworks() {
        let sources = [
            include_str!("config.rs"),
            include_str!("json_view.rs"),
            include_str!("clipboard.rs"),
            include_str!("sf/mod.rs"),
            include_str!("sf/catalog.rs"),
            include_str!("sf/deploy.rs"),
            include_str!("sf/detect.rs"),
            include_str!("sf/org.rs"),
            include_str!("sf/project.rs"),
            include_str!("sf/runner.rs"),
        ];
        for source in sources {
            assert!(
                !source.contains("ratatui::"),
                "domain module imports ratatui"
            );
            assert!(
                !source.contains("crossterm::"),
                "domain module imports crossterm"
            );
        }
    }
}
