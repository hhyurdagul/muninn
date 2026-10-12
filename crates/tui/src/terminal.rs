use std::io::{self, Stdout};

use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::CrosstermBackend;
use tokio::sync::mpsc;

type Backend = ratatui::Terminal<CrosstermBackend<Stdout>>;

/// Full-screen terminal session; restores the terminal on drop and on panic.
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
