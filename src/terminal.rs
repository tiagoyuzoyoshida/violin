use std::{
    io::{self, Write as _},
    panic,
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

use crossterm::{
    cursor::Show,
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

static PANIC_HOOK_ONCE: Once = Once::new();
static KEYBOARD_FLAGS_PUSHED: AtomicBool = AtomicBool::new(false);
static KITTY_IMAGE_ACTIVE: AtomicBool = AtomicBool::new(false);

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

        if matches!(supports_keyboard_enhancement(), Ok(true))
            && execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok()
        {
            KEYBOARD_FLAGS_PUSHED.store(true, Ordering::SeqCst);
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

    /// Writes a Kitty Graphics Protocol escape sequence to stdout and marks the overlay active.
    pub fn write_kitty_image(&mut self, seq: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.write_all(seq.as_bytes())?;
        stdout.flush()?;
        KITTY_IMAGE_ACTIVE.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Clears the active Kitty Graphics Protocol image from stdout.
    pub fn clear_kitty_image(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.write_all(crate::art::kitty_clear_image().as_bytes())?;
        stdout.flush()?;
        KITTY_IMAGE_ACTIVE.store(false, Ordering::SeqCst);
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
    if KITTY_IMAGE_ACTIVE.swap(false, Ordering::SeqCst) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(crate::art::kitty_clear_image().as_bytes());
        let _ = stdout.flush();
    }
    if KEYBOARD_FLAGS_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}
