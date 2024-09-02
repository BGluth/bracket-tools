use std::time::Duration;

use clap::{Args, Parser};
use gg_query_core::types::GGRestToken;

#[derive(Debug, Parser)]
#[command(version, about, author)]
pub(crate) struct ProgArgs {
    /// The token to use when sending requests to `start.gg`
    #[arg(short = 't')]
    pub(crate) gg_token: GGRestToken,

    #[command(flatten)]
    pub(crate) render_cfg: RenderCfg,
}

#[derive(Args, Debug)]
pub(crate) struct RenderCfg {
    /// The maximum fps the TUI should render at.
    #[arg(long = "max_fps", default_value_t = 60, help_heading = "Rendering")]
    max_fps: usize,
}

impl RenderCfg {
    pub(crate) fn delay_between_renders(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.max_fps as f64)
    }
}
