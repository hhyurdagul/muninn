use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

/// A multi-line text field. The owner decides what Enter means; Alt+Enter,
/// Shift+Enter and Ctrl+J insert a newline.
#[derive(Default)]
pub struct Input {
    text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
    /// Render every character as `•` (for secrets).
    pub masked: bool,
}

impl Input {
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self { cursor: text.len(), text, masked: false }
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the text and clears the field.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert_str(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    /// Applies an editing key; returns whether it was used. Up/Down are only
    /// used while there is a line above/below to move to.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => self.insert_str("\n"),
            KeyCode::Char('j') if ctrl => self.insert_str("\n"),
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => {
                let before = &self.text[..self.cursor];
                let start = before.trim_end().rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl && !alt => self.insert_str(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => {
                if let Some(prev) = self.prev_boundary() {
                    self.text.drain(prev..self.cursor);
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                if let Some(next) = self.next_boundary() {
                    self.text.drain(self.cursor..next);
                }
            }
            KeyCode::Left => self.cursor = self.prev_boundary().unwrap_or(0),
            KeyCode::Right => self.cursor = self.next_boundary().unwrap_or(self.text.len()),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up => return self.move_vertically(false),
            KeyCode::Down => return self.move_vertically(true),
            _ => return false,
        }
        true
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..].chars().next().map(|c| self.cursor + c.len_utf8())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i)
    }

    fn move_vertically(&mut self, down: bool) -> bool {
        let start = self.line_start();
        let column = self.text[start..self.cursor].chars().count();
        let target_start = if down {
            let end = self.line_end();
            if end == self.text.len() {
                return false;
            }
            end + 1
        } else {
            if start == 0 {
                return false;
            }
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        };
        let line = self.text[target_start..].split('\n').next().unwrap_or("");
        let offset: usize = line.chars().take(column).map(char::len_utf8).sum();
        self.cursor = target_start + offset;
        true
    }

    /// Display rows, hard-wrapped at `width`, plus the cursor's (row, col).
    fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = vec![String::new()];
        let mut col = 0;
        let mut cursor = (0, 0);
        for (i, ch) in self.text.char_indices() {
            if i == self.cursor {
                cursor = (rows.len() - 1, col);
            }
            if ch == '\n' {
                rows.push(String::new());
                col = 0;
                continue;
            }
            let w = ch.width().unwrap_or(0);
            if col + w > width {
                rows.push(String::new());
                col = 0;
                if i == self.cursor {
                    cursor = (rows.len() - 1, 0);
                }
            }
            rows.last_mut().unwrap().push(if self.masked { '•' } else { ch });
            col += w;
        }
        if self.cursor == self.text.len() {
            if col >= width {
                rows.push(String::new());
                col = 0;
            }
            cursor = (rows.len() - 1, col);
        }
        (rows, cursor)
    }

    /// Rows needed to show the whole text at `width`.
    pub fn height(&self, width: u16) -> u16 {
        self.layout(width as usize).0.len() as u16
    }

    /// Draws the field and places the terminal cursor in it.
    pub fn render(&self, frame: &mut Frame, area: Rect, block: Block) {
        let inner = block.inner(area);
        let (rows, (row, col)) = self.layout(inner.width as usize);
        let skip = (row + 1).saturating_sub(inner.height as usize);
        let visible: Vec<_> = rows.into_iter().skip(skip).map(ratatui::text::Line::from).collect();
        frame.render_widget(Paragraph::new(visible).block(block), area);
        frame.set_cursor_position(Position::new(inner.x + col as u16, inner.y + (row - skip) as u16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(input: &mut Input, s: &str) {
        for c in s.chars() {
            input.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn edits_multibyte_text_at_the_cursor() {
        let mut input = Input::default();
        type_str(&mut input, "çay");
        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Backspace));
        type_str(&mut input, "ş");
        assert_eq!(input.text(), "şay");
        input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "ay");
    }

    #[test]
    fn vertical_moves_keep_column_and_report_edges() {
        let mut input = Input::with_text("abc\nx");
        assert!(!input.handle_key(key(KeyCode::Down)), "already on the last line");
        assert!(input.handle_key(key(KeyCode::Up)));
        type_str(&mut input, "!");
        assert_eq!(input.text(), "a!bc\nx");
        assert!(!input.handle_key(key(KeyCode::Up)));
    }

    #[test]
    fn layout_wraps_and_tracks_cursor() {
        let input = Input::with_text("abcd\ne");
        assert_eq!(input.layout(3), (vec!["abc".into(), "d".into(), "e".into()], (2, 1)));
        assert_eq!(Input::with_text("abc").layout(3).1, (1, 0), "cursor wraps after a full row");
        assert_eq!(Input::with_text("ab").masked().layout(5).0, vec!["••".to_string()]);
    }
}
