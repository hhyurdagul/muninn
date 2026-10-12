use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::wrap::wrap_line;

/// Renders Markdown as lines wrapped to `width` columns: CommonMark plus
/// GitHub tables, task lists, strikethrough, footnotes and alerts, `$math$`
/// and `[[wikilinks]]`.
pub fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS;
    let mut renderer = Renderer { width: width.max(1), ..Renderer::default() };
    for event in Parser::new_ext(text, options) {
        renderer.event(event);
    }
    renderer.flush();
    renderer.lines
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn code() -> Style {
    Style::default().fg(Color::Yellow)
}

fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

fn heading(level: HeadingLevel) -> Style {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match level {
        HeadingLevel::H1 => bold.fg(Color::Magenta).add_modifier(Modifier::UNDERLINED),
        HeadingLevel::H2 => bold.fg(Color::Magenta),
        HeadingLevel::H3 => bold.fg(Color::Cyan),
        _ => bold.add_modifier(Modifier::ITALIC),
    }
}

/// A block that prefixes every line inside it.
enum Container {
    /// `│ ` in the given style.
    Quote(Style),
    /// A list item or footnote: `marker` on its first line, then `indent` spaces.
    Indent { marker: Option<Span<'static>>, indent: usize },
}

struct Link {
    /// Index into `inline` where the link text starts.
    start: usize,
    dest: String,
    kind: LinkType,
}

#[derive(Default)]
struct Table {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    has_head: bool,
}

#[derive(Default)]
struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    /// Inline content of the current paragraph, heading or table cell.
    inline: Vec<Span<'static>>,
    styles: Vec<Style>,
    containers: Vec<Container>,
    /// Next number of each open list; `None` for bullet lists.
    lists: Vec<Option<u64>>,
    links: Vec<Link>,
    /// Language and text of the code block being read.
    code: Option<(String, String)>,
    table: Option<Table>,
    /// A blank line is due before the next block.
    gap: bool,
}

