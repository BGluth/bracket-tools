//! Terminal lifecycle: an RAII raw-mode/alternate-screen guard plus a panic
//! hook that restores the screen before the panic message prints.
//!
//! A manual guard (rather than `ratatui::init`) so the panic path can also
//! route the report into the tracing log and force a backtrace capture.

use std::{
    backtrace::Backtrace,
    io::{self, Stdout},
    panic,
};

use crossterm::{
    cursor::Show,
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Raw mode + alternate screen while alive; restored on Drop — including the
/// early-return error paths that never reach the main loop.
pub struct TerminalGuard {
    pub terminal: Tui,
}

impl TerminalGuard {
    pub fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Best-effort, idempotent restore — callable from Drop, the panic hook, or
/// both in either order.
pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Chains the default hook: restore the terminal FIRST so the report lands
/// on a readable screen, mirror it into the tracing log, then defer to the
/// default hook.
pub fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        let backtrace = Backtrace::force_capture();
        eprintln!("scheduler panicked: {info}");
        tracing::error!("panic: {info}\n{backtrace}");
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::restore_terminal;

    #[test]
    fn restore_is_idempotent_outside_raw_mode() {
        // Headless (non-tty) and never in raw mode: both calls must be
        // silent no-ops, not errors.
        restore_terminal();
        restore_terminal();
    }
}
