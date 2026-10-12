use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::markdown::markdown;
use crate::wrap::wrap_line;

/// One entry in a [`ChatLog`]: an optional marker in a gutter, then a wrapped
/// body that is indented past the marker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub marker: Option<(String, Style)>,
    pub body: String,
    pub style: Style,
    /// Render the body as Markdown.
    pub markdown: bool,
    /// No blank line between this and a neighboring compact item (for runs
    /// of small status lines).
    pub compact: bool,
}

impl Item {
    pub fn new(body: impl Into<String>, style: Style) -> Self {
        Self { body: body.into(), style, ..Self::default() }
    }

    pub fn marker(mut self, marker: impl Into<String>, style: Style) -> Self {
        self.marker = Some((marker.into(), style));
        self
    }

    pub fn markdown(mut self) -> Self {
        self.markdown = true;
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let gutter = self.marker.as_ref().map_or(0, |(m, _)| m.width());
        let body_width = width.saturating_sub(gutter).max(1);
        let mut body = if self.markdown {
            markdown(&self.body, body_width).into_iter().map(|line| line.patch_style(self.style)).collect()
        } else {
            wrap_line(&[Span::styled(self.body.as_str(), self.style)], body_width)
        };
        if body.is_empty() {
            body.push(Line::default());
        }
        for (i, line) in body.iter_mut().enumerate() {
            let prefix = match &self.marker {
                Some((marker, style)) if i == 0 => Span::styled(marker.clone(), *style),
                _ => Span::raw(" ".repeat(gutter)),
            };
            line.spans.insert(0, prefix);
        }
        body
    }
}

/// A scrollable transcript that sticks to the bottom unless scrolled up.
#[derive(Default)]
pub struct ChatLog {
    pub items: Vec<Item>,
    /// Lines scrolled up from the bottom.
    scroll: usize,
    /// Each item's lines, reused while the item and the width stay the same.
    rendered: Vec<Rendered>,
}

struct Rendered {
    width: usize,
    item: Item,
    lines: Vec<Line<'static>>,
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
        let width = area.width as usize;
        self.rendered.truncate(self.items.len());
        for (i, item) in self.items.iter().enumerate() {
            if self.rendered.get(i).is_some_and(|r| r.width == width && r.item == *item) {
                continue;
            }
            let fresh = Rendered { width, item: item.clone(), lines: item.lines(width) };
            if i < self.rendered.len() {
                self.rendered[i] = fresh;
            } else {
                self.rendered.push(fresh);
            }
        }

        let blank = Line::default();
        let mut lines: Vec<&Line> = Vec::new();
        for (i, rendered) in self.rendered.iter().enumerate() {
            if i > 0 && !(self.items[i].compact && self.items[i - 1].compact) {
                lines.push(&blank);
            }
            lines.extend(&rendered.lines);
        }
        let height = area.height as usize;
        let max_scroll = lines.len().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
        let start = max_scroll - self.scroll;
        let visible: Vec<Line> = lines.into_iter().skip(start).take(height).cloned().collect();
        frame.render_widget(Paragraph::new(visible), area);
    }
}
