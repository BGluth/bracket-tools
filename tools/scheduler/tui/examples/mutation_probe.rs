//! One-shot live probe for the set mutations, exercising the full SDK write
//! path (auth → governor → mutation → cache delete-invalidation → extraction).
//!
//! WRITES to start.gg — point it only at a set you own, ideally on a
//! throwaway tournament:
//!
//! ```text
//! cargo run -p bracket-tools-scheduler --example mutation_probe -- \
//!     --token-file <path> --set-id <numeric id> --action in-progress
//! ```

use std::{fs, path::PathBuf, str::FromStr};

use anyhow::{Context, Result};
use bracket_tools_startgg::{types::GGRestToken, GGProvider, StartGgId};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, ValueEnum)]
enum Action {
    Called,
    InProgress,
}

#[derive(Parser)]
#[command(about = "Fire markSetCalled/markSetInProgress at one set and print the returned payload")]
struct ProbeArgs {
    /// Path to a file containing the start.gg API token
    #[arg(long)]
    token_file: PathBuf,

    /// Numeric set id (preview_* ids are not accepted by this probe)
    #[arg(long)]
    set_id: StartGgId,

    /// Which mutation to fire
    #[arg(long, value_enum)]
    action: Action,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = ProbeArgs::parse();

    let raw = fs::read_to_string(&args.token_file).with_context(|| format!("reading {}", args.token_file.display()))?;
    let token = GGRestToken::from_str(raw.trim()).map_err(|e| anyhow::anyhow!("invalid token: {e}"))?;
    let provider = GGProvider::builder(token).build()?;

    let result = match args.action {
        Action::Called => provider.mark_set_called(args.set_id).await?,
        Action::InProgress => provider.mark_set_in_progress(args.set_id).await?,
    };

    println!("{result:#?}");

    Ok(())
}
