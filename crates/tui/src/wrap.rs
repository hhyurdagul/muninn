use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Word-wraps styled text to `width` display columns. Newlines are kept,
/// words longer than a line are split, trailing spaces are dropped, and an
/// empty input yields one empty line.
pub fn wrap_line(spans: &[Span<'_>], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let chars: Vec<(char, Style)> = spans.iter().flat_map(|s| s.content.chars().map(move |c| (c, s.style))).collect();
    let mut out = Vec::new();
    for paragraph in chars.split(|(c, _)| *c == '\n') {
        let mut line: Vec<(char, Style)> = Vec::new();
        for word in paragraph.split_inclusive(|(c, _)| *c == ' ') {
            let word_width = columns(word.iter().rev().skip_while(|(c, _)| *c == ' '));
            if columns(&line) + word_width > width && !line.is_empty() {
                out.push(finish(std::mem::take(&mut line)));
            }
            for &(ch, style) in word {
                if columns(&line) + ch.width().unwrap_or(0) > width && ch != ' ' && !line.is_empty() {
                    out.push(finish(std::mem::take(&mut line)));
                }
                line.push((ch, style));
            }
        }
        out.push(finish(line));
    }
    out
}

fn columns<'a>(chars: impl IntoIterator<Item = &'a (char, Style)>) -> usize {
    chars.into_iter().map(|(c, _)| c.width().unwrap_or(0)).sum()
}

/// Joins runs of equally styled characters into spans, minus trailing spaces.
fn finish(mut chars: Vec<(char, Style)>) -> Line<'static> {
    while chars.last().is_some_and(|(c, _)| *c == ' ') {
        chars.pop();
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (ch, style) in chars {
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), style)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn plain(text: &str, width: usize) -> Vec<String> {
        wrap_line(&[Span::raw(text)], width).iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn wraps_words_breaks_long_ones_and_keeps_blank_lines() {
        assert_eq!(plain("one two three", 7), ["one two", "three"]);
        assert_eq!(plain("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(plain("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(plain("", 10), [""]);
        assert_eq!(plain("日本語テキスト", 6), ["日本語", "テキス", "ト"]);
        assert_eq!(plain("日", 1), ["日"], "a char wider than the line still lands once");
    }

    #[test]
    fn styles_follow_words_across_breaks() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let lines = wrap_line(&[Span::raw("see "), Span::styled("bold words", bold)], 8);
        assert_eq!(lines[0].spans, [Span::raw("see "), Span::styled("bold", bold)]);
        assert_eq!(lines[1].spans, [Span::styled("words", bold)]);
    }
}
