use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "chargecapd")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Daemon,
    /// Install the LaunchDaemon.
    Install,
    /// Uninstall the LaunchDaemon.
    Uninstall,
    /// Print the current charge status.
    Status,
    /// Set the upper charge limit.
    Limit { upper: u8 },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => eprintln!("chargecapd: daemon not implemented"),
        Command::Install => eprintln!("chargecapd: install not implemented"),
        Command::Uninstall => eprintln!("chargecapd: uninstall not implemented"),
        Command::Status => eprintln!("chargecapd: status not implemented"),
        Command::Limit { .. } => eprintln!("chargecapd: limit not implemented"),
    }
    std::process::ExitCode::from(2)
}
