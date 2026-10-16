//! The conversation screen: transcript, notes panel, input with slash
//! commands, approvals, and past conversations.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use muninn_core::{
    CancellationToken, Conversation, Decision, Kind, Librarian, NoteEvent, NoteKind, Paths, Proposal, Review, Reviewer,
    Settings, Stop, Store, Surface, Update,
};
use muninn_tui::ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use muninn_tui::ratatui::layout::{Alignment, Constraint, Layout, Rect};
use muninn_tui::ratatui::style::{Color, Modifier, Style};
use muninn_tui::ratatui::text::{Line, Span};
use muninn_tui::ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
use muninn_tui::ratatui::Frame;
use muninn_tui::{centered, diff_lines, ChatLog, Input, Item, Picker};
use tokio::sync::oneshot;

use super::commands::{self, Parsed, COMMANDS};
use super::model::{self, ModelChoice, Outcome};
use super::{is_ctrl, tilde, AppEvent, Tx};
use crate::cli;

const KICKOFF: &str = "Hi Muninn! Let's set up this vault.";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ACCENT: Color = Color::Magenta;

/// What the app should do after a key press.
pub enum Action {
    None,
    Quit,
    /// Leave the conversation for setup and onboarding.
    Setup,
}

struct Approval {
    proposal: Proposal,
    reply: oneshot::Sender<Decision>,
    scroll: u16,
}

pub struct Chat {
    librarian: Arc<Librarian>,
    paths: Paths,
    store: Store,
    /// Provider and model, for the header.
    model: String,
    model_choice: Option<ModelChoice>,
    tx: Tx,
    /// `None` while a turn owns it.
    conversation: Option<Conversation>,
    title: String,
    log: ChatLog,
    notes: Vec<NoteEvent>,
    input: Input,
    /// Highlighted entry of the slash-command list.
    command_choice: usize,
    /// Set while a turn runs.
    running: Option<CancellationToken>,
    streaming: bool,
    quit_after_turn: bool,
    approval: Option<Approval>,
    history: Option<(Picker, Vec<Conversation>)>,
    frame: usize,
}

impl Chat {
    /// `onboarding` starts an onboarding conversation and sends the first message.
    pub fn new(paths: &Paths, settings: &Settings, onboarding: bool, tx: Tx) -> Result<Self> {
        let librarian = Arc::new(Librarian::from_settings(settings, paths)?);
        let kind = if onboarding { Kind::Onboarding } else { Kind::Chat };
        let mut conversation = Conversation::new(Surface::Tui, kind, librarian.vault().root());
        if onboarding {
            conversation.title = "Onboarding".into();
        }
        let mut chat = Self {
            librarian,
            paths: paths.clone(),
            store: Store::new(paths.conversations_dir()),
            model: model_label(settings),
            model_choice: None,
            tx,
            conversation: Some(conversation),
            title: String::new(),
            log: ChatLog::default(),
            notes: Vec::new(),
            input: Input::default(),
            command_choice: 0,
            running: None,
            streaming: false,
            quit_after_turn: false,
            approval: None,
            history: None,
            frame: 0,
        };
        if onboarding {
            chat.send(KICKOFF.into());
        } else {
            chat.welcome();
        }
        Ok(chat)
    }

    fn welcome(&mut self) {
        self.info(format!(
            "Save something, ask about your notes in `{}`, or just talk.  \nType `/` for commands, `/help` for keys.",
            tilde(self.librarian.vault().root())
        ));
    }

    /// Shows Markdown from Muninn itself (not the model) in the transcript.
    fn info(&mut self, markdown: impl Into<String>) {
        self.log.push(Item::new(markdown, dim()).marker("  ", dim()).markdown());
        self.log.scroll_down(usize::MAX);
    }

    fn error(&mut self, text: impl Into<String>) {
        let red = Style::default().fg(Color::Red);
        self.log.push(Item::new(text, red).marker("✗ ", red));
        self.log.scroll_down(usize::MAX);
    }

    fn send(&mut self, text: String) {
        let Some(mut conversation) = self.conversation.take() else { return };
        self.log.push(you(&text));
        self.log.scroll_down(usize::MAX);
        self.streaming = false;
        let cancel = CancellationToken::new();
        self.running = Some(cancel.clone());
        let (librarian, tx) = (self.librarian.clone(), self.tx.clone());
        let review = Review::Interactive(Arc::new(ChannelReviewer(tx.clone())));
        tokio::spawn(async move {
            let updates = tx.clone();
            let mut on_update = move |u| {
                let _ = updates.send(AppEvent::Update(u));
            };
            let result = librarian.turn(&mut conversation, &text, review, &mut on_update, &cancel).await;
            let _ = tx.send(AppEvent::TurnDone(Box::new(conversation), result.map_err(|e| format!("{e:#}"))));
        });
    }

