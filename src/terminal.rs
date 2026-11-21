use std::{io, panic, sync::Once};

use crossterm::{
    cursor::Show,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

static PANIC_HOOK_ONCE: Once = Once::new();

/// RAII guard that enables raw mode and the alternate screen on creation,
/// and restores the terminal on drop or panic.
pub struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    /// Installs a panic hook that restores the terminal, enables raw mode,
    /// enters the alternate screen, and returns a guard that restores on drop.
    pub fn init() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;

        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen) {
            restore_terminal();
            return Err(err);
        }

        let backend = CrosstermBackend::new(io::stdout());
        let terminal = match Terminal::new(backend) {
            Ok(term) => term,
            Err(err) => {
                restore_terminal();
                return Err(err);
            }
        };

        Ok(Self { terminal })
    }

    /// Draws a single frame using the provided closure.
    pub fn draw<F>(&mut self, render_fn: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal.draw(render_fn)?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn install_panic_hook() {
    PANIC_HOOK_ONCE.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}
