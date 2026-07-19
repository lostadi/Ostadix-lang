//! Hosted Live-World Reference command line.
//!
//! This executable is a semantic oracle for the future O-core live system. It
//! is deliberately named `-host`: its workers are local host child processes,
//! not dynamically loaded O-core tasks.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser as ClapParser, Subcommand};

use o_lang::live_system::protocol;

#[derive(Debug, ClapParser)]
#[command(
    name = "o-live-host",
    about = "Hosted package-managed Live-World reference for Ostadix"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Internal fixed-protocol runtime worker entrypoint.
    #[command(name = "__worker", hide = true)]
    Worker {
        #[arg(long)]
        package_root: PathBuf,
        #[arg(long)]
        entry: String,
        #[arg(long)]
        service: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Worker {
            package_root,
            entry,
            service,
        } => protocol::worker_main(&package_root, &entry, &service),
    }
}