    /// Returns true when the app should quit.
    pub fn on_event(&mut self, event: AppEvent) -> bool {
        match event {
            AppEvent::Update(Update::Text(delta)) => {
                if !self.streaming {
                    self.log.push(muninn(""));
                    self.streaming = true;
                }
                if let Some(item) = self.log.items.last_mut() {
                    item.body.push_str(&delta);
                }
            }
            AppEvent::Update(Update::Tool { name, detail }) => {
                self.streaming = false;
                self.log.push(tool(&name, &detail));
            }
            AppEvent::Update(Update::Recheck(paths)) => {
                self.streaming = false;
                self.info(format!(
                    "↻ cited {} without reading; checking again. This answer replaces the one above.",
                    paths.join(", ")
                ));
            }
            AppEvent::Update(Update::Note(note)) => {
                if note.kind == NoteKind::Changed {
                    let green = Style::default().fg(Color::Green);
                    self.log.push(Item::new(format!("✎ saved {}", note.path), green).marker("  ", green).compact());
                }
                self.notes.push(note);
            }
            AppEvent::Review(proposal, reply) => self.approval = Some(Approval { proposal, reply, scroll: 0 }),
            AppEvent::TurnDone(conversation, result) => {
                self.running = None;
                self.streaming = false;
                match result {
                    Ok(r) if r.stop == Stop::Cancelled => self.info("*cancelled*"),
                    Ok(r) if r.stop == Stop::StepLimit => self.info("*stopped after too many steps*"),
                    Ok(_) => {}
                    Err(e) => self.error(format!("Error: {e}")),
                }
                self.title = conversation.title.clone();
                if let Err(e) = self.store.save(&conversation) {
                    self.error(format!("Could not save the conversation: {e:#}"));
                }
                self.conversation = Some(*conversation);
                return self.quit_after_turn;
            }
            event @ (AppEvent::ModelChecked(_) | AppEvent::Models(_)) => {
                if let Some(choice) = &mut self.model_choice {
                    let outcome = choice.on_event(event);
                    self.model_outcome(outcome);
                }
            }
        }
        false
    }

