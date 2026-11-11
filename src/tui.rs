//! The full-screen terminal UI, on ratatui + crossterm like `sts`:
//!
//!   dashboard (`prk` in a terminal): the review in progress with its step and a
//!     time bar, the queue, recent results, and the log, refreshed live.
//!   history (`prk -H` in a terminal, or `H` from the dashboard): every job,
//!     searchable, with each review's details and comments a key away.
//!
//! Only entered when stdout is a terminal; a service or a pipe gets plain lines.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::activity;
use crate::db::{self, Db, Enqueued, HistoryFilter, Job};
use crate::stop::Stop;
use crate::view;

const TICK: Duration = Duration::from_millis(120);
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How long a just-finished review stays highlighted in Recent.
const FLASH_SECS: i64 = 4;

fn status_color(status: &str) -> Color {
    match status {
        db::REVIEWED => Color::Green,
        db::SKIPPED => Color::Yellow,
        db::FAILED => Color::Red,
        db::RUNNING => Color::Cyan,
        _ => Color::Blue,
    }
}

fn glyph(status: &str) -> &'static str {
    view::status_look(status).0
}

fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(Color::DarkGray))
}

fn key_hint(k: &str, what: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!(" {k} "),
            Style::new().fg(Color::Black).bg(Color::Gray),
        ),
        Span::raw(format!(" {what} ")),
    ]
}

fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
}

/// Open a URL in the browser: Windows, or WSL first, then Linux, then macOS.
fn open_url(url: &str) -> bool {
    let openers: &[&str] = if cfg!(windows) {
        &["explorer"]
    } else {
        &["wslview", "xdg-open", "open"]
    };
    openers.iter().any(|opener| {
        std::process::Command::new(opener)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    })
}

/// What a confirmed `R` does.
#[derive(Clone)]
struct Rereview {
    owner: String,
    repo: String,
    number: u64,
    label: String,
}

enum Overlay {
    None,
    /// Details and comments of one job, scrolled by this many lines.
    Detail(i64, u16),
    Confirm(Rereview),
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Dashboard,
    History,
}

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Queue,
    Recent,
}

struct App {
    db: Arc<Db>,
    screen: Screen,
    /// The dashboard is live (a listener runs in this process).
    live: bool,
    channel: String,
    reviewer: String,
    timeout_secs: i64,
    focus: Focus,
    queue_state: ListState,
    recent_state: ListState,
    history_state: ListState,
    history_filter: HistoryFilter,
    search: Option<String>,
    search_committed: String,
    /// PR links being typed or pasted to queue (`a`, or paste anywhere).
    adding: Option<String>,
    overlay: Overlay,
    note: Option<(String, Instant)>,
    tick: usize,
    // refreshed from the database about twice a second
    waiting: Vec<Job>,
    running: Vec<Job>,
    recent: Vec<Job>,
    history: Vec<Job>,
    loaded: Instant,
}

