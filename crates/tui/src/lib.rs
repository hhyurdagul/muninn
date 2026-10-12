//! Reusable ratatui pieces for terminal chat apps.

mod chat_log;
mod diff;
mod input;
mod markdown;
mod picker;
mod terminal;
mod wrap;

pub use chat_log::{ChatLog, Item};
pub use diff::diff_lines;
pub use input::Input;
pub use markdown::markdown;
pub use picker::Picker;
pub use ratatui;
pub use terminal::{spawn_event_reader, Terminal};
pub use wrap::wrap_line;

use ratatui::layout::Rect;

/// A rectangle of at most `width` x `height` centered in `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height }
}
