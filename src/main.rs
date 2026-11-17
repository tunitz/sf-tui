use std::io::Write;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("sftui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "sftui {} — terminal UI for the Salesforce CLI",
                env!("CARGO_PKG_VERSION")
            );
            println!("Usage: sftui [--version] [--help]");
            println!("       sftui update [--check] [--force]");
            println!("Run without arguments to open the interface.");
            return Ok(());
        }
        Some("update") => {
            let mut options = sftui::update::UpdateOptions::default();
            for argument in args {
                match argument.as_str() {
                    "--check" => options.check_only = true,
                    "--force" => options.force = true,
                    "--help" | "-h" => {
                        println!("Usage: sftui update [--check] [--force]");
                        println!("  --check  report whether a newer release exists");
                        println!(
                            "  --force  reinstall the latest release even when versions match"
                        );
                        return Ok(());
                    }
                    other => {
                        let mut stderr = std::io::stderr();
                        writeln!(stderr, "sftui update: unknown argument {other}")?;
                        std::process::exit(2);
                    }
                }
            }
            if let Err(error) = sftui::update::run(options) {
                let mut stderr = std::io::stderr();
                writeln!(stderr, "sftui update: {error}")?;
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(other) => {
            let mut stderr = std::io::stderr();
            writeln!(stderr, "sftui: unknown argument {other} (try --help)")?;
            std::process::exit(2);
        }
        None => sftui::run().await,
    }
}