impl Renderer {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.code {
                Some((_, body)) => body.push_str(&text),
                None => self.push(&text, self.style()),
            },
            Event::Code(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.push(&text, self.style().patch(code()))
            }
            Event::Html(html) => self.push(&html, dim()),
            Event::InlineHtml(html) => {
                let tag: String = html.chars().filter(|c| !c.is_whitespace() && *c != '/').collect();
                if tag.eq_ignore_ascii_case("<br>") {
                    self.push("\n", Style::default());
                } else {
                    self.push(&html, dim());
                }
            }
            Event::FootnoteReference(label) => self.push(&format!("[{label}]"), accent()),
            Event::SoftBreak => self.push(" ", self.style()),
            Event::HardBreak => self.push("\n", Style::default()),
            Event::Rule => {
                self.begin_block();
                let rule = "─".repeat(self.available());
                self.emit(vec![Span::styled(rule, dim())]);
                self.gap = true;
            }
            Event::TaskListMarker(done) => {
                if let Some(Container::Indent { marker, indent }) = self.containers.last_mut() {
                    let (mark, style) =
                        if done { ("☑ ", Style::default().fg(Color::Green)) } else { ("☐ ", dim()) };
                    *marker = Some(Span::styled(mark, style));
                    *indent = mark.width();
                }
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.begin_block(),
            Tag::Heading { level, .. } => {
                self.begin_block();
                self.push_style(heading(level));
            }
            Tag::BlockQuote(kind) => {
                self.begin_block();
                let alert = kind.map(|kind| match kind {
                    BlockQuoteKind::Note => ("Note", Color::Blue),
                    BlockQuoteKind::Tip => ("Tip", Color::Green),
                    BlockQuoteKind::Important => ("Important", Color::Magenta),
                    BlockQuoteKind::Warning => ("Warning", Color::Yellow),
                    BlockQuoteKind::Caution => ("Caution", Color::Red),
                });
                let bar = alert.map_or(dim(), |(_, color)| Style::default().fg(color));
                self.containers.push(Container::Quote(bar));
                if let Some((label, _)) = alert {
                    self.emit(vec![Span::styled(label, bar.add_modifier(Modifier::BOLD))]);
                }
            }
            Tag::CodeBlock(kind) => {
                self.begin_block();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.begin_block();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => format!("{} ", ["•", "◦", "▪"][depth % 3]),
                };
                let indent = marker.width();
                self.containers.push(Container::Indent { marker: Some(Span::styled(marker, accent())), indent });
            }
            Tag::FootnoteDefinition(label) => {
                self.begin_block();
                let marker = format!("[{label}] ");
                let indent = marker.width();
                self.containers.push(Container::Indent { marker: Some(Span::styled(marker, accent())), indent });
            }
            Tag::Table(alignments) => {
                self.begin_block();
                self.table = Some(Table { alignments, ..Table::default() });
            }
            Tag::TableHead => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD));
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                    table.has_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(Vec::new());
                }
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { link_type, dest_url, .. } => {
                let color = if matches!(link_type, LinkType::WikiLink { .. }) { Color::Cyan } else { Color::Blue };
                self.push_style(Style::default().fg(color).add_modifier(Modifier::UNDERLINED));
                self.links.push(Link { start: self.inline.len(), dest: dest_url.to_string(), kind: link_type });
            }
            Tag::Image { .. } => {
                self.push_style(dim().add_modifier(Modifier::ITALIC));
                self.push("[image: ", self.style());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.gap = true;
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.styles.pop();
                self.gap = true;
            }
            TagEnd::BlockQuote(_) | TagEnd::FootnoteDefinition => {
                self.flush();
                self.containers.pop();
                self.gap = true;
            }
            TagEnd::HtmlBlock => {
                while let Some(last) = self.inline.last_mut() {
                    let trimmed = last.content.trim_end_matches('\n').len();
                    last.content.to_mut().truncate(trimmed);
                    if trimmed > 0 {
                        break;
                    }
                    self.inline.pop();
                }
                self.flush();
                self.gap = true;
            }
            TagEnd::CodeBlock => {
                if let Some((lang, body)) = self.code.take() {
                    self.code_block(&lang, &body);
                }
                self.gap = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if !self.containers.iter().any(|c| matches!(c, Container::Indent { .. })) {
                    self.gap = true;
                }
            }
            TagEnd::Item => {
                self.flush();
                // An empty item still shows its marker.
                if let Some(Container::Indent { marker: Some(_), .. }) = self.containers.last() {
                    self.emit(Vec::new());
                }
                self.containers.pop();
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.render_table(table);
                }
                self.gap = true;
            }
            TagEnd::TableHead => {
                self.styles.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inline);
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                let Some(link) = self.links.pop() else { return };
                let text: String = self.inline[link.start..].iter().map(|s| s.content.as_ref()).collect();
                let shows_url = !matches!(link.kind, LinkType::Autolink | LinkType::Email | LinkType::WikiLink { .. });
                if shows_url && !link.dest.is_empty() && text != link.dest {
                    self.push(&format!(" ({})", link.dest), dim());
                }
            }
            TagEnd::Image => {
                self.push("]", self.style());
                self.styles.pop();
            }
            _ => {}
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }

    fn push(&mut self, text: &str, style: Style) {
        self.inline.push(Span::styled(text.to_string(), style));
    }

    /// Columns left for content inside the open containers.
    fn available(&self) -> usize {
        let used: usize = self
            .containers
            .iter()
            .map(|c| match c {
                Container::Quote(_) => 2,
                Container::Indent { indent, .. } => *indent,
            })
            .sum();
        self.width.saturating_sub(used).max(1)
    }

    /// Ends the running paragraph and puts in a pending blank line.
    fn begin_block(&mut self) {
        self.flush();
        if std::mem::take(&mut self.gap) && !self.lines.is_empty() {
            let prefix = self.prefix(true);
            self.lines.push(Line::from(prefix));
        }
    }

    /// Wraps and emits the collected inline content.
    fn flush(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.inline);
        self.begin_block();
        for line in wrap_line(&spans, self.available()) {
            self.emit(line.spans);
        }
    }

    /// Container prefixes for the next line. Blank lines leave list markers
    /// for the first line with content.
    fn prefix(&mut self, blank: bool) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        for container in &mut self.containers {
            match container {
                Container::Quote(style) => out.push(Span::styled("│ ", *style)),
                Container::Indent { marker, indent } => {
                    let first = if blank { None } else { marker.take() };
                    out.push(first.unwrap_or_else(|| Span::raw(" ".repeat(*indent))));
                }
            }
        }
        out
    }

    fn emit(&mut self, content: Vec<Span<'static>>) {
        let mut spans = self.prefix(false);
        spans.extend(content);
        self.lines.push(Line::from(spans));
    }

    fn code_block(&mut self, lang: &str, body: &str) {
        let width = self.available().max(5);
        let inner = width - 4;
        let label = if lang.is_empty() { String::new() } else { format!(" {lang} ") };
        let fill = width.saturating_sub(3 + label.width());
        self.emit(vec![
            Span::styled("╭─", dim()),
            Span::styled(label, dim().add_modifier(Modifier::ITALIC)),
            Span::styled(format!("{}╮", "─".repeat(fill)), dim()),
        ]);
        for line in body.trim_end_matches('\n').split('\n') {
            for row in hard_wrap(&line.replace('\t', "    "), inner) {
                let pad = " ".repeat(inner.saturating_sub(row.width()));
                self.emit(vec![Span::styled("│ ", dim()), Span::raw(row), Span::raw(pad), Span::styled(" │", dim())]);
            }
        }
        self.emit(vec![Span::styled(format!("╰{}╯", "─".repeat(width - 2)), dim())]);
    }

    fn render_table(&mut self, table: Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0).max(table.alignments.len());
        if columns == 0 {
            return;
        }
        let mut widths = vec![1; columns];
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                let text: String = cell.iter().map(|s| s.content.as_ref()).collect();
                widths[i] = widths[i].max(text.split('\n').map(UnicodeWidthStr::width).max().unwrap_or(0));
            }
        }
        // Shrink the widest column until the table fits; cells wrap.
        let budget = self.available().saturating_sub(3 * columns + 1);
        while widths.iter().sum::<usize>() > budget {
            let widest = (0..columns).max_by_key(|&i| widths[i]).unwrap_or(0);
            if widths[widest] <= 1 {
                break;
            }
            widths[widest] -= 1;
        }

        let rule = |left: &str, middle: &str, right: &str| {
            let segments: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            vec![Span::styled(format!("{left}{}{right}", segments.join(middle)), dim())]
        };
        self.emit(rule("╭", "┬", "╮"));
        for (index, row) in table.rows.iter().enumerate() {
            let cells: Vec<Vec<Line>> = (0..columns)
                .map(|c| row.get(c).map_or_else(|| vec![Line::default()], |cell| wrap_line(cell, widths[c])))
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for r in 0..height {
                let mut spans = vec![Span::styled("│", dim())];
                for (c, cell) in cells.iter().enumerate() {
                    let line = cell.get(r).cloned().unwrap_or_default();
                    let pad = widths[c].saturating_sub(line.width());
                    let (left, right) = match table.alignments.get(c) {
                        Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                        Some(Alignment::Right) => (pad, 0),
                        _ => (0, pad),
                    };
                    spans.push(Span::raw(" ".repeat(left + 1)));
                    spans.extend(line.spans);
                    spans.push(Span::raw(" ".repeat(right + 1)));
                    spans.push(Span::styled("│", dim()));
                }
                self.emit(spans);
            }
            if index == 0 && table.has_head && table.rows.len() > 1 {
                self.emit(rule("├", "┼", "┤"));
            }
        }
        self.emit(rule("╰", "┴", "╯"));
    }
}