impl App {
    fn new(db: Arc<Db>, screen: Screen, live: bool, filter: HistoryFilter) -> App {
        let mut app = App {
            db,
            screen,
            live,
            channel: String::new(),
            reviewer: String::new(),
            timeout_secs: 30 * 60,
            focus: Focus::Recent,
            queue_state: ListState::default(),
            recent_state: ListState::default().with_selected(Some(0)),
            history_state: ListState::default().with_selected(Some(0)),
            history_filter: filter,
            search: None,
            search_committed: String::new(),
            adding: None,
            overlay: Overlay::None,
            note: None,
            tick: 0,
            waiting: Vec::new(),
            running: Vec::new(),
            recent: Vec::new(),
            history: Vec::new(),
            loaded: Instant::now() - Duration::from_secs(10),
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        self.waiting = self.db.waiting();
        self.running = self.db.running();
        let finished = self.db.history(&HistoryFilter {
            limit: 40,
            ..Default::default()
        });
        self.recent = finished
            .into_iter()
            .filter(|j| j.status != db::WAITING && j.status != db::RUNNING)
            .take(12)
            .collect();
        let query = self.search_committed.to_lowercase();
        self.history = self
            .db
            .history(&self.history_filter)
            .into_iter()
            .filter(|j| {
                query.is_empty()
                    || format!(
                        "{} {} {} {} {}",
                        j.key(),
                        j.title,
                        j.status,
                        j.reason,
                        j.author
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        clamp(&mut self.queue_state, self.waiting.len());
        clamp(&mut self.recent_state, self.recent.len());
        clamp(&mut self.history_state, self.history.len());
        self.loaded = Instant::now();
    }

    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    /// The job the cursor is on, on whichever list has focus.
    fn selected(&self) -> Option<&Job> {
        match self.screen {
            Screen::History => self
                .history_state
                .selected()
                .and_then(|i| self.history.get(i)),
            Screen::Dashboard => match self.focus {
                Focus::Queue => self
                    .queue_state
                    .selected()
                    .and_then(|i| self.waiting.get(i)),
                Focus::Recent => self
                    .recent_state
                    .selected()
                    .and_then(|i| self.recent.get(i)),
            },
        }
    }

    fn list_state(&mut self) -> (&mut ListState, usize) {
        match self.screen {
            Screen::History => (&mut self.history_state, self.history.len()),
            Screen::Dashboard => match self.focus {
                Focus::Queue => (&mut self.queue_state, self.waiting.len()),
                Focus::Recent => (&mut self.recent_state, self.recent.len()),
            },
        }
    }

    fn move_by(&mut self, delta: i64) {
        let (state, len) = self.list_state();
        if len == 0 {
            return;
        }
        let at = state.selected().unwrap_or(0) as i64;
        state.select(Some((at + delta).clamp(0, len as i64 - 1) as usize));
    }

    /// Handle one key. False to quit.
    fn key(&mut self, k: KeyEvent, stop: &Stop) -> bool {
        let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return false;
        }
        // typing a search
        if let Some(query) = &mut self.search {
            match k.code {
                KeyCode::Enter => {
                    self.search_committed = std::mem::take(query);
                    self.search = None;
                    self.history_state.select(Some(0));
                    self.reload();
                }
                KeyCode::Esc => self.search = None,
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c) => query.push(c),
                _ => {}
            }
            return true;
        }
        // typing or pasting PR links
        if let Some(text) = &mut self.adding {
            match k.code {
                KeyCode::Enter => {
                    let text = std::mem::take(text);
                    self.adding = None;
                    self.add_prs(&text);
                }
                KeyCode::Esc => self.adding = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            }
            return true;
        }
        match &mut self.overlay {
            Overlay::Confirm(action) => {
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    let action = action.clone();
                    self.rereview(&action);
                }
                self.overlay = Overlay::None;
                return true;
            }
            Overlay::Detail(_, scroll) => {
                match k.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(15),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(15),
                    KeyCode::Char('o') => self.open_selected(),
                    _ => {}
                }
                return true;
            }
            Overlay::None => {}
        }
        match k.code {
            KeyCode::Char('q') => return false,
            KeyCode::Esc => {
                if self.screen == Screen::History && self.live {
                    self.screen = Screen::Dashboard;
                } else if !self.search_committed.is_empty() {
                    self.search_committed.clear();
                    self.reload();
                } else {
                    return false;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(-100_000),
            KeyCode::End | KeyCode::Char('G') => self.move_by(100_000),
            KeyCode::Tab if self.screen == Screen::Dashboard => {
                self.focus = if self.focus == Focus::Queue {
                    Focus::Recent
                } else {
                    Focus::Queue
                };
                if self.focus == Focus::Queue && self.queue_state.selected().is_none() {
                    self.queue_state.select(Some(0));
                }
            }
            KeyCode::Enter => {
                if let Some(job) = self.selected() {
                    self.overlay = Overlay::Detail(job.id, 0);
                }
            }
            KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('a') => self.adding = Some(String::new()),
            KeyCode::Char('R') => {
                if let Some(job) = self.selected() {
                    if job.status == db::WAITING || job.status == db::RUNNING {
                        self.say(format!("{} is already {}", job.key(), job.status));
                    } else {
                        let action = Rereview {
                            owner: job.owner.clone(),
                            repo: job.repo.clone(),
                            number: job.number,
                            label: format!("{} {}", job.key(), view::commit_label(job)),
                        };
                        self.overlay = Overlay::Confirm(action);
                    }
                }
            }
            KeyCode::Char('x')
                if self.screen == Screen::Dashboard && self.focus == Focus::Queue =>
            {
                if let Some(job) = self.selected().cloned()
                    && self.db.drop_waiting(job.id)
                {
                    self.say(format!("dropped {} from the queue", job.key()));
                    self.reload();
                }
            }
            KeyCode::Char('p') if self.live => {
                let paused = activity::get().toggle_pause();
                self.say(if paused {
                    "queue paused: the review in progress finishes, nothing new starts"
                } else {
                    "queue resumed"
                });
            }
            KeyCode::Char('H') | KeyCode::Char('h') if self.live => {
                self.screen = if self.screen == Screen::History {
                    Screen::Dashboard
                } else {
                    Screen::History
                };
                self.reload();
            }
            KeyCode::Char('/') if self.screen == Screen::History => {
                self.search = Some(self.search_committed.clone());
            }
            _ => {}
        }
        let _ = stop;
        true
    }

    fn open_selected(&mut self) {
        let url = match &self.overlay {
            Overlay::Detail(id, _) => self.db.job(*id).ok().flatten().map(|j| j.url()),
            _ => self.selected().map(Job::url),
        };
        if let Some(url) = url {
            if open_url(&url) {
                self.say(format!("opened {url}"));
            } else {
                self.say(format!("no browser opener found; {url}"));
            }
        }
    }

    /// Text pasted into the terminal: PR links to queue. With no input open it
    /// opens one, so a link can be pasted straight onto the dashboard.
    fn paste(&mut self, text: &str) {
        if self.search.is_some() {
            return;
        }
        if matches!(self.overlay, Overlay::Confirm(_)) {
            return;
        }
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        match &mut self.adding {
            Some(typed) => {
                if !typed.is_empty() && !typed.ends_with(' ') {
                    typed.push(' ');
                }
                typed.push_str(&flat);
            }
            None if matches!(self.overlay, Overlay::None) => self.adding = Some(flat),
            None => {}
        }
    }

    /// Queue every PR link in `text`, as `prk review <url>` would (no forced
    /// full review: a PR already reviewed at this commit is skipped by the worker).
    fn add_prs(&mut self, text: &str) {
        let prs = crate::slack::PrRef::find_all(text);
        if prs.is_empty() {
            self.say("no GitHub PR link found; paste one like https://github.com/org/repo/pull/12");
            return;
        }
        let mut queued = Vec::new();
        let mut notes = Vec::new();
        for pr in &prs {
            match self
                .db
                .enqueue(&pr.owner, &pr.repo, pr.number, "", "", "", "manual", false)
            {
                Ok(Enqueued::New(_)) => queued.push(pr.key()),
                Ok(Enqueued::Pending(_, status)) => {
                    notes.push(format!("{} is already {status}", pr.key()))
                }
                Ok(Enqueued::Seen(_)) => notes.push(format!("{} is already queued", pr.key())),
                Err(e) => notes.push(format!("{}: {e}", pr.key())),
            }
        }
        let mut parts = Vec::new();
        if !queued.is_empty() {
            let tail = if self.live {
                ""
            } else {
                " (the running `prk` picks it up)"
            };
            parts.push(format!("queued {}{tail}", queued.join(", ")));
        }
        parts.extend(notes);
        self.say(parts.join("; "));
        self.reload();
    }

    fn rereview(&mut self, action: &Rereview) {
        // Reply in the PR's latest Slack thread, to whoever asked there.
        let asked = self
            .db
            .history(&HistoryFilter {
                pr: Some((action.owner.clone(), action.repo.clone(), action.number)),
                ..Default::default()
            })
            .into_iter()
            .find(|j| !j.slack_thread.is_empty());
        let (who, name, thread) = asked
            .map(|j| (j.requested_by, j.requested_name, j.slack_thread))
            .unwrap_or_default();
        let queued = self.db.enqueue(
            &action.owner,
            &action.repo,
            action.number,
            &who,
            "",
            &thread,
            "manual",
            true,
        );
        if let Ok(Enqueued::New(id)) = queued
            && !name.is_empty()
        {
            self.db.set_requested_name(id, &name);
        }
        match queued {
            Ok(_) if self.live => self.say(format!("{} queued for a full review", action.label)),
            Ok(_) => self.say(format!(
                "{} queued for a full review; the running `prk` picks it up",
                action.label
            )),
            Err(e) => self.say(format!("could not queue: {e}")),
        }
        self.reload();
    }
}

fn clamp(state: &mut ListState, len: usize) {
    match (state.selected(), len) {
        (_, 0) => state.select(None),
        (None, _) => {}
        (Some(i), n) if i >= n => state.select(Some(n - 1)),
        _ => {}
    }
}

/// `prk` in a terminal: the live dashboard. Returns when the user quits or
/// `stop` is set (ctrl-c / SIGTERM).
pub fn dashboard(
    db: Arc<Db>,
    channel: &str,
    reviewer: &str,
    timeout_minutes: u64,
    stop: Arc<Stop>,
) -> std::io::Result<()> {
    let mut app = App::new(db, Screen::Dashboard, true, HistoryFilter::default());
    app.channel = channel.to_string();
    app.reviewer = reviewer.to_string();
    app.timeout_secs = (timeout_minutes * 60) as i64;
    run(app, &stop)
}

/// `prk -H` in a terminal: the history browser.
pub fn history(db: Arc<Db>, filter: HistoryFilter) -> std::io::Result<()> {
    let app = App::new(db, Screen::History, false, filter);
    run(app, &Stop::default())
}

fn run(mut app: App, stop: &Stop) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    // So a pasted link arrives as one event, not as keystrokes (an Enter in it
    // would otherwise submit early).
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = event_loop(&mut terminal, &mut app, stop);
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, stop: &Stop) -> std::io::Result<()> {
    loop {
        if stop.is_set() {
            return Ok(());
        }
        if app.loaded.elapsed() > Duration::from_millis(500) {
            app.reload();
        }
        terminal.draw(|f| draw(f, app))?;
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if !app.key(k, stop) {
                        return Ok(());
                    }
                }
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
        app.tick += 1;
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
    match app.screen {
        Screen::Dashboard => draw_dashboard(f, body, app),
        Screen::History => draw_history(f, body, app),
    }
    draw_footer(f, footer, app);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Detail(id, scroll) => draw_detail(f, app, *id, *scroll),
        Overlay::Confirm(action) => draw_confirm(f, action),
    }
}

