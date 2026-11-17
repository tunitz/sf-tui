//! `sftui`: a terminal UI for the Salesforce CLI.
//!
//! Domain modules (`sf`, `config`, `json_view`, `clipboard`) contain no terminal-framework
//! types. The `ui` and `app` modules adapt that domain to ratatui and crossterm.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod fsutil;
pub mod json_view;
pub mod sf;
pub mod ui;
pub mod update;
pub mod view;

use anyhow::Context;
use config::Config;

/// Entry point used by the binary: open the UI immediately, then probe the CLI in the background.
pub async fn run() -> anyhow::Result<()> {
    let (config, config_notice) = Config::load_or_recover();
    let config_dir = Config::path()
        .and_then(|path| {
            path.parent()
                .map(std::path::Path::to_path_buf)
                .ok_or(config::ConfigError::NoConfigDirectory)
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let startup = sf::Startup::pending();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        previous_hook(info);
    }));

    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let result = app::run(&mut terminal, startup, config, config_dir, config_notice).await;
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
            include_str!("fsutil.rs"),
            include_str!("json_view.rs"),
            include_str!("clipboard.rs"),
            include_str!("view.rs"),
            include_str!("update.rs"),
            include_str!("sf/mod.rs"),
            include_str!("sf/catalog.rs"),
            include_str!("sf/commands.rs"),
            include_str!("sf/deploy.rs"),
            include_str!("sf/detect.rs"),
            include_str!("sf/metadata.rs"),
            include_str!("sf/org.rs"),
            include_str!("sf/project.rs"),
            include_str!("sf/runner.rs"),
            include_str!("sf/test_results.rs"),
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
