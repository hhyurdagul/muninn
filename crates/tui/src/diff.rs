use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Colors a unified diff: additions green, removals red, hunks cyan.
pub fn diff_lines(unified: &str) -> Vec<Line<'static>> {
    unified
        .lines()
        .map(|line| {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                Style::default().add_modifier(Modifier::BOLD)
            } else if line.starts_with('+') {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect()
}
