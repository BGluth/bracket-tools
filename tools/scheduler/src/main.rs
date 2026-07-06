//! `scheduler` — the TO-desk multi-bracket calling tool (S3 TUI).
//!
//! Real preflight → task-supervised Elm loop wiring lands across the S3
//! commits; for now the binary loads and validates the config.

use bracket_tools_scheduler::{cli::Cli, SchedulerConfig};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = SchedulerConfig::load(&cli.config)?;
    println!(
        "config OK: {} brackets, {} setups (TUI wiring lands later in S3)",
        config.brackets.len(),
        config.setups.len()
    );
    Ok(())
}
