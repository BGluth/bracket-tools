//! `scheduler` — the TO-desk multi-bracket calling tool (S3 TUI).
//!
//! Scaffolding entry point: real config load → preflight → task-supervised Elm
//! loop wiring lands across the S3 commits. For now it exists so the binary
//! target builds and the dependency graph is locked.

use clap::Parser;

/// Command-line arguments for the scheduler TUI.
#[derive(Debug, Parser)]
#[command(name = "scheduler", about = "Multi-bracket calling tool for the TO desk")]
struct Cli {
    /// Path to the TOML config.
    #[arg(long, default_value = "scheduler.toml")]
    config: String,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    println!("scheduler scaffolding — config: {}", cli.config);
    Ok(())
}
