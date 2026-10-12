use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::wrap::wrap;

/// One entry in a [`ChatLog`]: an optional label line and a wrapped body.
#[derive(Debug, Clone, Default)]
pub struct Item {
    pub label: Option<(String, Style)>,
    pub body: String,
    pub style: Style,
    /// Draw without a blank line after it (for runs of small status lines).
    pub compact: bool,
}

impl Item {
    pub fn new(body: impl Into<String>, style: Style) -> Self {
        Self { body: body.into(), style, ..Self::default() }
    }

    pub fn labeled(label: impl Into<String>, label_style: Style, body: impl Into<String>, style: Style) -> Self {
        Self { label: Some((label.into(), label_style)), ..Self::new(body, style) }
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

/// A scrollable transcript that sticks to the bottom unless scrolled up.
#[derive(Default)]
pub struct ChatLog {
    pub items: Vec<Item>,
    /// Lines scrolled up from the bottom.
    scroll: usize,
}

impl ChatLog {
    pub fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.scroll = 0;
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let lines = self.lines(area.width as usize);
        let height = area.height as usize;
        let max_scroll = lines.len().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
        let start = max_scroll - self.scroll;
        let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
        frame.render_widget(Paragraph::new(visible), area);
    }

    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for item in &self.items {
            if let Some((label, style)) = &item.label {
                out.push(Line::from(Span::styled(label.clone(), *style)));
            }
            if !(item.body.is_empty() && item.label.is_some()) {
                out.extend(wrap(&item.body, width).into_iter().map(|l| Line::from(Span::styled(l, item.style))));
            }
            if !item.compact {
                out.push(Line::default());
            }
        }
        out
    }
}
