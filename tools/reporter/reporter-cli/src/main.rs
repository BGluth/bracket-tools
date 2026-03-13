mod dispatcher;
mod message_loop;
mod pages;
mod prog_args;
mod tui_state;

use clap::Parser;
use message_loop::ProgState;
use prog_args::ProgArgs;
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

#[derive(Debug)]
struct App {}

impl Widget for App {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        todo!()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let p_args = ProgArgs::parse();
    let (p_state, _) = ProgState::init(p_args)?;

    p_state.process_messages().await?;

    Ok(())
}