fn draw_dashboard(f: &mut Frame, area: Rect, app: &mut App) {
    let running = app.running.len().max(1) as u16;
    let [header, reviewing, lists, log] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(running * 2 + 2),
        Constraint::Length(
            app.waiting.len().clamp(1, 6) as u16 + app.recent.len().clamp(1, 12) as u16 + 4,
        ),
        Constraint::Min(4),
    ])
    .areas(area);
    draw_header(f, header, app);
    draw_reviewing(f, reviewing, app);
    let [queue, recent] = Layout::vertical([
        Constraint::Length(app.waiting.len().clamp(1, 6) as u16 + 2),
        Constraint::Length(app.recent.len().clamp(1, 12) as u16 + 2),
    ])
    .areas(lists);
    draw_queue(f, queue, app);
    draw_recent(f, recent, app);
    draw_log(f, log);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let a = activity::get();
    let mut right: Vec<Span> = Vec::new();
    if a.connected() {
        right.push(Span::styled("● connected", Style::new().fg(Color::Green)));
    } else {
        right.push(Span::styled(
            format!("{} connecting", SPIN[app.tick % SPIN.len()]),
            Style::new().fg(Color::Yellow),
        ));
    }
    if a.paused() {
        right.push(Span::styled("  ⏸ paused", Style::new().fg(Color::Yellow)));
    }
    if let Some((until, why)) = a.held() {
        right.push(Span::styled(
            format!("  ⏸ {why} until {}", view::when(until)),
            Style::new().fg(Color::Yellow),
        ));
    }
    match a.caught_up() {
        Some(Ok(n)) => right.push(muted(format!("  caught up: {n} new"))),
        Some(Err(_)) => right.push(Span::styled(
            "  catch-up failed",
            Style::new().fg(Color::Red),
        )),
        None => {}
    }
    right.push(muted(format!(
        "  up {}",
        view::elapsed(db::now() - a.started())
    )));
    let left = Line::from(vec![
        Span::styled(
            "prk",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        muted(format!(
            "  watching {} for <@{}>",
            app.channel, app.reviewer
        )),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(left), inner);
    f.render_widget(Paragraph::new(Line::from(right)).right_aligned(), inner);
}

fn bar(ratio: f64, width: usize) -> Vec<Span<'static>> {
    let filled = ((ratio.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let color = if ratio > 0.85 {
        Color::Yellow
    } else {
        Color::Cyan
    };
    vec![
        Span::styled("▰".repeat(filled), Style::new().fg(color)),
        Span::styled("▱".repeat(width - filled), Style::new().fg(Color::DarkGray)),
    ]
}

fn draw_reviewing(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("Reviewing");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.running.is_empty() {
        let text = if activity::get().paused() {
            "paused: press p to resume"
        } else if app.waiting.is_empty() {
            "idle: waiting for a review request"
        } else {
            "starting the next review"
        };
        f.render_widget(Paragraph::new(muted(text)), inner);
        return;
    }
    let step = activity::get().step();
    let timeout = app.timeout_secs.max(1);
    let mut lines: Vec<Line> = Vec::new();
    for job in &app.running {
        let (step_name, since) = match &step {
            Some((id, s, since)) if *id == job.id => (s.clone(), *since),
            _ => ("reviewing (another prk)".to_string(), job.started_at),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", SPIN[app.tick % SPIN.len()]),
                Style::new().fg(Color::Cyan),
            ),
            Span::styled(job.short_key(), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(view::clip(&job.title, 50)),
            muted(format!("  {}  {}", view::commit_label(job), job.author)),
        ]));
        let elapsed = job.took();
        let mut second = vec![Span::styled(
            format!("  {:<20}", step_name),
            Style::new().fg(Color::Cyan),
        )];
        second.extend(bar(elapsed as f64 / timeout as f64, 24));
        second.push(muted(format!(
            "  {}  step {}",
            view::elapsed(elapsed),
            view::elapsed(db::now() - since)
        )));
        if job.kind == "follow-up" {
            second.push(Span::styled("  follow-up", Style::new().fg(Color::Magenta)));
        }
        lines.push(Line::from(second));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn job_line(job: &Job, flash: bool, width: u16) -> ListItem<'static> {
    let color = status_color(&job.status);
    let note = if job.reason.is_empty() {
        job.title.clone()
    } else {
        job.reason.clone()
    };
    let line = Line::from(vec![
        Span::styled(format!("{} ", glyph(&job.status)), Style::new().fg(color)),
        Span::raw(format!("{:<22} ", view::clip(&job.short_key(), 22))),
        Span::styled(
            format!("{:<14} ", view::clip(&requester(job), 14)),
            Style::new().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{:<10}", view::status_word(job)),
            Style::new().fg(color),
        ),
        muted(format!(
            "{:>7}  {:<16}",
            view::took(job.took()),
            view::when(job.finished_at.max(job.started_at).max(job.queued_at))
        )),
        Span::raw(view::clip(
            &note,
            (width as usize)
                .saturating_sub(2 + 23 + 15 + 10 + 7 + 2 + 16 + 4)
                .max(10),
        )),
    ]);
    let item = ListItem::new(line);
    if flash {
        item.style(Style::new().bg(if job.status == db::REVIEWED {
            Color::Rgb(20, 60, 30)
        } else {
            Color::Rgb(70, 25, 25)
        }))
    } else {
        item
    }
}

/// Who asked for the review: their Slack name, else their id, else "manual".
fn requester(job: &Job) -> String {
    if !job.requested_name.is_empty() {
        job.requested_name.clone()
    } else if !job.requested_by.is_empty() {
        job.requested_by.clone()
    } else {
        "manual".to_string()
    }
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(Color::Rgb(40, 50, 70))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    }
}

