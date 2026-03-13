use std::{
    collections::VecDeque,
    fmt::Debug,
    io::{stdout, Stdout},
    time::Duration,
};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use reporter_state::stores::store_action_processing::Stores;
use tokio::{
    task,
    time::{sleep_until, Instant},
};

use crate::{
    message_loop::{MsgLoopTx, TuiAction},
    pages::stats::Root,
    prog_args::RenderCfg,
};

type Terminal = ratatui::Terminal<CrosstermBackend<Stdout>>;

#[derive(Clone, Copy, Debug)]
enum PageType {
    Root,
}

pub(crate) struct TuiState {
    active_page_stack: VecDeque<PageType>,
    render_timer: RenderBackoffTimer,
    terminal: Terminal,
}

impl TuiState {
    pub(crate) fn new(tx: MsgLoopTx, render_time_cfg: RenderCfg) -> Result<Self> {
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

        Ok(Self {
            active_page_stack: Self::create_page_queue(),
            render_timer: RenderBackoffTimer::new(tx, render_time_cfg),
            terminal,
        })
    }

    fn create_page_queue() -> VecDeque<PageType> {
        let mut p_stack = VecDeque::new();
        p_stack.push_front(PageType::Root);

        p_stack
    }

    pub(crate) fn render(&mut self, stores: &Stores) -> anyhow::Result<()> {
        // Pages will always take up the entire screen.
        let active_page = self
            .active_page_stack
            .front()
            .expect("Somehow found no active page to render (There should always be one)!");

        self.terminal.draw(|frame| {
            match active_page {
                PageType::Root => frame.render_widget_ref(Root::new(stores.tourney_store.create_ref()), frame.size()),
            };
        })?;

        Ok(())
    }
}

#[derive(Debug)]
/// Used to prevent a flooding of render calls from rendering the TUI way too fast.
struct RenderBackoffTimer {
    last_render_time: Instant,
    min_time_between_renders: Duration,
    render_already_queued: bool,
    msg_loop_tx: MsgLoopTx,
}

impl RenderBackoffTimer {
    fn new(msg_loop_tx: MsgLoopTx, cfg: RenderCfg) -> Self {
        Self {
            last_render_time: Instant::now(),
            min_time_between_renders: cfg.delay_between_renders(),
            render_already_queued: false,
            msg_loop_tx,
        }
    }

    /// Sends a render message back to the message loop or else queues one to be seen as soon as enough time passes.
    async fn render_or_backoff(&mut self) {
        let now = Instant::now();
        let next_render_time = self.last_render_time + self.min_time_between_renders;

        match now >= next_render_time {
            // We're rendering too fast, so we'll potentially queue up a render message for later.
            false => {
                if !self.render_already_queued {
                    self.render_already_queued = true;
                    Self::create_delayed_render_task(self.msg_loop_tx.clone(), next_render_time);
                }
            }

            // Enough time has passed to render.
            true => {
                self.last_render_time = now;
                self.render_already_queued = false;

                Self::send_refresh_msg(&self.msg_loop_tx).await;
            }
        }
    }

    fn create_delayed_render_task(tx: MsgLoopTx, next_render_time: Instant) {
        task::spawn(async move {
            sleep_until(next_render_time).await;
            Self::send_refresh_msg(&tx).await;
        });
    }

    async fn send_refresh_msg(tx: &MsgLoopTx) {
        // Should I think only ever be able to be hit when the other end closes?
        tx.send(TuiAction::TuiRefresh).await.expect("Failed to send a TUI refresh message!");
    }
}