    fn model_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pending => {}
            Outcome::Cancel => self.model_choice = None,
            Outcome::Done(settings) => {
                self.model_choice = None;
                let switched =
                    settings.save(&self.paths).and_then(|()| Librarian::from_settings(&settings, &self.paths));
                match switched {
                    Ok(librarian) => {
                        self.librarian = Arc::new(librarian);
                        self.model = model_label(&settings);
                        self.info(format!("Now using `{}`.", self.model));
                    }
                    Err(e) => self.error(format!("Could not switch the model: {e:#}")),
                }
            }
        }
    }

    pub fn handle(&mut self, event: Event) -> Action {
        if let Some(choice) = &mut self.model_choice {
            match event {
                Event::Paste(text) => choice.handle_paste(&text),
                Event::Key(key) if is_ctrl(&key, 'c') => self.model_choice = None,
                Event::Key(key) => {
                    let outcome = choice.handle_key(key, &self.tx);
                    self.model_outcome(outcome);
                }
                _ => {}
            }
            return Action::None;
        }
        let key = match event {
            Event::Paste(text) if self.approval.is_none() && self.history.is_none() => {
                self.input.insert_str(&text);
                self.command_choice = 0;
                return Action::None;
            }
            Event::Key(key) => key,
            _ => return Action::None,
        };
        if self.approval.is_some() {
            self.handle_approval(key);
            return Action::None;
        }
        if self.history.is_some() {
            self.handle_history(key);
            return Action::None;
        }
        if is_ctrl(&key, 'c') {
            return self.run(commands::Action::Quit, "");
        }
        if let Some(action) = self.handle_completion(key) {
            return action;
        }
        match key.code {
            KeyCode::Esc => {
                if let Some(cancel) = &self.running {
                    cancel.cancel();
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => return self.submit(),
            KeyCode::PageUp => self.log.scroll_up(10),
            KeyCode::PageDown => self.log.scroll_down(10),
            _ if is_ctrl(&key, 'n') => return self.run(commands::Action::New, ""),
            _ if is_ctrl(&key, 'o') => return self.run(commands::Action::Resume, ""),
            _ => {
                if self.input.handle_key(key) {
                    self.command_choice = 0;
                } else {
                    match key.code {
                        KeyCode::Up => self.log.scroll_up(3),
                        KeyCode::Down => self.log.scroll_down(3),
                        _ => {}
                    }
                }
            }
        }
        Action::None
    }

    /// Keys for the slash-command list while it is open; `None` when the key
    /// is not for the list.
    fn handle_completion(&mut self, key: KeyEvent) -> Option<Action> {
        let matches = commands::completions(self.input.text());
        if matches.is_empty() {
            return None;
        }
        let choice = self.command_choice.min(matches.len() - 1);
        match key.code {
            KeyCode::Up => self.command_choice = (choice + matches.len() - 1) % matches.len(),
            KeyCode::Down => self.command_choice = (choice + 1) % matches.len(),
            KeyCode::Tab => self.input.set_text(format!("/{} ", matches[choice].name)),
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.input.take();
                return Some(self.run(matches[choice].action, ""));
            }
            KeyCode::Esc => {
                self.input.take();
            }
            _ => return None,
        }
        Some(Action::None)
    }

    fn submit(&mut self) -> Action {
        let text = self.input.text().trim().to_string();
        if text.is_empty() {
            return Action::None;
        }
        match commands::parse(&text) {
            Some(Parsed::Run(action, args)) => {
                let args = args.to_string();
                self.input.take();
                self.run(action, &args)
            }
            Some(Parsed::Unknown(name)) => {
                self.error(format!("Unknown command /{name}. Type /help for the list."));
                Action::None
            }
            None if self.running.is_none() => {
                let text = self.input.take();
                self.send(text);
                Action::None
            }
            None => Action::None,
        }
    }

    fn run(&mut self, action: commands::Action, args: &str) -> Action {
        use commands::Action as C;
        let busy = self.running.is_some();
        match action {
            C::Quit if busy => {
                if let Some(cancel) = &self.running {
                    cancel.cancel();
                }
                self.quit_after_turn = true;
            }
            C::Quit => return Action::Quit,
            C::Help => self.info(help()),
            C::Log => self.show_log(),
            C::New | C::Resume | C::Undo | C::Model | C::Init if busy => {
                self.info("Muninn is still working; wait for it or press `Esc` to cancel.")
            }
            C::New => {
                self.reset(None);
                self.welcome();
            }
            C::Resume => self.open_history(),
            C::Undo => self.undo(args),
            C::Model => match Settings::load(&self.paths) {
                Ok(settings) => self.model_choice = Some(ModelChoice::new(settings)),
                Err(e) => self.error(format!("Could not read the settings: {e:#}")),
            },
            C::Init => return Action::Setup,
        }
        Action::None
    }

    fn show_log(&mut self) {
        let changes = match cli::changes(self.librarian.vault(), self.librarian.journal()) {
            Ok(changes) => changes,
            Err(e) => return self.error(format!("Could not read the change log: {e:#}")),
        };
        if changes.is_empty() {
            return self.info("No changes yet.");
        }
        let mut table = String::from("| Id | When | Note | Summary |\n|---|---|---|---|\n");
        for (entry, undone) in changes.iter().take(20) {
            let summary =
                if *undone { format!("~~{}~~ *(undone)*", cell(&entry.summary)) } else { cell(&entry.summary) };
            table.push_str(&format!(
                "| `{}` | {} | {} | {summary} |\n",
                entry.id,
                entry.time.format("%Y-%m-%d %H:%M"),
                cell(&entry.path)
            ));
        }
        table.push_str("\n`/undo` reverts the latest change, `/undo <id>` a specific one.");
        self.info(table);
    }

    fn undo(&mut self, id: &str) {
        let id = (!id.is_empty()).then_some(id);
        match self.librarian.journal().undo(self.librarian.vault(), id) {
            Ok(undo @ muninn_vault::Undo::Reverted { .. }) => self.info(cli::describe_undo(&undo)),
            Ok(undo) => self.error(cli::describe_undo(&undo)),
            Err(e) => self.error(format!("Nothing undone: {e:#}")),
        }
    }

    fn handle_approval(&mut self, key: KeyEvent) {
        let approval = self.approval.as_mut().expect("approval");
        let decision = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Decision::Approve,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Decision::Reject(String::new()),
            KeyCode::Up => return approval.scroll = approval.scroll.saturating_sub(1),
            KeyCode::Down => return approval.scroll = approval.scroll.saturating_add(1),
            KeyCode::PageUp => return approval.scroll = approval.scroll.saturating_sub(10),
            KeyCode::PageDown => return approval.scroll = approval.scroll.saturating_add(10),
            _ => return,
        };
        let approval = self.approval.take().expect("approval");
        let (verdict, color) =
            if decision == Decision::Approve { ("✓ approved", Color::Green) } else { ("✗ rejected", Color::Red) };
        let style = Style::default().fg(color);
        self.log.push(
            Item::new(format!("{verdict} change to {}", approval.proposal.path), style).marker("  ", style).compact(),
        );
        let _ = approval.reply.send(decision);
    }

    fn open_history(&mut self) {
        let conversations = self.store.list(self.librarian.vault().root()).unwrap_or_default();
        let items = conversations
            .iter()
            .map(|c| {
                let title = if c.title.is_empty() { "(untitled)" } else { &c.title };
                format!(
                    "{}  {:<4} {title}",
                    c.updated.format("%Y-%m-%d %H:%M"),
                    format!("{:?}", c.surface).to_lowercase()
                )
            })
            .collect();
        self.history = Some((Picker::new(items), conversations));
    }

    fn handle_history(&mut self, key: KeyEvent) {
        let (picker, conversations) = self.history.as_mut().expect("history");
        match key.code {
            KeyCode::Esc => self.history = None,
            KeyCode::Enter => {
                let chosen = picker.selected().and_then(|i| conversations.get(i).cloned());
                self.history = None;
                if let Some(conversation) = chosen {
                    self.reset(Some(conversation));
                }
            }
            _ => {
                picker.handle_key(key);
            }
        }
    }

    /// Starts a fresh conversation, or continues `conversation`.
    fn reset(&mut self, conversation: Option<Conversation>) {
        let conversation =
            conversation.unwrap_or_else(|| Conversation::new(Surface::Tui, Kind::Chat, self.librarian.vault().root()));
        self.log.clear();
        for message in &conversation.messages {
            use muninn_core::Role;
            match message.role {
                Role::User if muninn_core::is_automatic(message) => {}
                Role::User => self.log.push(you(&message.content)),
                Role::Assistant => {
                    if !message.content.trim().is_empty() {
                        self.log.push(muninn(&message.content));
                    }
                    for call in &message.tool_calls {
                        let args = serde_json::from_str(&call.arguments).unwrap_or_default();
                        self.log.push(tool(&call.name, &muninn_core::tool_detail(&args)));
                    }
                }
                Role::Tool => {}
            }
        }
        self.title = conversation.title.clone();
        self.notes = conversation.notes.clone();
        self.conversation = Some(conversation);
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        self.frame = self.frame.wrapping_add(1);
        let area = frame.area();
        let input_height = (self.input.height(area.width.saturating_sub(4)) + 2).min(10);
        let [header, body, input, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(area);

        self.draw_header(frame, Rect { height: 1, ..header });

        let (log_area, notes_area) = if body.width >= 100 {
            let [log, notes] = Layout::horizontal([Constraint::Min(40), Constraint::Length(34)]).areas(body);
            (log, Some(notes))
        } else {
            (body, None)
        };
        let log_inner = Rect { x: log_area.x + 1, width: log_area.width.saturating_sub(3), ..log_area };
        self.log.render(frame, log_inner);
        if let Some(area) = notes_area {
            self.draw_notes(frame, area);
        }

        let busy = self.running.is_some();
        let border = if busy { dim() } else { Style::default().fg(ACCENT) };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .padding(Padding::horizontal(1));
        let text_area = block.inner(input);
        self.input.render(frame, input, block);
        if self.input.text().is_empty() {
            let placeholder =
                if busy { "Muninn is working… Esc cancels" } else { "Message Muninn · / for commands" };
            frame.render_widget(Paragraph::new(Span::styled(placeholder, dim())), text_area);
        }
        frame.render_widget(Paragraph::new(self.footer(footer.width)), footer);

        self.draw_completions(frame, input);
        if let Some(approval) = &self.approval {
            draw_approval(frame, approval);
        }
        if let Some((picker, _)) = &mut self.history {
            let popup = centered(area, area.width.saturating_sub(10).min(100), area.height.saturating_sub(6).min(24));
            frame.render_widget(Clear, popup);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT))
                .title(Span::styled(" Continue a conversation ", Style::default().add_modifier(Modifier::BOLD)))
                .title_bottom(Line::from(hints(&[("↑↓", "select"), ("enter", "open"), ("esc", "close")])));
            if picker.items.is_empty() {
                frame.render_widget(Paragraph::new(" No saved conversations yet.").block(block), popup);
            } else {
                picker.render(frame, popup, block);
            }
        }
        if let Some(choice) = &mut self.model_choice {
            let popup = centered(area, area.width.saturating_sub(10).min(90), area.height.saturating_sub(6).min(26));
            frame.render_widget(Clear, popup);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT))
                .padding(Padding::horizontal(1))
                .title(model::title("Choose a model"))
                .title_bottom(Span::styled(choice.hints(), dim()));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            choice.draw(frame, inner);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = if self.title.is_empty() { "New conversation" } else { &self.title };
        let badge = Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD);
        let right = Line::from(vec![
            Span::styled(tilde(self.librarian.vault().root()), Style::default().fg(Color::Cyan)),
            Span::styled("  ·  ", dim()),
            Span::styled(format!("{} ", self.model), dim()),
        ]);
        // The title gets what is left beside the badge and the right side.
        let room = (area.width as usize).saturating_sub(right.width() + " Muninn ".len() + 4);
        let left = Line::from(vec![
            Span::styled(" Muninn ", badge),
            Span::raw("  "),
            Span::styled(fit(title, room), Style::default().add_modifier(Modifier::BOLD)),
        ]);
        frame.render_widget(Paragraph::new(left), area);
        frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }

    fn footer(&self, width: u16) -> Line<'static> {
        if self.running.is_some() {
            let spinner = SPINNER[self.frame % SPINNER.len()];
            let mut spans = vec![Span::styled(format!(" {spinner} working "), Style::default().fg(ACCENT))];
            spans.extend(hints(&[("esc", "cancel"), ("ctrl+c", "quit")]));
            return Line::from(spans);
        }
        // Most useful first; the tail is dropped on narrow terminals.
        let mut pairs = vec![
            ("enter", "send"),
            ("/", "commands"),
            ("ctrl+enter", "newline"),
            ("ctrl+c", "quit"),
            ("ctrl+o", "conversations"),
            ("pgup/pgdn", "scroll"),
        ];
        let mut line = Line::from(hints(&pairs));
        while line.width() > width as usize && pairs.len() > 1 {
            pairs.pop();
            line = Line::from(hints(&pairs));
        }
        line
    }

    /// The slash-command list, just above the input.
    fn draw_completions(&self, frame: &mut Frame, input: Rect) {
        let matches = commands::completions(self.input.text());
        if matches.is_empty() || self.approval.is_some() || self.history.is_some() || self.model_choice.is_some() {
            return;
        }
        let choice = self.command_choice.min(matches.len() - 1);
        let height = (matches.len() as u16 + 2).min(input.y);
        let area = Rect { y: input.y - height, height, ..input };
        let name_width = COMMANDS.iter().map(|c| c.name.len() + c.args.len() + 4).max().unwrap_or(0);
        let lines: Vec<Line> = matches
            .iter()
            .enumerate()
            .map(|(i, command)| {
                let selected = i == choice;
                let name = format!("/{} {}", command.name, command.args);
                let name_style =
                    if selected { Style::default().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::default() };
                Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, name_style),
                    Span::styled(format!("{name:<name_width$}"), name_style),
                    Span::styled(command.about, dim()),
                ])
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(dim())
            .title_bottom(Line::from(hints(&[("↑↓", "select"), ("tab", "complete"), ("enter", "run")])));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_notes(&self, frame: &mut Frame, area: Rect) {
        let mut seen: Vec<(&str, NoteKind)> = Vec::new();
        for note in self.notes.iter().rev().filter(|n| n.kind != NoteKind::Found) {
            if !seen.iter().any(|(p, _)| *p == note.path) {
                seen.push((&note.path, note.kind));
            }
        }
        let lines: Vec<Line> = if seen.is_empty() {
            vec![Line::from(Span::styled("Nothing read or changed yet.", dim()))]
        } else {
            seen.iter()
                .map(|(path, kind)| {
                    let (mark, style) = match kind {
                        NoteKind::Read | NoteKind::Found => ("·", dim()),
                        NoteKind::Changed => ("✎", Style::default().fg(Color::Green)),
                        NoteKind::Proposed => ("?", Style::default().fg(Color::Yellow)),
                    };
                    Line::from(vec![Span::styled(format!("{mark} "), style), Span::styled(path.to_string(), style)])
                })
                .collect()
        };
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(dim())
            .title(Span::styled(" Notes ", Style::default().add_modifier(Modifier::BOLD)))
            .padding(Padding::horizontal(1));
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
    }
}