fn draw_queue(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Queue;
    let title = format!(
        "Queue ({}){}",
        app.waiting.len(),
        if focused { " ◂" } else { "" }
    );
    let items: Vec<ListItem> = if app.waiting.is_empty() {
        vec![ListItem::new(muted("nothing waiting"))]
    } else {
        app.waiting
            .iter()
            .map(|j| {
                let wait = if j.not_before > db::now() {
                    format!("retry {}", view::when(j.not_before))
                } else {
                    format!("queued {}", view::when(j.queued_at))
                };
                let asked = if j.requested_by.is_empty() {
                    j.source.clone()
                } else {
                    format!("{} ({})", j.requested_by, j.source)
                };
                ListItem::new(Line::from(vec![
                    Span::styled("• ", Style::new().fg(Color::Blue)),
                    Span::raw(format!("{:<22} ", view::clip(&j.short_key(), 22))),
                    muted(format!("{:<22} {:<26}", wait, view::clip(&asked, 26))),
                    Span::raw(view::clip(
                        if j.reason.is_empty() {
                            &j.title
                        } else {
                            &j.reason
                        },
                        60,
                    )),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(panel(&title))
        .highlight_style(highlight(focused));
    f.render_stateful_widget(list, area, &mut app.queue_state);
}

fn draw_recent(f: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Recent;
    let title = format!("Recent{}", if focused { " ◂" } else { "" });
    let now = db::now();
    let items: Vec<ListItem> = if app.recent.is_empty() {
        vec![ListItem::new(muted("no reviews yet"))]
    } else {
        app.recent
            .iter()
            .map(|j| job_line(j, now - j.finished_at < FLASH_SECS, area.width))
            .collect()
    };
    let list = List::new(items)
        .block(panel(&title))
        .highlight_style(highlight(focused));
    f.render_stateful_widget(list, area, &mut app.recent_state);
}

fn draw_log(f: &mut Frame, area: Rect) {
    let block = panel("Log");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = activity::get()
        .logs(inner.height as usize)
        .into_iter()
        .map(|(at, level, msg)| {
            let color = match level {
                "ERROR" => Color::Red,
                "WARN" => Color::Yellow,
                "DEBUG" => Color::DarkGray,
                _ => Color::Gray,
            };
            let time = chrono::DateTime::from_timestamp(at, 0)
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            Line::from(vec![
                muted(format!("{time} ")),
                Span::styled(msg, Style::new().fg(color)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_history(f: &mut Frame, area: Rect, app: &mut App) {
    let jobs = &app.history;
    let count = |s: &str| jobs.iter().filter(|j| j.status == s).count();
    let mut title = format!(
        "History · {} jobs · {} reviewed · {} skipped · {} failed",
        jobs.len(),
        count(db::REVIEWED),
        count(db::SKIPPED),
        count(db::FAILED)
    );
    if !app.search_committed.is_empty() {
        title.push_str(&format!(" · /{}", app.search_committed));
    }
    // borders, glyph, when, status, PR, commit, took
    let note_w = (area.width as usize)
        .saturating_sub(2 + 2 + 16 + 10 + 23 + 17 + 9)
        .max(10);
    let items: Vec<ListItem> = if jobs.is_empty() {
        vec![ListItem::new(muted("nothing here yet"))]
    } else {
        jobs.iter()
            .map(|j| {
                let color = status_color(&j.status);
                let note = if j.reason.is_empty() {
                    j.title.clone()
                } else {
                    j.reason.clone()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", glyph(&j.status)), Style::new().fg(color)),
                    muted(format!(
                        "{:<16}",
                        view::when(j.finished_at.max(j.started_at).max(j.queued_at))
                    )),
                    Span::styled(
                        format!("{:<10}", view::status_word(j)),
                        Style::new().fg(color),
                    ),
                    Span::raw(format!("{:<22} ", view::clip(&j.short_key(), 22))),
                    muted(format!(
                        "{:<16} {:>7}  ",
                        view::commit_label(j),
                        view::took(j.took())
                    )),
                    Span::raw(view::clip(&note, note_w)),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(panel(&title))
        .highlight_style(highlight(true));
    f.render_stateful_widget(list, area, &mut app.history_state);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    if let Some(text) = &app.adding {
        // keep the hint visible: show only the end of a long paste
        let room = (area.width as usize).saturating_sub(48).max(10);
        let chars: Vec<char> = text.chars().collect();
        let shown: String = if chars.len() > room {
            format!(
                "…{}",
                chars[chars.len() - room..].iter().collect::<String>()
            )
        } else {
            text.clone()
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" PR ", Style::new().fg(Color::Black).bg(Color::Green)),
                Span::raw(format!(" {shown}▏")),
                muted("  paste PR link(s) · enter queue · esc cancel"),
            ])),
            area,
        );
        return;
    }
    if let Some(query) = &app.search {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" / ", Style::new().fg(Color::Black).bg(Color::Cyan)),
                Span::raw(format!(" {query}▏")),
                muted("  enter search · esc cancel"),
            ])),
            area,
        );
        return;
    }
    if let Some((note, at)) = &app.note
        && at.elapsed() < Duration::from_secs(4)
    {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {note}"),
                Style::new().fg(Color::Yellow),
            )),
            area,
        );
        return;
    }
    let mut spans = key_hint("q", "quit");
    let keys: &[(&str, &str)] = match (&app.overlay, app.screen) {
        (Overlay::Detail(..), _) => &[("↑↓", "scroll"), ("o", "open PR"), ("esc", "back")],
        (_, Screen::Dashboard) => &[
            ("↑↓", "select"),
            ("tab", "queue/recent"),
            ("enter", "details"),
            ("o", "open PR"),
            ("a", "add PR"),
            ("R", "re-review"),
            ("x", "drop"),
            ("p", "pause"),
            ("H", "history"),
        ],
        (_, Screen::History) => &[
            ("↑↓", "scroll"),
            ("/", "search"),
            ("enter", "details"),
            ("o", "open PR"),
            ("R", "re-review"),
        ],
    };
    for (k, what) in keys {
        spans.extend(key_hint(k, what));
    }
    if app.screen == Screen::History && app.live && matches!(app.overlay, Overlay::None) {
        spans.extend(key_hint("esc", "dashboard"));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let [_, mid, _] = Layout::vertical([
        Constraint::Percentage((100 - height_pct) / 2),
        Constraint::Percentage(height_pct),
        Constraint::Percentage((100 - height_pct) / 2),
    ])
    .areas(area);
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage((100 - width_pct) / 2),
        Constraint::Percentage(width_pct),
        Constraint::Percentage((100 - width_pct) / 2),
    ])
    .areas(mid);
    mid
}

fn draw_confirm(f: &mut Frame, action: &Rereview) {
    let area = centered(f.area(), 50, 20);
    f.render_widget(Clear, area);
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("Review "),
            Span::styled(
                action.label.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" again?"),
        ]),
        Line::from(muted("A full review that posts new comments on the PR.")),
        Line::from(""),
        Line::from(vec![
            Span::styled(" y ", Style::new().fg(Color::Black).bg(Color::Green)),
            Span::raw(" yes   "),
            Span::styled(
                " any other key ",
                Style::new().fg(Color::Black).bg(Color::Gray),
            ),
            Span::raw(" no"),
        ]),
    ]);
    f.render_widget(
        Paragraph::new(text).centered().block(panel("Review again")),
        area,
    );
}

