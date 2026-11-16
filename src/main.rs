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
            println!("Run without arguments to open the interface.");
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