/// Splits `text` into rows of at most `width` columns, keeping every space.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("one row").push(ch);
        used += w;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(text: &str, width: usize) -> Vec<String> {
        markdown(text, width).iter().map(|l| l.to_string()).collect()
    }

    fn style_of(lines: &[Line], text: &str) -> Style {
        let span = lines.iter().flat_map(|l| &l.spans).find(|s| s.content == text).expect("span");
        span.style
    }

    #[test]
    fn inline_markup_becomes_styles() {
        let lines = markdown("# Title\n\nSome **bold**, *soft*, ~~gone~~ and `code`.", 80);
        assert_eq!(
            lines.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
            ["Title", "", "Some bold, soft, gone and code."]
        );
        assert!(style_of(&lines, "Title").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&lines, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&lines, "soft").add_modifier.contains(Modifier::ITALIC));
        assert!(style_of(&lines, "gone").add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(style_of(&lines, "code").fg, Some(Color::Yellow));
    }

    #[test]
    fn lists_nest_number_and_indent_wrapped_lines() {
        let text = "1. first item wraps here\n2. second\n   - [x] done\n   - [ ] open";
        assert_eq!(render(text, 16), ["1. first item", "   wraps here", "2. second", "   ☑ done", "   ☐ open"]);
    }

    #[test]
    fn tables_align_columns_and_fit_the_width() {
        let text = "| Note | Count |\n|:-----|------:|\n| ravens.md | 3 |\n| a | 12 |";
        assert_eq!(
            render(text, 40),
            [
                "╭───────────┬───────╮",
                "│ Note      │ Count │",
                "├───────────┼───────┤",
                "│ ravens.md │     3 │",
                "│ a         │    12 │",
                "╰───────────┴───────╯",
            ]
        );
        let narrow = render("| a | b |\n|---|---|\n| one two three four | x |", 16);
        assert!(narrow.iter().all(|l| l.width() <= 16), "{narrow:#?}");
        assert!(narrow.len() > 5, "the long cell wraps onto more rows");
    }

    #[test]
    fn code_blocks_keep_their_text_in_a_box() {
        assert_eq!(
            render("```rs\nfn  main()\n```", 20),
            ["╭─ rs ─────────────╮", "│ fn  main()       │", "╰──────────────────╯"]
        );
    }

    #[test]
    fn quotes_prefix_every_line_and_links_show_targets() {
        assert_eq!(render("> one two three", 9), ["│ one two", "│ three"]);
        assert_eq!(render("[site](https://x.y) and [[Note|alias]]", 80), ["site (https://x.y) and alias"]);
    }
}