fn draw_approval(frame: &mut Frame, approval: &Approval) {
    let area = frame.area();
    let popup = centered(area, area.width.saturating_sub(6).min(120), area.height.saturating_sub(4));
    frame.render_widget(Clear, popup);
    let proposal = &approval.proposal;
    let yellow = Style::default().fg(Color::Yellow);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(yellow)
        .title(Span::styled(format!(" Approve change to {}? ", proposal.path), yellow.add_modifier(Modifier::BOLD)))
        .title_bottom(Line::from(hints(&[("y", "approve"), ("n", "reject"), ("↑↓", "scroll")])))
        .padding(Padding::horizontal(1));
    let mut lines = vec![
        Line::from(Span::styled(proposal.summary.clone(), Style::default().add_modifier(Modifier::BOLD))),
        Line::default(),
    ];
    lines.extend(diff_lines(&proposal.diff()));
    frame.render_widget(Paragraph::new(lines).block(block).scroll((approval.scroll, 0)), popup);
}

/// `text` cut to `width` columns, ending in `…` when cut.
fn fit(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        used += c.width().unwrap_or(0);
        if used + 1 > width {
            break;
        }
        out.push(c);
    }
    out + "…"
}

fn model_label(settings: &Settings) -> String {
    format!("{} · {}", settings.provider.id(), settings.model)
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// ` key description · key description ` with the keys bright.
fn hints(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, description)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(key.to_string(), Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {description}"), dim()));
    }
    spans.push(Span::raw(" "));
    spans
}

