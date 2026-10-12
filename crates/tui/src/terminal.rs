use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::CrosstermBackend;
use tokio::sync::mpsc;

type Backend = ratatui::Terminal<CrosstermBackend<Stdout>>;

/// Whether the kitty keyboard protocol was turned on, so it is turned off again.
static ENHANCED_KEYS: AtomicBool = AtomicBool::new(false);

/// Full-screen terminal session; restores the terminal on drop and on panic.
///
/// Where the terminal supports it, keys are reported unambiguously, so
/// combinations like Ctrl+Enter, Shift+Enter or Cmd+← reach the app.
pub struct Terminal {
    pub inner: Backend,
}

impl Terminal {
    pub fn enter() -> io::Result<Self> {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        if terminal::supports_keyboard_enhancement().unwrap_or(false) {
            let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
            execute!(io::stdout(), PushKeyboardEnhancementFlags(flags))?;
            ENHANCED_KEYS.store(true, Ordering::Relaxed);
        }
        let inner = ratatui::Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Self { inner })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    if ENHANCED_KEYS.swap(false, Ordering::Relaxed) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

/// Forwards terminal events from a blocking reader thread into a channel.
pub fn spawn_event_reader() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    rx
}