/// The details of one job and the comments it posted.
fn detail_text(db: &Db, job: &Job, width: usize) -> Vec<Line<'static>> {
    let color = status_color(&job.status);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} {}  ", glyph(&job.status), view::status_word(job)),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(job.key(), Style::new().add_modifier(Modifier::BOLD)),
            muted(format!("  {}", view::commit_label(job))),
        ]),
        Line::from(job.title.clone()),
        Line::from(muted(job.url())),
        Line::from(""),
    ];
    let mut field = |label: &str, value: String| {
        if !value.is_empty() {
            lines.push(Line::from(vec![
                muted(format!("{label:<10} ")),
                Span::raw(value),
            ]));
        }
    };
    field("result", job.reason.clone());
    field("author", job.author.clone());
    field(
        "asked by",
        if job.requested_by.is_empty() {
            job.source.clone()
        } else {
            format!("{} ({})", job.requested_by, job.source)
        },
    );
    field("message", view::slack_time(&job.slack_ts));
    field("queued", view::when(job.queued_at));
    field(
        "took",
        if job.took() > 0 {
            view::took(job.took())
        } else {
            String::new()
        },
    );
    field(
        "attempts",
        if job.attempts > 1 {
            job.attempts.to_string()
        } else {
            String::new()
        },
    );
    field("log", job.log.clone());

    let comments = db.comments(job.id);
    lines.push(Line::from(""));
    let inline = comments.iter().filter(|c| c.kind == "inline").count();
    lines.push(Line::from(Span::styled(
        format!("Comments ({inline})"),
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    if comments.is_empty() {
        lines.push(Line::from(muted("none saved for this job")));
    }
    let mut path = String::new();
    for c in comments.iter().filter(|c| c.kind != "summary") {
        if c.path != path {
            path = c.path.clone();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                path.clone(),
                Style::new().fg(Color::Cyan),
            )));
        }
        let (mark, word, vcolor) = view::verdict_look(&c.verdict);
        let tag = if c.kind == "reply" {
            muted("reply   ")
        } else if word.is_empty() {
            Span::raw("        ")
        } else {
            Span::styled(
                format!("{mark} {word:<6}"),
                Style::new().fg(to_ratatui(vcolor)),
            )
        };
        let wrapped = view::wrap(&c.body, width.saturating_sub(26).max(30));
        for (i, text) in wrapped.into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    muted(format!("  {:>6}  ", format!(":{}", c.line))),
                    tag.clone(),
                    Span::raw(text),
                ]));
            } else {
                lines.push(Line::from(vec![Span::raw(" ".repeat(18)), Span::raw(text)]));
            }
        }
    }
    for c in comments.iter().filter(|c| c.kind == "summary") {
        lines.push(Line::from(""));
        for (i, text) in view::wrap(&c.body, width.saturating_sub(12).max(30))
            .into_iter()
            .enumerate()
        {
            let label = if i == 0 {
                Span::styled("summary  ", Style::new().fg(Color::Magenta))
            } else {
                Span::raw("         ")
            };
            lines.push(Line::from(vec![label, Span::raw(text)]));
        }
    }
    lines
}