fn help() -> String {
    let mut text = String::from("**Commands**\n\n| Command | What it does |\n|---|---|\n");
    for command in COMMANDS {
        let usage = if command.args.is_empty() {
            command.name.to_string()
        } else {
            format!("{} {}", command.name, command.args)
        };
        text.push_str(&format!("| `/{usage}` | {} |\n", command.about));
    }
    text.push_str(
        "\n**Keys**\n\n\
| Key | What it does |\n|---|---|\n\
| Enter | Send |\n\
| Ctrl+Enter, Shift+Enter, Alt+Enter, Ctrl+J | New line |\n\
| Ctrl+← →, Alt+← → | Move by word |\n\
| Home, End, Ctrl+A, Ctrl+E, Cmd+← → | Start or end of the line |\n\
| Ctrl+Home, Ctrl+End, Cmd+↑ ↓ | Start or end of the message |\n\
| Ctrl+Backspace, Alt+Backspace, Ctrl+W | Delete the word before the cursor |\n\
| Ctrl+Delete, Alt+D | Delete the word after the cursor |\n\
| Ctrl+U, Ctrl+K | Delete to the start or end of the line |\n\
| ↑ ↓, PgUp, PgDn | Scroll the conversation |\n\
| Ctrl+N, Ctrl+O | New conversation, continue an earlier one |\n\
| Esc | Cancel Muninn's turn |\n\
| Ctrl+C | Quit |\n\
\nCtrl+Enter, Shift+Enter and Cmd need a terminal with the kitty keyboard protocol \
(kitty, Ghostty, WezTerm, foot, iTerm2); Alt+Enter and Ctrl+J work everywhere.",
    );
    text
}

/// Text safe to put in a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// Hands approvals to the UI loop and waits for the user's answer.
struct ChannelReviewer(Tx);

#[async_trait]
impl Reviewer for ChannelReviewer {
    async fn review(&self, proposal: &Proposal) -> Decision {
        let (reply, answer) = oneshot::channel();
        if self.0.send(AppEvent::Review(proposal.clone(), reply)).is_err() {
            return Decision::Reject("the user interface closed".into());
        }
        answer.await.unwrap_or_else(|_| Decision::Reject("the review was interrupted".into()))
    }
}

fn you(text: &str) -> Item {
    let cyan = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    Item::new(text, Style::default()).marker("❯ ", cyan)
}

fn muninn(text: &str) -> Item {
    Item::new(text, Style::default()).marker("● ", Style::default().fg(ACCENT)).markdown()
}

fn tool(name: &str, detail: &str) -> Item {
    Item::new(format!("↳ {name} {detail}"), dim()).marker("  ", dim()).compact()
}
