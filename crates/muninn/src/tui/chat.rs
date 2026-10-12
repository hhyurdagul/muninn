//! The conversation screen: transcript, notes panel, input, approvals, and
//! past conversations.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use muninn_core::{
    CancellationToken, Conversation, Decision, Kind, Librarian, NoteEvent, NoteKind, Paths, Proposal, Review,
    Reviewer, Settings, Stop, Store, Surface, Update,
};
use muninn_tui::ratatui::crossterm::event::{Event, KeyCode, KeyEvent};
use muninn_tui::ratatui::layout::{Constraint, Layout, Rect};
use muninn_tui::ratatui::style::{Color, Modifier, Style};
use muninn_tui::ratatui::text::{Line, Span};
use muninn_tui::ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use muninn_tui::ratatui::Frame;
use muninn_tui::{centered, diff_lines, ChatLog, Input, Item, Picker};
use tokio::sync::oneshot;

use super::{is_ctrl, tilde, AppEvent, Tx};

const KICKOFF: &str = "Hi Muninn! Let's set up this vault.";
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

struct Approval {
    proposal: Proposal,
    reply: oneshot::Sender<Decision>,
    scroll: u16,
}

pub struct Chat {
    librarian: Arc<Librarian>,
    store: Store,
    model: String,
    tx: Tx,
    /// `None` while a turn owns it.
    conversation: Option<Conversation>,
    title: String,
    log: ChatLog,
    notes: Vec<NoteEvent>,
    input: Input,
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
        let conversation = Conversation::new(Surface::Tui, kind, librarian.vault().root());
        let mut chat = Self {
            librarian,
            store: Store::new(paths.conversations_dir()),
            model: settings.model.clone(),
            tx,
            conversation: Some(conversation),
            title: String::new(),
            log: ChatLog::default(),
            notes: Vec::new(),
            input: Input::default(),
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
            chat.log.push(Item::new(
                "Save something, ask about your notes, or just talk. Ctrl+O continues an earlier conversation.",
                dim(),
            ));
        }
        Ok(chat)
    }

    fn send(&mut self, text: String) {
        let Some(mut conversation) = self.conversation.take() else { return };
        self.log.push(Item::labeled("you", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD), text.clone(), Style::default()));
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
                    self.log.push(Item::labeled("muninn", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD), "", Style::default()));
                    self.streaming = true;
                }
                if let Some(item) = self.log.items.last_mut() {
                    item.body.push_str(&delta);
                }
            }
            AppEvent::Update(Update::Tool { name, detail }) => {
                self.streaming = false;
                self.log.push(Item::new(format!("  ↳ {name} {detail}"), dim()).compact());
            }
            AppEvent::Update(Update::Note(note)) => {
                if note.kind == NoteKind::Changed {
                    self.log.push(Item::new(format!("  ✎ saved {}", note.path), Style::default().fg(Color::Green)).compact());
                }
                self.notes.push(note);
            }
            AppEvent::Review(proposal, reply) => self.approval = Some(Approval { proposal, reply, scroll: 0 }),
            AppEvent::TurnDone(conversation, result) => {
                self.running = None;
                self.streaming = false;
                match result {
                    Ok(r) if r.stop == Stop::Cancelled => self.log.push(Item::new("(cancelled)", dim())),
                    Ok(r) if r.stop == Stop::StepLimit => self.log.push(Item::new("(stopped after too many steps)", dim())),
                    Ok(_) => {}
                    Err(e) => self.log.push(Item::new(format!("Error: {e}"), Style::default().fg(Color::Red))),
                }
                self.title = conversation.title.clone();
                if let Err(e) = self.store.save(&conversation) {
                    self.log.push(Item::new(format!("Could not save the conversation: {e:#}"), Style::default().fg(Color::Red)));
                }
                self.conversation = Some(*conversation);
                return self.quit_after_turn;
            }
            AppEvent::ModelChecked(_) => {}
        }
        false
    }

    /// Returns true when the app should quit.
    pub fn handle(&mut self, event: Event) -> bool {
        let key = match event {
            Event::Paste(text) if self.approval.is_none() && self.history.is_none() => {
                self.input.insert_str(&text);
                return false;
            }
            Event::Key(key) => key,
            _ => return false,
        };
        if self.approval.is_some() {
            self.handle_approval(key);
            return false;
        }
        if self.history.is_some() {
            self.handle_history(key);
            return false;
        }
        if is_ctrl(&key, 'c') {
            let Some(cancel) = &self.running else { return true };
            cancel.cancel();
            self.quit_after_turn = true;
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                if let Some(cancel) = &self.running {
                    cancel.cancel();
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if self.running.is_none() && !self.input.text().trim().is_empty() {
                    let text = self.input.take();
                    self.send(text);
                }
            }
            KeyCode::PageUp => self.log.scroll_up(10),
            KeyCode::PageDown => self.log.scroll_down(10),
            _ if is_ctrl(&key, 'n') && self.running.is_none() => self.reset(None),
            _ if is_ctrl(&key, 'o') && self.running.is_none() => self.open_history(),
            _ => {
                if !self.input.handle_key(key) {
                    match key.code {
                        KeyCode::Up => self.log.scroll_up(3),
                        KeyCode::Down => self.log.scroll_down(3),
                        _ => {}
                    }
                }
            }
        }
        false
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
        let verdict = if decision == Decision::Approve { "approved" } else { "rejected" };
        self.log.push(Item::new(format!("  {verdict} change to {}", approval.proposal.path), dim()).compact());
        let _ = approval.reply.send(decision);
    }

    fn open_history(&mut self) {
        let conversations = self.store.list(self.librarian.vault().root()).unwrap_or_default();
        let items = conversations
            .iter()
            .map(|c| {
                let title = if c.title.is_empty() { "(untitled)" } else { &c.title };
                format!("{}  {:<4} {title}", c.updated.format("%Y-%m-%d %H:%M"), format!("{:?}", c.surface).to_lowercase())
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
        let conversation = conversation
            .unwrap_or_else(|| Conversation::new(Surface::Tui, Kind::Chat, self.librarian.vault().root()));
        self.log.clear();
        for message in &conversation.messages {
            use muninn_core::Role;
            match message.role {
                Role::User => self.log.push(Item::labeled("you", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD), message.content.clone(), Style::default())),
                Role::Assistant => {
                    if !message.content.trim().is_empty() {
                        self.log.push(Item::labeled("muninn", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD), message.content.clone(), Style::default()));
                    }
                    for call in &message.tool_calls {
                        let args: serde_json::Value = serde_json::from_str(&call.arguments).unwrap_or_default();
                        let detail = ["path", "query", "change_id"].iter().find_map(|k| args.get(*k).and_then(|v| v.as_str())).unwrap_or("");
                        self.log.push(Item::new(format!("  ↳ {} {detail}", call.name), dim()).compact());
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
        let input_height = (self.input.height(area.width.saturating_sub(2)) + 2).min(10);
        let [header, body, input, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(area);

        let title = if self.title.is_empty() { "new conversation" } else { &self.title };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Muninn ", Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)),
                Span::styled(format!("  {}  ·  {}  ·  {title}", tilde(self.librarian.vault().root()), self.model), dim()),
            ])),
            header,
        );

        let (log_area, notes_area) = if body.width >= 100 {
            let [log, notes] = Layout::horizontal([Constraint::Min(40), Constraint::Length(34)]).areas(body);
            (log, Some(notes))
        } else {
            (body, None)
        };
        let log_inner = Rect { x: log_area.x + 1, width: log_area.width.saturating_sub(2), ..log_area };
        self.log.render(frame, log_inner);
        if let Some(area) = notes_area {
            self.draw_notes(frame, area);
        }

        let busy = self.running.is_some();
        let input_title = if busy { " waiting for muninn… " } else { " message " };
        self.input.render(frame, input, Block::default().borders(Borders::ALL).title(input_title).border_style(dim()));

        let status = if busy {
            format!(" {} working · Esc cancel · Ctrl+C quit", SPINNER[self.frame / 2 % SPINNER.len()])
        } else {
            " Enter send · Alt+Enter newline · PgUp/PgDn scroll · Ctrl+O conversations · Ctrl+N new · Ctrl+C quit".into()
        };
        frame.render_widget(Paragraph::new(Span::styled(status, dim())), footer);

        if let Some(approval) = &self.approval {
            draw_approval(frame, approval);
        }
        if let Some((picker, _)) = &mut self.history {
            let popup = centered(area, area.width.saturating_sub(10).min(100), area.height.saturating_sub(6).min(24));
            frame.render_widget(Clear, popup);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Continue a conversation · Enter open · Esc close ");
            if picker.items.is_empty() {
                frame.render_widget(Paragraph::new("No saved conversations yet.").block(block), popup);
            } else {
                picker.render(frame, popup, block);
            }
        }
    }

    fn draw_notes(&self, frame: &mut Frame, area: Rect) {
        let mut seen: Vec<(&str, NoteKind)> = Vec::new();
        for note in self.notes.iter().rev() {
            if !seen.iter().any(|(p, _)| *p == note.path) {
                seen.push((&note.path, note.kind));
            }
        }
        let lines: Vec<Line> = seen
            .iter()
            .map(|(path, kind)| {
                let (mark, style) = match kind {
                    NoteKind::Read => ("·", dim()),
                    NoteKind::Changed => ("✎", Style::default().fg(Color::Green)),
                    NoteKind::Proposed => ("?", Style::default().fg(Color::Yellow)),
                };
                Line::from(Span::styled(format!("{mark} {path}"), style))
            })
            .collect();
        let block = Block::default().borders(Borders::LEFT).border_style(dim()).title(" notes ");
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
    }
}

fn draw_approval(frame: &mut Frame, approval: &Approval) {
    let area = frame.area();
    let popup = centered(area, area.width.saturating_sub(6).min(120), area.height.saturating_sub(4));
    frame.render_widget(Clear, popup);
    let proposal = &approval.proposal;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" Approve change to {}? ", proposal.path))
        .title_bottom(" y approve · n reject · ↑↓ scroll ");
    let mut lines = vec![Line::from(Span::styled(proposal.summary.clone(), Style::default().add_modifier(Modifier::BOLD))), Line::default()];
    lines.extend(diff_lines(&proposal.diff()));
    frame.render_widget(Paragraph::new(lines).block(block).scroll((approval.scroll, 0)), popup);
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
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