fn to_ratatui(c: crossterm::style::Color) -> Color {
    use crossterm::style::Color as C;
    match c {
        C::Green => Color::Green,
        C::Yellow => Color::Yellow,
        C::Cyan => Color::Cyan,
        C::Red => Color::Red,
        _ => Color::Reset,
    }
}

fn draw_detail(f: &mut Frame, app: &App, id: i64, scroll: u16) {
    let area = centered(f.area(), 90, 90);
    f.render_widget(Clear, area);
    let Some(job) = app.db.job(id).ok().flatten() else {
        return;
    };
    let text = detail_text(&app.db, &job, area.width as usize);
    f.render_widget(
        Paragraph::new(text)
            .block(panel(&format!("{} · details", job.key())))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Comment, Enqueued};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_db() -> Arc<Db> {
        // one folder per call: tests run in parallel
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("prk-tui-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let db = Arc::new(Db::open(&dir.join("state.sqlite3")).unwrap());
        let add = |repo: &str, n: u64, who: &str| match db
            .enqueue(
                "Blitzscale-19",
                repo,
                n,
                who,
                &format!("{n}.0"),
                "",
                "slack",
                false,
            )
            .unwrap()
        {
            Enqueued::New(id) => id,
            other => panic!("{other:?}"),
        };
        let done = |id: i64, status: &str, kind: &str, prev: &str, why: &str, title: &str| {
            db.claim(id).unwrap();
            db.describe(id, "916a2e0685f1", title, "abhishek-blitz", kind, prev);
            db.finish(id, status, why);
        };
        let a = add("suchna", 819, "U0BTNU87");
        done(
            a,
            db::REVIEWED,
            "review",
            "",
            "3 comments",
            "reassign open tasks to the seller's GL",
        );
        db.save_comments(&[
            Comment { job_id: a, gh_id: 1, kind: "inline".into(), path: "src/evolutions/reassign.ts".into(), line: 42,
                      body: "Handler is not wrapped in asyncWrapper, so a rejected promise skips the error middleware. (rule: wrap route handlers)".into(),
                      verdict: "fixed".into(), verdict_commit: "a1b2c3d".into(), ..Default::default() },
            Comment { job_id: a, gh_id: 2, kind: "inline".into(), path: "src/dao/task.dao.ts".into(), line: 17,
                      body: "Query is missing is_active: true. (rule: soft delete)".into(), verdict: "open".into(), ..Default::default() },
            Comment { job_id: a, gh_id: 3, kind: "summary".into(), body: "**Follow-up review** (427869c → 916a2e0): 1 of 2 fixed, 1 still open.".into(), ..Default::default() },
        ]);
        let b = add("alfred", 2014, "U0B033K6");
        done(
            b,
            db::REVIEWED,
            "follow-up",
            "427869c3461a",
            "follow-up: 1/2 fixed · 1 open · 0 partly · 1 new",
            "bar chart atom and order trends widget",
        );
        let c = add("suchna", 826, "U046BKH1");
        done(c, db::SKIPPED, "", "", "PR is merged", "edit website CTA");
        let d = add("vartalap", 504, "U046BKH1");
        db.claim(d).unwrap();
        db.describe(
            d,
            "77de901aaaa",
            "enable gc hits calling",
            "pankaj",
            "review",
            "",
        );
        db.fail(d, "timed out after 30 minutes", 1, 0);
        let e = add("naarad", 285, "U0B0XB7K");
        db.claim(e).unwrap();
        db.describe(
            e,
            "c8545260f3aa",
            "admin report download",
            "hitesh",
            "review",
            "",
        );
        activity::get().set_step(e, "reviewing");
        add("soochi_dash", 870, "U0BTNU87");
        add("otp_service", 176, "U0BTNU87");
        db
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(app: &mut App, code: KeyCode) {
        let k = KeyEvent::new(code, KeyModifiers::NONE);
        assert!(app.key(k, &Stop::default()));
    }

    #[test]
    fn pasting_pr_links_queues_them() {
        let db = sample_db();
        let mut app = App::new(
            db.clone(),
            Screen::Dashboard,
            true,
            HistoryFilter::default(),
        );
        let before = db.waiting().len();

        // `a` opens the input, a paste fills it (newlines flattened), enter queues
        press(&mut app, KeyCode::Char('a'));
        assert!(app.adding.is_some());
        app.paste("https://github.com/Blitzscale-19/new_repo/pull/5\nhttps://github.com/Blitzscale-19/other/pull/9\n");
        let shown = render(&mut app, 130, 30);
        assert!(
            shown.contains("pull/5") && shown.contains("enter queue"),
            "{shown}"
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.adding.is_none());
        assert_eq!(db.waiting().len(), before + 2);
        let keys: Vec<String> = db.waiting().iter().map(|j| j.key()).collect();
        assert!(keys.iter().any(|k| k.contains("new_repo#5")), "{keys:?}");
        assert!(db.waiting().iter().any(|j| j.source == "manual"));

        // pasting the same link again is not queued twice
        app.paste("https://github.com/Blitzscale-19/new_repo/pull/5");
        assert!(
            app.adding.is_some(),
            "a paste on the dashboard opens the input"
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(db.waiting().len(), before + 2);
        assert!(app.note.as_ref().unwrap().0.contains("already"));

        // text without a link queues nothing and says so
        press(&mut app, KeyCode::Char('a'));
        for c in "hello".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(db.waiting().len(), before + 2);
        assert!(app.note.as_ref().unwrap().0.contains("no GitHub PR link"));

        // esc cancels
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        assert!(app.adding.is_none());
    }

    #[test]
    fn dashboard_history_and_details_render() {
        let db = sample_db();
        let mut app = App::new(
            db.clone(),
            Screen::Dashboard,
            true,
            HistoryFilter::default(),
        );
        app.channel = "CPWCB54G2".into();
        app.reviewer = "U0ANSHUMAN".into();
        let dash = render(&mut app, 130, 36);
        if std::env::var_os("PRK_SHOW").is_some() {
            println!("{dash}");
        }
        for want in [
            "watching CPWCB54G2",
            "Reviewing",
            "naarad#285",
            "reviewing",
            "Queue (2)",
            "soochi_dash#870",
            "Recent",
            "suchna#819",
            "follow-up",
            "PR is merged",
            "failed",
            "re-review",
            "history",
        ] {
            assert!(
                dash.contains(want),
                "dashboard is missing {want:?}:\n{dash}"
            );
        }

        app.screen = Screen::History;
        app.reload();
        let hist = render(&mut app, 130, 20);
        if std::env::var_os("PRK_SHOW").is_some() {
            println!("{hist}");
        }
        for want in [
            "History",
            "7 jobs",
            "alfred#2014",
            "427869c→916a2e0",
            "PR is merged",
            "search",
        ] {
            assert!(hist.contains(want), "history is missing {want:?}:\n{hist}");
        }

        let reviewed = app.history.iter().find(|j| j.number == 819).unwrap().id;
        app.overlay = Overlay::Detail(reviewed, 0);
        let detail = render(&mut app, 130, 36);
        if std::env::var_os("PRK_SHOW").is_some() {
            println!("{detail}");
        }
        for want in [
            "Comments (2)",
            "src/dao/task.dao.ts",
            ":17",
            "fixed",
            "open",
            "summary",
            "asked by",
        ] {
            assert!(
                detail.contains(want),
                "details are missing {want:?}:\n{detail}"
            );
        }
    }
}
