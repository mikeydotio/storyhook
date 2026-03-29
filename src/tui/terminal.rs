use std::io::stdout;

use crossterm::execute;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::DefaultTerminal;

use crate::error::AppError;

/// Initialize the terminal for TUI mode.
///
/// Enters alternate screen, enables raw mode, installs a panic hook that
/// restores terminal state, and enables mouse capture.
pub fn init() -> Result<DefaultTerminal, AppError> {
    // ratatui::init() handles: raw mode, alternate screen, panic hook, terminal creation.
    let terminal = ratatui::init();
    // Enable mouse capture on top of the ratatui defaults.
    execute!(stdout(), EnableMouseCapture)
        .map_err(|e| AppError::Storage(format!("failed to enable mouse capture: {e}")))?;
    Ok(terminal)
}

/// Restore the terminal to its original state.
///
/// Disables mouse capture, then delegates to ratatui::restore() which
/// leaves the alternate screen and disables raw mode.
pub fn restore() {
    // Best-effort: disable mouse capture before ratatui restores.
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
}
