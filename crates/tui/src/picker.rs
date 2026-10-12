use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState};
use ratatui::Frame;

/// A selectable list with optional checkboxes.
pub struct Picker {
    pub items: Vec<String>,
    /// `Some` turns the picker into a multi-select with checkboxes.
    pub checked: Option<Vec<bool>>,
    state: ListState,
}

impl Picker {
    pub fn new(items: Vec<String>) -> Self {
        let mut state = ListState::default();
        state.select((!items.is_empty()).then_some(0));
        Self { items, checked: None, state }
    }

    pub fn with_checks(mut self, checked: Vec<bool>) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Handles Up/Down (and Space to toggle a checkbox); returns whether the
    /// key was used.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let len = self.items.len();
        if len == 0 {
            return false;
        }
        let current = self.state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.state.select(Some((current + len - 1) % len)),
            KeyCode::Down | KeyCode::Char('j') => self.state.select(Some((current + 1) % len)),
            KeyCode::Char(' ') => match self.checked.as_mut() {
                Some(checked) => checked[current] = !checked[current],
                None => return false,
            },
            _ => return false,
        }
        true
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| match &self.checked {
                Some(checked) => ListItem::new(format!("[{}] {item}", if checked[i] { "x" } else { " " })),
                None => ListItem::new(item.as_str()),
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, area, &mut self.state);
    }
}
