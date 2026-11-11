//! The terminal screen: `orgthread watch`. It draws what `watch.rs` reads. It
//! changes a file in one way only: `r`, then `y`, reverts the newest change of
//! a file, through `revert.rs`.

use crate::fingerprint::fingerprint;
use crate::history::{self, Job, Remote, Step};
use crate::revert::{self, Plan, Question, Reverter};
use crate::versions::Versions;
use crate::watch::{self, Entry, Row, Snapshot, snapshot};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};
use std::cell::Cell;
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const BLUE: Color = Color::Rgb(0x22, 0x6D, 0xBD);
const ORANGE: Color = Color::Rgb(0xE8, 0x74, 0x0C);
const REFRESH: Duration = Duration::from_secs(1);
/// How often the wait screen moves its spinner.
const SPIN: Duration = Duration::from_millis(100);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// What the screen tells about itself. Each part is fixed at build time, one word
/// from a short list, or the path of the log, so no address, secret path, token or
/// login hash can reach the screen through it.
pub struct Info {
    storage: &'static str,
    dropbox: bool,
    log: String,
}

impl Info {
    pub fn new(dropbox: bool, log: &Path) -> Info {
        Info {
            storage: crate::storage::kind(dropbox),
            dropbox,
            log: log.display().to_string(),
        }
    }
}

/// Where the diff screen gets the two texts of a write.
pub enum Diffs {
    /// The copies that each write keeps on this disk, on the Mac.
    Kept(Versions),
    /// The Dropbox history, on a server, in memory only.
    Dropbox(Remote),
}

/// The name, the version, the commit of this build and the kind of storage.
fn title(info: &Info) -> String {
    format!("orgthread {} · {}", crate::VERSION, info.storage)
}

fn head_line(info: &Info) -> Line<'static> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(title(info), bold),
        Span::raw("   r, then y: revert   ?: help"),
    ])
}

/// Which line is chosen. `None` follows the newest line. A chosen line is kept
/// by its time, path, fingerprints and the rest of its fields, never by its
/// place in the list, so neither a new line nor the end of the log window
/// moves the choice to another line.
#[derive(Default)]
pub struct Choice(Option<Pick>);

struct Pick {
    entry: Entry,
    /// How many equal lines come before it, the oldest first.
    nth: usize,
}

impl Choice {
    /// The index of the chosen line in a list with the oldest first. A chosen
    /// line that left the part of the log that is read gives the newest.
    fn index(&self, entries: &[Entry]) -> Option<usize> {
        let newest = entries.len().checked_sub(1)?;
        let Some(pick) = &self.0 else {
            return Some(newest);
        };
        let found = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| **e == pick.entry)
            .nth(pick.nth);
        Some(found.map_or(newest, |(i, _)| i))
    }

    /// The place on the screen, where 0 is the newest line.
    fn place(&self, entries: &[Entry]) -> usize {
        self.index(entries).map_or(0, |i| entries.len() - 1 - i)
    }

    /// Chooses the line at `index` and stays on it, also when it is the newest.
    fn pin(&mut self, index: usize, entries: &[Entry]) {
        self.0 = entries.get(index).map(|entry| Pick {
            nth: entries[..index].iter().filter(|e| *e == entry).count(),
            entry: entry.clone(),
        });
    }

    fn set_place(&mut self, place: usize, entries: &[Entry]) {
        let count = entries.len();
        if place == 0 || place >= count {
            self.0 = None;
        } else {
            self.pin(count - 1 - place, entries);
        }
    }

    /// Applies one key. True means quit.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, entries: &[Entry]) -> bool {
        let count = entries.len();
        let place = self.place(entries);
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Up | KeyCode::Char('k') => self.set_place(place.saturating_sub(1), entries),
            KeyCode::Down | KeyCode::Char('j') => {
                self.set_place((place + 1).min(count.saturating_sub(1)), entries);
            }
            KeyCode::Home | KeyCode::Char('g') => self.0 = None,
            _ => {}
        }
        false
    }
}

fn color_of(outcome: &str) -> Color {
    if watch::is_written(outcome) || outcome == "copied" || outcome == "moved" {
        BLUE
    } else {
        ORANGE
    }
}

/// Breaks text at spaces to fit the width. A longer word is cut, so nothing is
/// ever lost at the edge of a narrow terminal.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let used = row.chars().count();
        let size = word.chars().count();
        if used > 0 && used + 1 + size > width {
            rows.push(std::mem::take(&mut row));
        } else if used > 0 {
            row.push(' ');
        }
        let mut rest: Vec<char> = word.chars().collect();
        while rest.len() > width {
            let tail = rest.split_off(width);
            rows.push(rest.iter().collect());
            rest = tail;
        }
        row.extend(rest);
    }
    rows.push(row);
    rows
}

const LABEL: usize = 10;

/// A label in bold, and its value in a column that wraps under itself.
fn labelled(label: &str, value: &str, width: usize) -> Vec<Line<'static>> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    wrap(value, width.saturating_sub(LABEL))
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let head = if i == 0 { label } else { "" };
            Line::from(vec![
                Span::styled(format!("{head:<LABEL$}"), bold),
                Span::raw(row),
            ])
        })
        .collect()
}

fn plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|row| Line::styled(row, style))
        .collect()
}

fn details(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    let (first, second) = if entry.outcome == "changed" {
        ("Expected", "Found")
    } else {
        ("Before", "After")
    };
    let mut lines = labelled("File", &entry.path, width);
    if let Some(from) = entry.copy_from.as_ref().or(entry.move_from.as_ref()) {
        lines.extend(labelled("From", from, width));
    }
    lines.extend(labelled("Who", &entry.who, width));
    let time = format!("{} ({} ago)", entry.time, watch::ago(&entry.time));
    lines.extend(labelled("Time", &time, width));
    let color = Style::new().fg(color_of(&entry.outcome));
    lines.extend(plain(watch::why_of(entry), width, color));
    if !entry.before.is_empty() {
        lines.extend(labelled(first, &entry.before, width));
    }
    if !entry.after.is_empty() {
        lines.extend(labelled(second, &entry.after, width));
    }
    lines
}

fn top_lines(snap: &Snapshot, width: usize) -> Vec<Line<'static>> {
    let orange = Style::new().fg(ORANGE);
    let mut lines = Vec::new();
    for lock in &snap.locks {
        let state = if lock.stale { ", STALE" } else { "" };
        let text = format!(
            "{}  since {}{state}  {}",
            lock.path,
            watch::short_age(lock.age.as_secs()),
            lock.holder
        );
        lines.extend(plain(&text, width, orange));
    }
    for name in &snap.left_behind {
        lines.extend(plain(&format!("left behind: {name}"), width, orange));
    }
    for problem in &snap.problems {
        lines.extend(plain(&format!("cannot read {problem}"), width, orange));
    }
    if lines.is_empty() {
        lines.push(Line::raw("none"));
    }
    lines
}

/// The end of a path matters most, so a long one loses its start.
fn tail_of(path: &str, room: usize) -> String {
    let count = path.chars().count();
    if count <= room {
        return path.to_string();
    }
    let keep = room.saturating_sub(3);
    let tail: String = path.chars().skip(count - keep).collect();
    format!("...{tail}")
}

/// One row of the list. A narrow terminal drops the size and the word "ago".
fn row_of(entry: &Entry, width: usize) -> String {
    let ago = watch::ago(&entry.time);
    if width >= 80 {
        let size = if entry.size.is_empty() {
            String::new()
        } else {
            format!("{} B", entry.size)
        };
        let head = format!("{ago:>7} ago  {:<12}  {size:>9}  ", entry.outcome);
        let room = width.saturating_sub(head.chars().count());
        format!("{head}{}", tail_of(&entry.path, room))
    } else {
        let head = format!("{ago:>6}  {:<12}  ", entry.outcome);
        let room = width.saturating_sub(head.chars().count());
        format!("{head}{}", tail_of(&entry.path, room))
    }
}

pub fn draw(frame: &mut Frame, snap: &Snapshot, choice: &Choice, info: &Info) {
    draw_with(frame, snap, choice, info, None);
}

/// The keys of the main screen, for its last line. A narrow one gets fewer.
fn list_keys(width: u16) -> &'static str {
    if width >= 80 {
        "up, down: line  h, l: same file  enter: diff  r: revert  ?: help  q: quit"
    } else {
        "up, down: line  enter: diff  ?: help  q: quit"
    }
}

/// The main screen. `foot` takes the place of the keys on the last lines: a
/// question, a wait or an answer.
fn draw_with(
    frame: &mut Frame,
    snap: &Snapshot,
    choice: &Choice,
    info: &Info,
    foot: Option<Vec<Line<'static>>>,
) {
    let width = usize::from(frame.area().width.saturating_sub(2));
    let foot = foot.unwrap_or_else(|| vec![Line::raw(list_keys(frame.area().width))]);
    let top = top_lines(snap, width);
    let count = snap.entries.len();
    let place = choice.place(&snap.entries);
    let chosen = snap.entries.iter().rev().nth(place);
    let detail = chosen.map_or_else(Vec::new, |e| details(e, width));

    let [head, locks, list, below, foot_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length((top.len() as u16 + 2).min(10)),
        Constraint::Min(4),
        Constraint::Length(detail.len() as u16 + 2),
        Constraint::Length(foot.len() as u16),
    ])
    .areas(frame.area());

    frame.render_widget(head_line(info), head);
    frame.render_widget(
        Paragraph::new(top).block(Block::bordered().title("Locks held now")),
        locks,
    );

    let items: Vec<ListItem> = snap
        .entries
        .iter()
        .rev()
        .map(|e| {
            let text = row_of(e, width);
            ListItem::new(text).style(Style::new().fg(color_of(&e.outcome)))
        })
        .collect();
    let title = format!("Latest writes and refusals ({count})");
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No writes yet.").block(Block::bordered().title(title)),
            list,
        );
    } else {
        let widget = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default().with_selected(Some(place));
        frame.render_stateful_widget(widget, list, &mut state);
    }

    frame.render_widget(
        Paragraph::new(detail).block(Block::bordered().title("Chosen line")),
        below,
    );
    frame.render_widget(Paragraph::new(foot), foot_area);
}

/// The steps of a fetch that runs in the background.
struct Wait {
    headline: &'static str,
    started: Instant,
    /// Each step, and whether it is done.
    steps: Vec<(bool, String)>,
    steps_in: Receiver<Step>,
    refused: bool,
}

enum Body {
    Ready(Result<Vec<Row>, String>),
    Waiting(Wait),
}

/// The rows of two texts, or why there are none.
fn rows_of(refused: bool, (old, new): history::Texts) -> Result<Vec<Row>, String> {
    let rows = watch::diff_rows(&old, &new);
    if !rows.is_empty() {
        Ok(rows)
    } else if refused {
        Err("The expected and the found version hold the same text.".to_string())
    } else {
        Err("The text did not change in this write.".to_string())
    }
}

/// The open diff screen.
pub struct DiffView {
    title: String,
    entry: Entry,
    body: Body,
    /// True after a wait that ran out of time: Enter tries again.
    retry: bool,
    scroll: usize,
    /// How far it can scroll, known once it is drawn at the terminal's size.
    max_scroll: Cell<usize>,
}

impl DiffView {
    fn with(entry: &Entry, body: Body) -> DiffView {
        let mut title = format!("{}   {} ago", entry.path, watch::ago(&entry.time));
        if entry.outcome == "changed" {
            title.push_str("   expected → found");
        }
        DiffView {
            title,
            entry: entry.clone(),
            body,
            retry: false,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    /// On Dropbox, the fetch starts in the background, and the screen shows its
    /// steps. A write whose versions are in memory opens at once.
    pub fn open(entry: &Entry, diffs: &Diffs) -> DiffView {
        let body = match diffs {
            Diffs::Kept(versions) => Body::Ready(watch::diff_of(entry, versions)),
            Diffs::Dropbox(remote) => match Job::of(entry) {
                Err(reason) => Body::Ready(Err(reason)),
                Ok(job) => match remote.from_memory(&job) {
                    Some(texts) => Body::Ready(rows_of(job.refused, texts)),
                    None => Body::Waiting(Wait {
                        headline: job.headline(),
                        started: Instant::now(),
                        steps: Vec::new(),
                        refused: job.refused,
                        steps_in: remote.start(job),
                    }),
                },
            },
        };
        DiffView::with(entry, body)
    }

    /// True while the fetch runs.
    pub fn waiting(&self) -> bool {
        matches!(self.body, Body::Waiting(_))
    }

    /// Takes in what the fetch told so far, and stops waiting after the limit.
    pub fn tick(&mut self, now: Instant) {
        let Body::Waiting(wait) = &mut self.body else {
            return;
        };
        let mut done = None;
        loop {
            match wait.steps_in.try_recv() {
                Ok(Step::Doing(text)) => wait.steps.push((false, text)),
                Ok(Step::Did(text)) => match wait.steps.last_mut() {
                    Some(last) if !last.0 => *last = (true, text),
                    _ => wait.steps.push((true, text)),
                },
                Ok(Step::Done(result)) => {
                    done = Some(result.and_then(|texts| rows_of(wait.refused, texts)));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = Some(Err(
                        "The fetch stopped with no answer. Press Esc, then Enter to try again."
                            .to_string(),
                    ));
                    break;
                }
            }
        }
        if done.is_none() && now.saturating_duration_since(wait.started) >= history::WAIT {
            done = Some(Err(history::TIMEOUT.to_string()));
            self.retry = true;
        }
        if let Some(result) = done {
            self.body = Body::Ready(result);
        }
    }

    /// Applies one key. True means go back to the main screen.
    pub fn key(&mut self, code: KeyCode) -> bool {
        let max = self.max_scroll.get();
        match code {
            KeyCode::Esc => return true,
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + 20).min(max),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = max,
            _ => {}
        }
        false
    }
}

/// One diff row as screen lines. A long line wraps under its own text.
fn diff_lines(row: &Row, width: usize) -> Vec<Line<'static>> {
    const HEAD: usize = 8;
    let (sign, line, text, style) = match row {
        Row::Same { line, text } => (" ", *line, text, Style::new()),
        Row::Removed { line, text } => ("-", *line, text, Style::new().fg(ORANGE)),
        Row::Added { line, text } => ("+", *line, text, Style::new().fg(BLUE)),
        Row::Gap => return vec![Line::raw("...")],
    };
    wrap(text, width.saturating_sub(HEAD))
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            let head = if i == 0 {
                format!("{sign} {line:>4}  ")
            } else {
                " ".repeat(HEAD)
            };
            Line::styled(format!("{head}{part}"), style)
        })
        .collect()
}

pub fn draw_diff(frame: &mut Frame, view: &DiffView, info: &Info) {
    draw_diff_with(frame, view, info, None);
}

fn diff_keys(width: u16) -> &'static str {
    if width >= 80 {
        "up, down: scroll   h, l: same file   r: revert   Esc: back   ?: help   q: quit"
    } else {
        "up, down: scroll   Esc: back   ?: help   q: quit"
    }
}

fn draw_diff_with(
    frame: &mut Frame,
    view: &DiffView,
    info: &Info,
    foot: Option<Vec<Line<'static>>>,
) {
    let width = usize::from(frame.area().width.saturating_sub(2));
    let foot = foot.unwrap_or_else(|| vec![Line::raw(diff_keys(frame.area().width))]);
    let [head, body, foot_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(foot.len() as u16),
    ])
    .areas(frame.area());
    let lines: Vec<Line> = match &view.body {
        Body::Ready(Ok(rows)) => rows.iter().flat_map(|r| diff_lines(r, width)).collect(),
        Body::Ready(Err(reason)) => plain(reason, width, Style::new().fg(ORANGE)),
        Body::Waiting(wait) => wait_lines(wait, Instant::now(), width),
    };
    let visible = usize::from(body.height.saturating_sub(2));
    let max = lines.len().saturating_sub(visible);
    view.max_scroll.set(max);
    let scroll = view.scroll.min(max);
    let shown: Vec<Line> = lines.into_iter().skip(scroll).collect();
    frame.render_widget(head_line(info), head);
    frame.render_widget(
        Paragraph::new(shown).block(Block::bordered().title(view.title.clone())),
        body,
    );
    frame.render_widget(Paragraph::new(foot), foot_area);
}

/// The wait screen: the headline with a spinner and the seconds, then the steps.
fn wait_lines(wait: &Wait, now: Instant, width: usize) -> Vec<Line<'static>> {
    let gone = now.saturating_duration_since(wait.started);
    let spin = SPINNER[(gone.as_millis() / SPIN.as_millis()) as usize % SPINNER.len()];
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{spin} "), Style::new().fg(BLUE)),
        Span::raw(format!("{}   {} s", wait.headline, gone.as_secs())),
    ])];
    for (done, text) in &wait.steps {
        let (sign, style) = if *done {
            ("✓", Style::new().fg(BLUE))
        } else {
            ("…", Style::new())
        };
        for (i, part) in wrap(text, width.saturating_sub(4)).into_iter().enumerate() {
            let head = if i == 0 {
                format!("  {sign} ")
            } else {
                "    ".to_string()
            };
            lines.push(Line::styled(format!("{head}{part}"), style));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw("Esc: back to the list"));
    lines
}

/// The keys of the screen, as `Watch::key`, `Choice::key` and `DiffView::key`
/// read them.
const KEYS: [(&str, &str); 13] = [
    ("Up, k", "one line up, or scroll up in a diff"),
    ("Down, j", "one line down, or scroll down in a diff"),
    ("Home, g", "the newest line, or the top of a diff"),
    ("End, G", "the end of a diff"),
    ("PgUp, PgDn", "a page up or down in a diff, also Space"),
    ("Enter", "what changed in the chosen write"),
    (
        "h, Left",
        "the next older change of the same file, also in a diff",
    ),
    (
        "l, Right",
        "the next newer change of the same file, also in a diff",
    ),
    ("r", "revert the chosen change, after a question"),
    (
        "y",
        "yes, revert. Any other key cancels, and nothing changes",
    ),
    ("Esc", "back from a diff, or close this help"),
    ("?", "open or close this help"),
    ("q, Ctrl+C", "quit"),
];

fn help_lines(info: &Info, width: usize) -> Vec<Line<'static>> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut lines = labelled("Version", crate::VERSION, width);
    lines.extend(labelled("Storage", info.storage, width));
    lines.extend(labelled("Log", &info.log, width));
    lines.push(Line::raw(""));
    lines.push(Line::styled("Keys", bold));
    const KEY: usize = 12;
    for (key, what) in KEYS {
        for (i, row) in wrap(what, width.saturating_sub(KEY))
            .into_iter()
            .enumerate()
        {
            let head = if i == 0 { key } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("{head:<KEY$}"), bold),
                Span::raw(row),
            ]));
        }
    }
    lines.push(Line::raw(""));
    if info.dropbox {
        let text = "On Dropbox, Enter and r get the versions of a write from the Dropbox history, in the background. Esc goes back at once. The versions stay in memory only, up to 25 MB, and are gone when watch quits.";
        lines.extend(plain(text, width, Style::new()));
        lines.push(Line::raw(""));
    }
    let text = "r works only on the newest change of a file with a before and an after: a write, a patch or a revert. After y, it writes the text from before that change, only if the file is still as the change left it. The log gets a reverted line, and r on it brings the change back.";
    lines.extend(plain(text, width, Style::new()));
    lines
}

/// A panel over the middle of the screen, at most `most` columns wide. `lines`
/// gets the width inside the border.
fn draw_panel(
    frame: &mut Frame,
    most: u16,
    block: Block<'static>,
    lines: impl FnOnce(usize) -> Vec<Line<'static>>,
) {
    let area = frame.area();
    let width = area.width.min(most);
    let lines = lines(usize::from(width.saturating_sub(2)));
    let height = (lines.len() as u16 + 2).min(area.height);
    let place = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, place);
    frame.render_widget(Paragraph::new(lines).block(block), place);
}

/// The help panel, over the middle of the screen.
pub fn draw_help(frame: &mut Frame, info: &Info) {
    let block = Block::bordered().title("Help   ?, Esc: close");
    draw_panel(frame, 72, block, |width| help_lines(info, width));
}

/// The lines of the revert question: what changes, then the two answers.
fn question_lines(q: &Question, width: usize) -> Vec<Line<'static>> {
    let change = format!("{} on {} by {}", q.outcome, q.time, q.who);
    let mut lines = vec![Line::raw("")];
    lines.extend(labelled("File", &q.path, width));
    lines.extend(labelled("Change", &change, width));
    lines.extend(labelled("Undo", &q.size(), width));
    lines.push(Line::raw(""));
    lines.extend(plain(
        "The file gets its text from before this change back.",
        width,
        Style::new(),
    ));
    lines.push(Line::raw(""));
    let bold = Modifier::BOLD;
    let answers = vec![
        Span::styled("y", Style::new().fg(ORANGE).add_modifier(bold)),
        Span::styled(": revert", Style::new().fg(ORANGE)),
        Span::raw("      "),
        Span::styled("any other key", Style::new().fg(BLUE).add_modifier(bold)),
        Span::styled(": cancel", Style::new().fg(BLUE)),
    ];
    let fits = answers
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>()
        <= width;
    if fits {
        lines.push(Line::from(answers));
    } else {
        lines.push(Line::styled(
            "y: revert",
            Style::new().fg(ORANGE).add_modifier(bold),
        ));
        lines.push(Line::styled("any other key: cancel", Style::new().fg(BLUE)));
    }
    lines
}

/// The revert question, as a popup over the list or the diff.
fn draw_question(frame: &mut Frame, q: &Question) {
    let title = Span::styled(
        " Revert this change? ",
        Style::new().fg(ORANGE).add_modifier(Modifier::BOLD),
    );
    let block = Block::bordered()
        .border_style(Style::new().fg(ORANGE))
        .padding(Padding::horizontal(1))
        .title(title);
    // The padding takes one column on each side.
    draw_panel(frame, 64, block, |width| {
        question_lines(q, width.saturating_sub(2))
    });
}

/// A revert on its way: the old text from the Dropbox history, the question,
/// or the write.
enum Pending {
    /// On Dropbox, the texts come from the history before the question.
    Fetching {
        entry: Box<Entry>,
        job: Job,
        steps_in: Receiver<Step>,
        started: Instant,
    },
    /// The question waits for `y`.
    Asking { plan: Plan, question: Question },
    /// The write runs, in the background on Dropbox.
    Writing {
        answer_in: Receiver<Result<String, String>>,
        started: Instant,
    },
}

/// A short answer on the last line, until the next key.
struct Note {
    text: String,
    good: bool,
}

/// All that the screen holds between two keys.
pub struct Watch {
    choice: Choice,
    open: Option<DiffView>,
    help: bool,
    /// `None` on a screen that cannot revert, as in a test of the drawing.
    reverter: Option<Reverter>,
    pending: Option<Pending>,
    note: Option<Note>,
    /// The zone of the times in the question.
    zone: jiff::tz::TimeZone,
}

impl Default for Watch {
    fn default() -> Watch {
        Watch::new(None)
    }
}

/// Why the old text of a revert did not come from the Dropbox history.
fn fetch_refusal(job: &Job, text: &str) -> String {
    if text == job.gone(true, true) || text == job.gone(true, false) {
        revert::NO_COPY.to_string()
    } else if text == job.gone(false, true) {
        // The text after the change is the file now, unless it changed since.
        revert::CHANGED.to_string()
    } else {
        format!("{text} Nothing changed.")
    }
}

impl Watch {
    pub fn new(reverter: Option<Reverter>) -> Watch {
        Watch {
            choice: Choice::default(),
            open: None,
            help: false,
            reverter,
            pending: None,
            note: None,
            zone: jiff::tz::TimeZone::system(),
        }
    }

    fn say(&mut self, text: &str, good: bool) {
        self.note = Some(Note {
            text: text.to_string(),
            good,
        });
    }

    /// Applies one key. True means quit.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, snap: &Snapshot, diffs: &Diffs) -> bool {
        if code == KeyCode::Char('q') || (ctrl && code == KeyCode::Char('c')) {
            return true;
        }
        if self.help {
            if matches!(code, KeyCode::Char('?') | KeyCode::Esc) {
                self.help = false;
            }
            return false;
        }
        // A question takes the next key: only y reverts, every other key cancels.
        match self.pending.take() {
            Some(Pending::Asking { plan, .. }) => {
                if code == KeyCode::Char('y') {
                    self.write(plan);
                } else {
                    self.say(revert::CANCELLED, false);
                }
                return false;
            }
            Some(Pending::Fetching { .. }) => {
                // The fetch goes on alone, into the cache, but asks nothing.
                self.say(revert::CANCELLED, false);
                return false;
            }
            other => self.pending = other,
        }
        self.note = None;
        let entries = &snap.entries;
        match code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('r') => self.ask(entries, diffs),
            KeyCode::Char('h') | KeyCode::Left => self.walk(true, entries, diffs),
            KeyCode::Char('l') | KeyCode::Right => self.walk(false, entries, diffs),
            _ => {
                if let Some(view) = &mut self.open {
                    if code == KeyCode::Enter && view.retry {
                        self.open = Some(DiffView::open(&view.entry.clone(), diffs));
                    } else if view.key(code) {
                        // Back at once, also during a wait: the fetch goes on alone.
                        self.open = None;
                    }
                } else if code == KeyCode::Enter {
                    if let Some(i) = self.choice.index(entries) {
                        // The diff and r name the same line, whatever arrives.
                        self.choice.pin(i, entries);
                        self.open = Some(DiffView::open(&entries[i], diffs));
                    }
                } else {
                    self.choice.key(code, ctrl, entries);
                }
            }
        }
        false
    }

    /// `h` and `l`: the next older or newer change of the same path, in the
    /// list and in an open diff. At both ends the choice stays.
    fn walk(&mut self, older: bool, entries: &[Entry], diffs: &Diffs) {
        let Some(i) = self.choice.index(entries) else {
            return;
        };
        let next = if older {
            revert::older(entries, i)
        } else {
            revert::newer(entries, i)
        };
        match next {
            Some(j) => {
                self.choice.pin(j, entries);
                if self.open.is_some() {
                    self.open = Some(DiffView::open(&entries[j], diffs));
                }
            }
            None if older => self.say(revert::NO_OLDER, false),
            None => self.say(revert::NO_NEWER, false),
        }
    }

    /// `r`: the checks, then the old text, then the question. Nothing changes
    /// before `y`.
    fn ask(&mut self, entries: &[Entry], diffs: &Diffs) {
        if matches!(self.pending, Some(Pending::Writing { .. })) {
            return self.say(revert::BUSY, false);
        }
        let Some(i) = self.choice.index(entries) else {
            return;
        };
        let entry = &entries[i];
        // A new file, a delete, a refusal: r does nothing.
        if !revert::can_revert(entry) {
            return;
        }
        // A revert is a write: the limits of the door hold, as for any write.
        if let Some(reverter) = &self.reverter
            && let Err(text) = reverter.check(&entry.path)
        {
            return self.say(&text, false);
        }
        if revert::newer(entries, i).is_some() {
            return self.say(revert::NEWER, false);
        }
        match diffs {
            Diffs::Kept(versions) => {
                let old = match versions.load(&entry.before) {
                    Ok(Some(bytes))
                        if versions.keeps_copies() && fingerprint(&bytes) == entry.before =>
                    {
                        bytes
                    }
                    Ok(_) => return self.say(revert::NO_COPY, false),
                    Err(e) => {
                        return self.say(
                            &format!("Could not read the copy: {e}. Nothing changed."),
                            false,
                        );
                    }
                };
                let new = versions.load(&entry.after).ok().flatten();
                self.asking(entry, old, new.as_deref());
            }
            Diffs::Dropbox(remote) => {
                let job = match Job::of(entry) {
                    Ok(job) => job,
                    Err(reason) => return self.say(&format!("{reason} Nothing changed."), false),
                };
                match remote.from_memory(&job) {
                    Some((old, new)) => self.asking(entry, old.into_bytes(), Some(new.as_bytes())),
                    None => {
                        self.pending = Some(Pending::Fetching {
                            entry: Box::new(entry.clone()),
                            steps_in: remote.start(job.clone()),
                            job,
                            started: Instant::now(),
                        });
                    }
                }
            }
        }
    }

    fn asking(&mut self, entry: &Entry, old: Vec<u8>, new: Option<&[u8]>) {
        let counts = new.map(|new| {
            revert::counts(
                &String::from_utf8_lossy(&old),
                &String::from_utf8_lossy(new),
            )
        });
        let question = revert::question(entry, counts, &self.zone);
        self.pending = Some(Pending::Asking {
            plan: Plan {
                path: entry.path.clone(),
                before: entry.before.clone(),
                after: entry.after.clone(),
                old,
            },
            question,
        });
    }

    /// `y`: the write, with its answer at once on the Mac.
    fn write(&mut self, plan: Plan) {
        let Some(reverter) = &self.reverter else {
            return self.say("This screen cannot revert. Nothing changed.", false);
        };
        self.pending = Some(Pending::Writing {
            answer_in: reverter.start(plan),
            started: Instant::now(),
        });
        self.tick_pending(Instant::now());
    }

    /// Takes in what a revert in the background told, and stops waiting after
    /// its limit.
    fn tick_pending(&mut self, now: Instant) {
        match self.pending.take() {
            Some(Pending::Fetching {
                entry,
                job,
                steps_in,
                started,
            }) => {
                let mut done = None;
                loop {
                    match steps_in.try_recv() {
                        Ok(Step::Done(result)) => {
                            done = Some(result);
                            break;
                        }
                        Ok(_) => {}
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            done = Some(Err("The fetch stopped with no answer.".to_string()));
                            break;
                        }
                    }
                }
                match done {
                    Some(Ok((old, new))) => {
                        self.asking(&entry, old.into_bytes(), Some(new.as_bytes()));
                    }
                    Some(Err(text)) => self.say(&fetch_refusal(&job, &text), false),
                    None if now.saturating_duration_since(started) >= history::WAIT => self.say(
                        "Dropbox did not answer in 20 seconds. Nothing changed. Press r to try again.",
                        false,
                    ),
                    None => {
                        self.pending = Some(Pending::Fetching {
                            entry,
                            job,
                            steps_in,
                            started,
                        });
                    }
                }
            }
            Some(Pending::Writing { answer_in, started }) => match answer_in.try_recv() {
                Ok(Ok(text)) => self.say(&text, true),
                Ok(Err(text)) => self.say(&text, false),
                Err(mpsc::TryRecvError::Disconnected) => self.say(
                    "The revert stopped with no answer. The log shows what happened.",
                    false,
                ),
                Err(mpsc::TryRecvError::Empty)
                    if now.saturating_duration_since(started) >= revert::WAIT =>
                {
                    self.say(revert::TIMEOUT, false);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending = Some(Pending::Writing { answer_in, started });
                }
            },
            other => self.pending = other,
        }
    }

    /// Takes in what a fetch or a revert in the background told. True while
    /// one runs.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.tick_pending(now);
        let view = match &mut self.open {
            Some(view) => {
                view.tick(now);
                view.waiting()
            }
            None => false,
        };
        view || matches!(
            self.pending,
            Some(Pending::Fetching { .. } | Pending::Writing { .. })
        )
    }

    /// The last lines in place of the keys: the question, a wait or an answer.
    fn foot(&self, width: u16, now: Instant) -> Option<Vec<Line<'static>>> {
        let width = usize::from(width);
        let spin = |started: &Instant| {
            let gone = now.saturating_duration_since(*started);
            let sign = SPINNER[(gone.as_millis() / SPIN.as_millis()) as usize % SPINNER.len()];
            (sign, gone.as_secs())
        };
        let lines = match &self.pending {
            Some(Pending::Asking { .. }) => plain(
                "y: revert   any other key: cancel",
                width,
                Style::new().fg(ORANGE).add_modifier(Modifier::BOLD),
            ),
            Some(Pending::Fetching { started, .. }) => {
                let (sign, secs) = spin(started);
                let text = format!(
                    "{sign} Getting the old version from the Dropbox history   {secs} s   any key: cancel"
                );
                plain(&text, width, Style::new())
            }
            Some(Pending::Writing { started, .. }) => {
                let (sign, secs) = spin(started);
                let mut lines = plain(&format!("{sign} Reverting   {secs} s"), width, Style::new());
                if let Some(note) = &self.note {
                    lines.extend(plain(&note.text, width, Style::new().fg(ORANGE)));
                }
                lines
            }
            None => {
                let note = self.note.as_ref()?;
                let color = if note.good { BLUE } else { ORANGE };
                plain(&note.text, width, Style::new().fg(color))
            }
        };
        Some(lines)
    }

    pub fn draw(&self, frame: &mut Frame, snap: &Snapshot, info: &Info) {
        let foot = self.foot(frame.area().width, Instant::now());
        match &self.open {
            Some(view) => draw_diff_with(frame, view, info, foot),
            None => draw_with(frame, snap, &self.choice, info, foot),
        }
        if let Some(Pending::Asking { question, .. }) = &self.pending {
            draw_question(frame, question);
        }
        if self.help {
            draw_help(frame, info);
        }
    }
}

/// Runs the screen until a quit key.
pub fn run(
    lock_dir: &Path,
    log: &Path,
    diffs: &Diffs,
    info: &Info,
    reverter: Reverter,
) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut watch = Watch::new(Some(reverter));
    let mut snap = snapshot(lock_dir, log);
    let mut taken = Instant::now();
    let result = loop {
        // The log is read once a second, also while the spinner turns faster.
        if taken.elapsed() >= REFRESH {
            snap = snapshot(lock_dir, log);
            taken = Instant::now();
        }
        let waiting = watch.tick(Instant::now());
        let drawn = terminal.draw(|frame| watch.draw(frame, &snap, info));
        if let Err(e) = drawn {
            break Err(e);
        }
        match event::poll(if waiting { SPIN } else { REFRESH }) {
            Ok(false) => {}
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if watch.key(key.code, ctrl, &snap, diffs) {
                        break Ok(());
                    }
                    // A key shows the newest log at once, as before.
                    snap = snapshot(lock_dir, log);
                    taken = Instant::now();
                }
                Ok(_) => {}
                Err(e) => break Err(e),
            },
            Err(e) => break Err(e),
        }
    };
    ratatui::restore();
    result
}

/// The same screen as plain text, once, for a chat or a log.
pub fn once(
    lock_dir: &Path,
    log: &Path,
    info: &Info,
    width: u16,
    height: u16,
) -> io::Result<String> {
    use ratatui::{Terminal, backend::TestBackend};
    let snap = snapshot(lock_dir, log);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).map_err(io::Error::other)?;
    terminal
        .draw(|frame| draw(frame, &snap, &Choice::default(), info))
        .map_err(io::Error::other)?;
    Ok(text_of(&terminal))
}

fn text_of(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    let mut out = String::new();
    for row in buffer.content.chunks(width) {
        let line: String = row.iter().map(|c| c.symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::{Lock, parse_line};
    use ratatui::{Terminal, backend::TestBackend};

    fn entry(outcome: &str, name: &str) -> Entry {
        parse_line(&format!(
            "2026-11-07 07:28:26Z\t{outcome}\torg/notes/{name}.md\tinvented chat\taaa\tbbb\t170"
        ))
        .unwrap()
    }

    fn local() -> Info {
        Info::new(false, Path::new("state/writes.log"))
    }

    fn diff_entry() -> Entry {
        entry("written", "a")
    }

    fn screen(snap: &Snapshot, choice: &Choice) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 28)).unwrap();
        t.draw(|f| draw(f, snap, choice, &local())).unwrap();
        text_of(&t)
    }

    fn watch_screen(watch: &Watch, snap: &Snapshot, info: &Info) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| watch.draw(f, snap, info)).unwrap();
        text_of(&t)
    }

    #[test]
    fn an_empty_screen_says_so() {
        let text = screen(&Snapshot::default(), &Choice::default());
        assert!(!text.contains("read only"), "{text}");
        assert!(text.contains("r, then y: revert"), "{text}");
        assert!(text.contains("none"));
        assert!(text.contains("No writes yet."));
    }

    #[test]
    fn every_screen_shows_the_version_of_the_build_on_its_first_line() {
        use crate::versions::Versions;
        let want = format!("orgthread {} · local folder", crate::VERSION);
        assert!(crate::VERSION.starts_with(env!("CARGO_PKG_VERSION")));
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        let commit = String::from_utf8(commit.stdout).unwrap();
        assert!(
            crate::VERSION.contains(&format!("({}", commit.trim())),
            "{} should name the commit {commit}",
            crate::VERSION
        );

        let text = screen(&Snapshot::default(), &Choice::default());
        assert!(text.lines().next().unwrap().contains(&want), "{text}");

        let none = Path::new("no-such-place");
        let text = once(none, none, &local(), 100, 28).unwrap();
        assert!(text.lines().next().unwrap().contains(&want), "{text}");

        let view = DiffView::open(&entry("written", "a"), &Diffs::Kept(Versions::none()));
        let mut t = Terminal::new(TestBackend::new(100, 28)).unwrap();
        t.draw(|f| draw_diff(f, &view, &local())).unwrap();
        let text = text_of(&t);
        assert!(text.lines().next().unwrap().contains(&want), "{text}");

        let dropbox = Info::new(true, Path::new("state/writes.log"));
        let text = once(none, none, &dropbox, 100, 28).unwrap();
        let want = format!("orgthread {} · Dropbox", crate::VERSION);
        assert!(text.lines().next().unwrap().contains(&want), "{text}");
    }

    #[test]
    fn question_mark_opens_the_help_and_question_mark_or_esc_closes_it() {
        let snap = Snapshot {
            entries: vec![entry("written", "a")],
            ..Snapshot::default()
        };
        let none = Diffs::Kept(Versions::none());
        let info = local();
        let mut w = Watch::default();
        assert!(!watch_screen(&w, &snap, &info).contains("Keys"));
        assert!(watch_screen(&w, &snap, &info).contains("?: help"));

        assert!(!w.key(KeyCode::Char('?'), false, &snap, &none));
        let text = watch_screen(&w, &snap, &info);
        assert!(text.contains("Help"), "{text}");
        assert!(text.contains(crate::VERSION), "{text}");
        assert!(text.contains("local folder"), "{text}");
        assert!(text.contains("state/writes.log"), "{text}");
        for (key, _) in KEYS {
            assert!(text.contains(key), "{key} is missing: {text}");
        }
        // Other keys do nothing while the help is open.
        w.key(KeyCode::Enter, false, &snap, &none);
        assert!(w.open.is_none());

        assert!(!w.key(KeyCode::Char('?'), false, &snap, &none));
        assert!(!watch_screen(&w, &snap, &info).contains("Keys"));

        w.key(KeyCode::Char('?'), false, &snap, &none);
        assert!(!w.key(KeyCode::Esc, false, &snap, &none));
        assert!(!watch_screen(&w, &snap, &info).contains("Keys"));

        // Over the diff too, and Esc closes the help before it leaves the diff.
        w.key(KeyCode::Enter, false, &snap, &none);
        assert!(w.open.is_some());
        w.key(KeyCode::Char('?'), false, &snap, &none);
        assert!(watch_screen(&w, &snap, &info).contains("Keys"));
        w.key(KeyCode::Esc, false, &snap, &none);
        assert!(w.open.is_some(), "the diff stays open");
        assert!(!watch_screen(&w, &snap, &info).contains("Keys"));
        w.key(KeyCode::Esc, false, &snap, &none);
        assert!(w.open.is_none());

        // q quits from the help too.
        w.key(KeyCode::Char('?'), false, &snap, &none);
        assert!(w.key(KeyCode::Char('q'), false, &snap, &none));
        assert!(w.key(KeyCode::Char('c'), true, &snap, &none));
    }

    #[test]
    fn the_help_shows_no_secret_even_when_the_settings_hold_one() {
        const URL: &str = "https://example.com/invented-secret-path";
        const HASH: &str = "invented-login-hash-7f3a";
        // No other unit test reads these two settings.
        unsafe {
            std::env::set_var("ORGTHREAD_PUBLIC_URL", URL);
            std::env::set_var("ORGTHREAD_LOGIN_HASH", HASH);
        }
        let snap = Snapshot::default();
        for dropbox in [false, true] {
            let info = Info::new(dropbox, Path::new("state/writes.log"));
            let mut w = Watch::default();
            w.key(
                KeyCode::Char('?'),
                false,
                &snap,
                &Diffs::Kept(Versions::none()),
            );
            let text = watch_screen(&w, &snap, &info);
            assert!(text.contains("Keys"), "{text}");
            for secret in ["example.com", "invented-secret-path", HASH, "token"] {
                assert!(!text.contains(secret), "{secret} shows: {text}");
            }
        }
        unsafe {
            std::env::remove_var("ORGTHREAD_PUBLIC_URL");
            std::env::remove_var("ORGTHREAD_LOGIN_HASH");
        }
    }

    #[test]
    fn the_help_fits_a_small_terminal() {
        let mut w = Watch::default();
        w.key(
            KeyCode::Char('?'),
            false,
            &Snapshot::default(),
            &Diffs::Kept(Versions::none()),
        );
        let mut t = Terminal::new(TestBackend::new(20, 5)).unwrap();
        t.draw(|f| w.draw(f, &Snapshot::default(), &local()))
            .unwrap();
        let mut t = Terminal::new(TestBackend::new(49, 40)).unwrap();
        t.draw(|f| w.draw(f, &Snapshot::default(), &local()))
            .unwrap();
        assert!(text_of(&t).contains("q, Ctrl+C"));
    }

    #[test]
    fn the_newest_line_is_first_and_chosen() {
        let snap = Snapshot {
            entries: vec![entry("written", "older"), entry("changed", "newer")],
            ..Snapshot::default()
        };
        let text = screen(&snap, &Choice::default());
        // Below the title, as "local folder" holds "older" too.
        let list = text.split_once('\n').unwrap().1;
        assert!(list.find("newer").unwrap() < list.find("older").unwrap());
        assert!(
            text.contains("Expected"),
            "the details follow the newest: {text}"
        );
        assert!(text.contains("Nothing was written."));
    }

    #[test]
    fn locks_and_leftovers_show() {
        let snap = Snapshot {
            locks: vec![Lock {
                path: "org/notes/a.md".into(),
                holder: "invented chat".into(),
                age: Duration::from_secs(1_000),
                stale: true,
            }],
            left_behind: vec!["org--notes--a.md.7.rescue".into()],
            ..Snapshot::default()
        };
        let text = screen(&snap, &Choice::default());
        assert!(text.contains("org/notes/a.md  since 16 min, STALE  invented chat"));
        assert!(text.contains("left behind: org--notes--a.md.7.rescue"));
    }

    fn three() -> Vec<Entry> {
        vec![
            entry("written", "a"),
            entry("written", "b"),
            entry("written", "c"),
        ]
    }

    #[test]
    fn the_keys_move_and_stay_inside() {
        let list = three();
        let mut c = Choice::default();
        assert!(!c.key(KeyCode::Up, false, &list));
        assert_eq!(c.place(&list), 0);
        c.key(KeyCode::Down, false, &list);
        c.key(KeyCode::Char('j'), false, &list);
        c.key(KeyCode::Down, false, &list);
        assert_eq!(c.place(&list), 2);
        c.key(KeyCode::Char('k'), false, &list);
        assert_eq!(c.place(&list), 1);
        c.key(KeyCode::Home, false, &list);
        assert_eq!(c.place(&list), 0);
        assert!(c.key(KeyCode::Char('q'), false, &list));
        assert!(
            !c.key(KeyCode::Esc, false, &list),
            "Esc never leaves the main screen"
        );
        assert!(c.key(KeyCode::Char('c'), true, &list));
        assert!(!c.key(KeyCode::Char('c'), false, &list));
        assert!(!c.key(KeyCode::Down, false, &[]), "an empty list is safe");
    }

    #[test]
    fn a_chosen_entry_stays_when_a_new_one_arrives() {
        let mut list = three();
        let mut c = Choice::default();
        c.key(KeyCode::Down, false, &list);
        assert_eq!(c.place(&list), 1);
        list.push(entry("written", "d"));
        assert_eq!(c.place(&list), 2, "one line further down, the same entry");
    }

    #[test]
    fn a_chosen_entry_stays_when_old_lines_leave_the_window() {
        // The log is read from its last 256 KB: a new line pushes old ones out.
        let mut list = three();
        let mut c = Choice::default();
        c.key(KeyCode::Down, false, &list);
        let chosen = list[1].clone();
        list.remove(0);
        list.push(entry("written", "d"));
        let place = c.place(&list);
        assert_eq!(
            list[list.len() - 1 - place],
            chosen,
            "the same line, at a new place"
        );
        // Two equal lines stay apart.
        let twins = vec![
            entry("written", "a"),
            entry("written", "a"),
            entry("written", "b"),
        ];
        let mut c = Choice::default();
        c.key(KeyCode::Down, false, &twins);
        c.key(KeyCode::Down, false, &twins);
        assert_eq!(c.place(&twins), 2);
        c.key(KeyCode::Up, false, &twins);
        assert_eq!(c.place(&twins), 1);
    }

    #[test]
    fn a_narrow_terminal_keeps_the_file_name_and_all_details() {
        let snap = Snapshot {
            entries: vec![
                parse_line(&format!(
                    "2026-11-07 07:28:26Z\tchanged\torg/claude-notes/2026-11-07-invented-long-name.md\tinvented chat\t{}\t{}\t-",
                    "a".repeat(64),
                    "b".repeat(64)
                ))
                .unwrap(),
            ],
            ..Snapshot::default()
        };
        // The width of Silviu's Terminal panel on the first try.
        let mut t = Terminal::new(TestBackend::new(49, 50)).unwrap();
        t.draw(|f| draw(f, &snap, &Choice::default(), &local()))
            .unwrap();
        let text = text_of(&t);
        assert!(text.contains("-invented-long-name.md"), "{text}");
        assert!(text.contains("Expected"), "{text}");
        assert!(text.contains("Found"), "{text}");
        let hexes = |c: char| text.chars().filter(|x| *x == c).count();
        assert!(hexes('b') >= 64, "the whole fingerprint shows: {text}");
        assert!(text.contains("q: quit"), "{text}");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("", 5), [""]);
    }

    fn diff_view(rows: Vec<Row>) -> DiffView {
        DiffView::with(&diff_entry(), Body::Ready(Ok(rows)))
    }

    fn diff_screen(view: &DiffView, width: u16, height: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| draw_diff(f, view, &local())).unwrap();
        text_of(&t)
    }

    #[test]
    fn the_diff_screen_shows_removed_and_added_lines_and_how_to_go_back() {
        let view = diff_view(vec![
            Row::Same {
                line: 9,
                text: "kept".into(),
            },
            Row::Removed {
                line: 10,
                text: "old line".into(),
            },
            Row::Added {
                line: 10,
                text: "new line".into(),
            },
        ]);
        let text = diff_screen(&view, 80, 12);
        assert!(text.contains("org/notes/a.md"), "{text}");
        assert!(text.contains("     9  kept"), "{text}");
        assert!(text.contains("-   10  old line"), "{text}");
        assert!(text.contains("+   10  new line"), "{text}");
        assert!(text.contains("Esc: back"), "{text}");
    }

    #[test]
    fn a_diff_with_no_copy_says_why() {
        let view = DiffView::with(
            &diff_entry(),
            Body::Ready(Err("No copy of this version.".to_string())),
        );
        assert!(diff_screen(&view, 80, 8).contains("No copy of this version."));
    }

    #[test]
    fn esc_goes_back_and_scrolling_stays_inside() {
        let rows = (1..=50)
            .map(|line| Row::Added {
                line,
                text: format!("line {line}"),
            })
            .collect();
        let mut view = diff_view(rows);
        let text = diff_screen(&view, 80, 12);
        assert!(text.contains("line 1"), "{text}");
        assert!(!text.contains("line 50"), "{text}");
        assert!(!view.key(KeyCode::Up));
        assert_eq!(view.scroll, 0);
        assert!(!view.key(KeyCode::End));
        let text = diff_screen(&view, 80, 12);
        assert!(text.contains("line 50"), "{text}");
        let max = view.scroll;
        assert!(!view.key(KeyCode::Down));
        assert_eq!(view.scroll, max);
        assert!(view.key(KeyCode::Esc));
    }

    #[test]
    fn a_long_diff_line_wraps_and_nothing_is_lost() {
        let view = diff_view(vec![Row::Added {
            line: 1,
            text: "x".repeat(100),
        }]);
        let text = diff_screen(&view, 49, 10);
        assert_eq!(text.matches('x').count(), 100, "{text}");
    }

    #[test]
    fn a_small_terminal_does_not_crash() {
        let snap = Snapshot {
            entries: vec![entry("written", "a")],
            ..Snapshot::default()
        };
        let mut t = Terminal::new(TestBackend::new(20, 5)).unwrap();
        t.draw(|f| draw(f, &snap, &Choice::default(), &local()))
            .unwrap();
    }

    mod dropbox {
        use super::*;
        use crate::fingerprint::fingerprint;
        use crate::history::tests::Fake;
        use crate::history::{History, Remote};
        use std::sync::{Arc, Mutex};

        const OLD: &str = "- buy apples\n- call Ana\n";
        const NEW: &str = "- buy apples\n- call Ana\n- water the plants\n";

        fn remote(fake: Fake) -> Diffs {
            let fake: Arc<dyn History> = Arc::new(fake);
            Diffs::Dropbox(Remote::new(Arc::new(move || Ok(Arc::clone(&fake)))))
        }

        fn written() -> Snapshot {
            let line = format!(
                "2026-11-09 08:00:00Z\twritten\torg/notes/plan.md\tinvented chat\t{}\t{}\t40",
                fingerprint(OLD.as_bytes()),
                fingerprint(NEW.as_bytes())
            );
            Snapshot {
                entries: vec![parse_line(&line).unwrap()],
                ..Snapshot::default()
            }
        }

        /// Ticks until the fetch is done, as the loop of `run` does.
        fn settle(w: &mut Watch) {
            for _ in 0..500 {
                if !w.tick(Instant::now()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the fetch never ended");
        }

        fn text(w: &Watch, snap: &Snapshot) -> String {
            let mut t = Terminal::new(TestBackend::new(90, 16)).unwrap();
            t.draw(|f| w.draw(f, snap, &Info::new(true, Path::new("state/writes.log"))))
                .unwrap();
            text_of(&t)
        }

        #[test]
        fn enter_on_dropbox_shows_the_diff_from_the_history() {
            let diffs = remote(Fake::with(&[OLD, NEW]));
            let snap = written();
            let mut w = Watch::default();
            w.key(KeyCode::Enter, false, &snap, &diffs);
            settle(&mut w);
            let screen = text(&w, &snap);
            assert!(screen.contains("+    3  - water the plants"), "{screen}");
            // Back, and Enter again: from memory, at once.
            w.key(KeyCode::Esc, false, &snap, &diffs);
            w.key(KeyCode::Enter, false, &snap, &diffs);
            assert!(
                !w.open.as_ref().unwrap().waiting(),
                "the second Enter is instant"
            );
        }

        #[test]
        fn esc_during_the_wait_goes_back_at_once() {
            let (open_gate, gate) = std::sync::mpsc::channel();
            let mut fake = Fake::with(&[OLD, NEW]);
            fake.gate = Some(Mutex::new(gate));
            let diffs = remote(fake);
            let snap = written();
            let mut w = Watch::default();
            w.key(KeyCode::Enter, false, &snap, &diffs);
            assert!(w.tick(Instant::now()), "the fetch waits at its gate");
            let screen = text(&w, &snap);
            assert!(
                screen.contains("Getting both versions from the Dropbox history"),
                "{screen}"
            );
            assert!(screen.contains("Esc: back to the list"), "{screen}");
            assert!(!w.key(KeyCode::Esc, false, &snap, &diffs));
            assert!(w.open.is_none(), "Esc is never blocked");
            open_gate.send(()).unwrap();
            open_gate.send(()).unwrap();
            // The late answer lands in the cache, so a new Enter opens at once.
            let Diffs::Dropbox(r) = &diffs else {
                unreachable!()
            };
            let job = Job::of(&snap.entries[0]).unwrap();
            for _ in 0..500 {
                if r.from_memory(&job).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            w.key(KeyCode::Enter, false, &snap, &diffs);
            assert!(!w.open.as_ref().unwrap().waiting());
        }

        #[test]
        fn the_wait_shows_its_steps_and_stops_after_20_seconds() {
            let (_open_gate, gate) = std::sync::mpsc::channel::<()>();
            let mut fake = Fake::with(&[OLD, NEW]);
            fake.gate = Some(Mutex::new(gate));
            let diffs = remote(fake);
            let snap = written();
            let mut w = Watch::default();
            w.key(KeyCode::Enter, false, &snap, &diffs);
            for _ in 0..500 {
                w.tick(Instant::now());
                if text(&w, &snap).contains("Downloading before and after") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let screen = text(&w, &snap);
            assert!(
                screen.contains("✓ Found 2 versions in the list of revisions"),
                "{screen}"
            );
            assert!(
                screen.contains("… Downloading before and after, at the same time"),
                "{screen}"
            );
            w.tick(Instant::now() + history::WAIT);
            let screen = text(&w, &snap);
            assert!(
                screen.contains("Dropbox did not answer in 20 seconds"),
                "{screen}"
            );
            // Enter tries again, and the screen waits once more.
            w.key(KeyCode::Enter, false, &snap, &diffs);
            assert!(w.open.as_ref().unwrap().waiting());
        }

        #[test]
        fn a_gone_version_is_named_on_the_screen() {
            let diffs = remote(Fake::with(&[NEW]));
            let snap = written();
            let mut w = Watch::default();
            w.key(KeyCode::Enter, false, &snap, &diffs);
            settle(&mut w);
            let screen = text(&w, &snap);
            assert!(
                screen.contains("The version before this write is gone"),
                "{screen}"
            );
            assert!(!screen.contains("buy apples"), "{screen}");
        }

        #[test]
        fn a_refused_line_shows_expected_against_found() {
            let diffs = remote(Fake::with(&[OLD, NEW]));
            let mut snap = written();
            snap.entries[0].outcome = "changed".into();
            snap.entries[0].size = String::new();
            let mut w = Watch::default();
            w.key(KeyCode::Enter, false, &snap, &diffs);
            settle(&mut w);
            let screen = text(&w, &snap);
            assert!(screen.contains("expected → found"), "{screen}");
            assert!(screen.contains("+    3  - water the plants"), "{screen}");
        }

        #[test]
        fn the_help_on_dropbox_says_what_enter_does() {
            let mut w = Watch::default();
            w.key(
                KeyCode::Char('?'),
                false,
                &Snapshot::default(),
                &remote(Fake::default()),
            );
            let mut t = Terminal::new(TestBackend::new(80, 40)).unwrap();
            t.draw(|f| {
                w.draw(
                    f,
                    &Snapshot::default(),
                    &Info::new(true, Path::new("state/writes.log")),
                )
            })
            .unwrap();
            let screen = text_of(&t);
            assert!(screen.contains("from the Dropbox"), "{screen}");
            assert!(screen.contains("25 MB"), "{screen}");
        }
    }

    /// The rules of "What a done build shows" in the design note of the
    /// revert, on invented files in a temporary folder.
    mod revert {
        use super::*;
        use crate::fingerprint::fingerprint;
        use crate::history::tests::Fake;
        use crate::history::{History, Remote};
        use crate::revert::{self as rv, OpenStorage, Reverter};
        use crate::storage::{LocalFolder, Storage};
        use std::fs;
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};

        const OLD: &str = "- buy apples\n- call Ana\n";
        const NEW: &str = "- buy apples\n- call Ana\n- water the plants\n";
        const PATH: &str = "org/notes/plan.md";
        /// An invented owner, as `ORGTHREAD_OWNER=Ana` gives it.
        const WHO: &str = "Ana (watch)";

        struct Place {
            dir: PathBuf,
        }

        impl Drop for Place {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.dir);
            }
        }

        impl Place {
            fn new(name: &str) -> Place {
                let dir = std::env::temp_dir().join(format!(
                    "orgthread-screen-revert-{name}-{}",
                    std::process::id()
                ));
                let _ = fs::remove_dir_all(&dir);
                fs::create_dir_all(dir.join("root/.locks")).unwrap();
                fs::create_dir_all(dir.join("root/org/notes")).unwrap();
                let place = Place { dir };
                place.put(NEW);
                place
            }
            fn root(&self) -> PathBuf {
                self.dir.join("root")
            }
            fn log(&self) -> PathBuf {
                self.dir.join("writes.log")
            }
            fn versions(&self) -> PathBuf {
                self.dir.join("versions")
            }
            fn put(&self, text: &str) {
                fs::write(self.root().join(PATH), text).unwrap();
            }
            fn file(&self) -> String {
                fs::read_to_string(self.root().join(PATH)).unwrap()
            }
            fn log_text(&self) -> String {
                fs::read_to_string(self.log()).unwrap_or_default()
            }
            /// A log line of a change from one text to the next. An empty
            /// text stands for no file.
            fn add(&self, outcome: &str, path: &str, before: &str, after: &str) {
                let print = |t: &str| {
                    if t.is_empty() {
                        "-".to_string()
                    } else {
                        fingerprint(t.as_bytes())
                    }
                };
                let line = format!(
                    "2026-11-10 09:14:26Z\t{outcome}\t{path}\tinvented chat\t{}\t{}\t{}\n",
                    print(before),
                    print(after),
                    after.len()
                );
                let mut log = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.log())
                    .unwrap();
                std::io::Write::write_all(&mut log, line.as_bytes()).unwrap();
            }
            fn snap(&self) -> Snapshot {
                snapshot(&self.root().join(".locks"), &self.log())
            }
            fn kept(&self) -> Diffs {
                let v = Versions::new(self.versions());
                v.save(OLD.as_bytes()).unwrap();
                v.save(NEW.as_bytes()).unwrap();
                Diffs::Kept(v)
            }
            fn open(&self) -> OpenStorage {
                let root = self.root();
                Arc::new(
                    move || Ok(Box::new(LocalFolder::new(&root, ".locks")) as Box<dyn Storage>),
                )
            }
            /// The Mac: one local write, with copies for the diff screen.
            fn mac(&self) -> Watch {
                Watch::new(Some(Reverter::new(
                    self.open(),
                    WHO.to_string(),
                    self.log(),
                    Some(self.versions()),
                    false,
                )))
            }
            /// The server: a write in the background, and no copies.
            fn server(&self) -> Watch {
                Watch::new(Some(Reverter::new(
                    self.open(),
                    WHO.to_string(),
                    self.log(),
                    None,
                    true,
                )))
            }
        }

        /// The Mac, with the limits of a door: the lists of `ORGTHREAD_ALLOW`,
        /// `ORGTHREAD_DENY` and `ORGTHREAD_READ_ONLY`.
        fn limited(place: &Place, allow: &str, deny: &str, read_only: &str) -> Reverter {
            let (allow, deny, read_only) =
                (allow.to_string(), deny.to_string(), read_only.to_string());
            Reverter::new(
                place.open(),
                WHO.to_string(),
                place.log(),
                Some(place.versions()),
                false,
            )
            .with_limits(Arc::new(move |path: &str| {
                crate::write::check_limits(path, true, &allow, &deny, &read_only)
            }))
        }

        /// r on a path the door may not write: the refusal, and no question,
        /// no write and no log line.
        fn refused(name: &str, allow: &str, deny: &str, read_only: &str, words: &str) {
            let place = Place::new(name);
            place.add("patched", PATH, OLD, NEW);
            let log = place.log_text();
            let snap = place.snap();
            let diffs = place.kept();
            let mut w = Watch::new(Some(limited(&place, allow, deny, read_only)));
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            let screen = shown(&w, &snap);
            assert!(!screen.contains("Revert this change?"), "{screen}");
            let text = format!("{PATH} {words}. Nothing changed.");
            assert!(screen.contains(&text), "{screen}");
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert_eq!(place.file(), NEW);
            assert_eq!(place.log_text(), log, "no log line");
        }

        #[test]
        fn r_on_a_read_only_path_is_refused() {
            refused(
                "read-only",
                "",
                "",
                "org/notes",
                "is read only for this door",
            );
        }

        #[test]
        fn r_on_a_closed_path_is_refused() {
            refused("deny", "", "Org/Notes", "", "is closed to this door");
        }

        #[test]
        fn r_outside_the_allowed_folders_is_refused() {
            refused(
                "allow",
                "org/tasks",
                "",
                "",
                "is outside the folders this door may touch",
            );
        }

        #[test]
        fn r_then_y_on_an_open_path_still_reverts_with_limits_set() {
            let place = Place::new("open");
            place.add("patched", PATH, OLD, NEW);
            let snap = place.snap();
            let diffs = place.kept();
            let mut w = Watch::new(Some(limited(&place, "org", "org/private", "org/tasks")));
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            assert!(shown(&w, &snap).contains("Revert this change?"));
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert_eq!(place.file(), OLD);
            is_reverted_back(&place, OLD.len());
        }

        #[test]
        fn the_write_checks_the_limits_again_before_the_lock() {
            // The limits may close between r and y; the write itself checks too.
            let place = Place::new("again");
            place.add("patched", PATH, OLD, NEW);
            let log = place.log_text();
            let plan = rv::Plan {
                path: PATH.to_string(),
                before: fingerprint(OLD.as_bytes()),
                after: fingerprint(NEW.as_bytes()),
                old: OLD.as_bytes().to_vec(),
            };
            let answer = limited(&place, "", "", "org").start(plan).recv().unwrap();
            assert_eq!(
                answer,
                Err(format!(
                    "{PATH} is read only for this door. Nothing changed."
                ))
            );
            assert_eq!(place.file(), NEW);
            assert_eq!(place.log_text(), log, "no log line");
            assert!(
                !place
                    .root()
                    .join(".locks")
                    .read_dir()
                    .unwrap()
                    .any(|_| true)
            );
        }

        fn dropbox(fake: Fake) -> Diffs {
            let fake: Arc<dyn History> = Arc::new(fake);
            Diffs::Dropbox(Remote::new(Arc::new(move || Ok(Arc::clone(&fake)))))
        }

        fn settle(w: &mut Watch) {
            for _ in 0..500 {
                if !w.tick(Instant::now()) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the wait never ended");
        }

        fn shown(w: &Watch, snap: &Snapshot) -> String {
            let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
            t.draw(|f| w.draw(f, snap, &local())).unwrap();
            text_of(&t)
        }

        fn last_line(place: &Place) -> Entry {
            parse_line(place.log_text().lines().last().unwrap()).unwrap()
        }

        fn is_reverted_back(place: &Place, size: usize) {
            let line = last_line(place);
            assert_eq!(line.outcome, "reverted");
            assert_eq!(line.path, PATH);
            assert_eq!(line.who, WHO);
            assert_eq!(line.before, fingerprint(NEW.as_bytes()), "the old after");
            assert_eq!(line.after, fingerprint(OLD.as_bytes()), "the old before");
            assert_eq!(line.size, size.to_string());
        }

        #[test]
        fn r_then_y_on_the_newest_patch_writes_the_old_text_back_on_the_mac() {
            let place = Place::new("mac");
            place.add("patched", PATH, OLD, NEW);
            let snap = place.snap();
            // Only the old text is kept here: the size of the diff is then unknown.
            Versions::new(place.versions())
                .save(OLD.as_bytes())
                .unwrap();
            let diffs = Diffs::Kept(Versions::new(place.versions()));
            let mut w = place.mac();
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            let screen = shown(&w, &snap);
            assert!(screen.contains("Revert this change?"), "{screen}");
            assert!(screen.contains(&format!("File      {PATH}")), "{screen}");
            assert!(screen.contains("Undo      not known"), "{screen}");
            assert_eq!(place.file(), NEW, "nothing before y");
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert_eq!(place.file(), OLD);
            is_reverted_back(&place, OLD.len());
            let screen = shown(&w, &snap);
            assert!(screen.contains(&format!("Reverted {PATH}.")), "{screen}");
            // Both texts are kept, as with a write.
            let v = Versions::new(place.versions());
            assert!(v.load(&fingerprint(NEW.as_bytes())).unwrap().is_some());
            assert!(v.load(&fingerprint(OLD.as_bytes())).unwrap().is_some());
        }

        #[test]
        fn r_then_y_on_dropbox_gets_the_old_text_from_the_history_and_writes_it_in_the_background()
        {
            let place = Place::new("dropbox");
            place.add("patched", PATH, OLD, NEW);
            let snap = place.snap();
            let diffs = dropbox(Fake::with(&[OLD, NEW]));
            let mut w = place.server();
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            assert!(w.tick(Instant::now()) || matches!(w.pending, Some(Pending::Asking { .. })));
            settle(&mut w);
            let screen = shown(&w, &snap);
            assert!(screen.contains("Undo      -1 +0 lines"), "{screen}");
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert!(
                matches!(w.pending, Some(Pending::Writing { .. })),
                "in the background"
            );
            settle(&mut w);
            assert_eq!(place.file(), OLD);
            is_reverted_back(&place, OLD.len());
            assert!(
                !place.versions().exists(),
                "a server keeps no copy on its disk"
            );
            assert!(shown(&w, &snap).contains("Reverted"));
        }

        #[test]
        fn the_question_shows_the_path_the_time_the_who_and_the_size_of_the_diff() {
            let place = Place::new("question");
            place.add("written", PATH, OLD, NEW);
            let (snap, diffs) = (place.snap(), place.kept());
            let mut w = place.mac();
            w.zone = jiff::tz::TimeZone::get("Europe/Vienna").unwrap();
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            let screen = shown(&w, &snap);
            assert!(screen.contains("Revert this change?"), "{screen}");
            assert!(screen.contains(&format!("File      {PATH}")), "{screen}");
            assert!(
                screen.contains("Change    written on 2026-11-10 10:14 by invented chat"),
                "{screen}"
            );
            assert!(screen.contains("Undo      -1 +0 lines"), "{screen}");
            assert!(
                screen.contains("y: revert      any other key: cancel"),
                "{screen}"
            );
            // The popup sits over the list, which stays below it.
            assert!(screen.contains("Latest writes and refusals"), "{screen}");
            // A cancel takes the popup away.
            w.key(KeyCode::Char('n'), false, &snap, &diffs);
            assert!(!shown(&w, &snap).contains("Revert this change?"));
            // In a diff, the popup shows over the diff.
            w.key(KeyCode::Enter, false, &snap, &diffs);
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            let screen = shown(&w, &snap);
            assert!(screen.contains("Revert this change?"), "{screen}");
            assert!(screen.contains("- buy apples"), "{screen}");
            // A narrow terminal keeps every part.
            let mut t = Terminal::new(TestBackend::new(40, 30)).unwrap();
            t.draw(|f| w.draw(f, &snap, &local())).unwrap();
            let narrow = text_of(&t);
            assert!(narrow.contains("invented chat"), "{narrow}");
            assert!(narrow.contains("-1 +0 lines"), "{narrow}");
            assert!(narrow.contains("cancel"), "{narrow}");
        }

        #[test]
        fn r_then_n_or_esc_or_any_other_key_changes_nothing_and_logs_nothing() {
            for key in [
                KeyCode::Char('n'),
                KeyCode::Esc,
                KeyCode::Char('x'),
                KeyCode::Enter,
            ] {
                let place = Place::new("cancel");
                place.add("patched", PATH, OLD, NEW);
                let (snap, diffs) = (place.snap(), place.kept());
                let before = place.log_text();
                let mut w = place.mac();
                w.key(KeyCode::Char('r'), false, &snap, &diffs);
                assert!(matches!(w.pending, Some(Pending::Asking { .. })));
                assert!(!w.key(key, false, &snap, &diffs));
                assert!(w.pending.is_none(), "{key:?}");
                assert!(w.open.is_none(), "{key:?} only cancels");
                assert_eq!(place.file(), NEW, "{key:?}");
                assert_eq!(place.log_text(), before, "{key:?}");
                assert!(shown(&w, &snap).contains("Nothing changed."), "{key:?}");
            }
        }

        #[test]
        fn r_on_an_older_change_shows_the_newer_change_and_l_goes_to_it() {
            let place = Place::new("older");
            let third = "- water the plants\n";
            place.add("patched", PATH, OLD, NEW);
            place.add("patched", PATH, NEW, third);
            place.add("written", "org/notes/other.md", "", "x\n");
            place.put(third);
            let (snap, diffs) = (place.snap(), place.kept());
            let mut w = place.mac();
            // Down twice: the older patch.
            w.key(KeyCode::Down, false, &snap, &diffs);
            w.key(KeyCode::Down, false, &snap, &diffs);
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            assert!(w.pending.is_none(), "no question");
            assert!(shown(&w, &snap).contains(rv::NEWER));
            assert_eq!(place.file(), third);
            w.key(KeyCode::Char('l'), false, &snap, &diffs);
            assert_eq!(w.choice.index(&snap.entries), Some(1));
        }

        #[test]
        fn a_file_changed_outside_orgthread_is_refused_and_stays_as_it_is() {
            for server in [false, true] {
                let place = Place::new("outside");
                place.add("patched", PATH, OLD, NEW);
                let snap = place.snap();
                let (mut w, diffs) = if server {
                    (place.server(), dropbox(Fake::with(&[OLD, NEW])))
                } else {
                    (place.mac(), place.kept())
                };
                w.key(KeyCode::Char('r'), false, &snap, &diffs);
                settle(&mut w);
                // An edit by hand, or on the other door, after the question.
                place.put("- edited by hand\n");
                w.key(KeyCode::Char('y'), false, &snap, &diffs);
                settle(&mut w);
                assert_eq!(place.file(), "- edited by hand\n");
                assert!(shown(&w, &snap).contains(rv::CHANGED), "{server}");
                let line = last_line(&place);
                assert_eq!(line.outcome, "changed", "a refusal is logged as one");
                assert!(!place.log_text().contains("\treverted\t"));
            }
        }

        #[test]
        fn no_kept_copy_is_refused_with_its_message() {
            let place = Place::new("nocopy");
            place.add("patched", PATH, OLD, NEW);
            let snap = place.snap();
            // The Mac, with no copy kept.
            let empty = Diffs::Kept(Versions::new(place.versions()));
            let mut w = place.mac();
            w.key(KeyCode::Char('r'), false, &snap, &empty);
            assert!(w.pending.is_none());
            assert!(shown(&w, &snap).contains(rv::NO_COPY));
            // A copy with a wrong text counts as none.
            fs::create_dir_all(place.versions()).unwrap();
            fs::write(
                place.versions().join(fingerprint(OLD.as_bytes())),
                "other\n",
            )
            .unwrap();
            w.key(KeyCode::Char('r'), false, &snap, &empty);
            assert!(shown(&w, &snap).contains(rv::NO_COPY));
            // Dropbox, where the history lost the old version.
            let mut w = place.server();
            let gone = dropbox(Fake::with(&[NEW]));
            w.key(KeyCode::Char('r'), false, &snap, &gone);
            settle(&mut w);
            assert!(w.pending.is_none());
            assert!(shown(&w, &snap).contains(rv::NO_COPY));
            assert_eq!(place.file(), NEW);
            assert!(!place.log_text().contains(WHO));
        }

        #[test]
        fn r_on_a_reverted_line_brings_the_change_back() {
            let place = Place::new("redo");
            place.add("patched", PATH, OLD, NEW);
            let diffs = place.kept();
            let mut w = place.mac();
            let snap = place.snap();
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert_eq!(place.file(), OLD);
            // The new snapshot has the reverted line as the newest.
            let snap = place.snap();
            assert_eq!(snap.entries.last().unwrap().outcome, "reverted");
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            let screen = shown(&w, &snap);
            assert!(screen.contains("by Ana (watch)"), "{screen}");
            assert!(screen.contains("Undo      -0 +1 lines"), "{screen}");
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert_eq!(place.file(), NEW, "the change is back");
            let line = last_line(&place);
            assert_eq!(line.outcome, "reverted");
            assert_eq!(line.before, fingerprint(OLD.as_bytes()));
            assert_eq!(line.after, fingerprint(NEW.as_bytes()));
            assert_eq!(place.log_text().matches("\treverted\t").count(), 2);
        }

        #[test]
        fn r_on_a_new_file_or_a_delete_does_nothing() {
            for (outcome, before, after) in [("written", "", NEW), ("deleted", NEW, "")] {
                let place = Place::new("nothing");
                place.add(outcome, PATH, before, after);
                let (snap, diffs) = (place.snap(), place.kept());
                let log = place.log_text();
                let mut w = place.mac();
                let quiet = shown(&w, &snap);
                w.key(KeyCode::Char('r'), false, &snap, &diffs);
                assert!(w.pending.is_none() && w.note.is_none(), "{outcome}");
                assert_eq!(shown(&w, &snap), quiet, "{outcome}");
                assert_eq!(place.log_text(), log);
                assert_eq!(place.file(), NEW);
            }
        }

        #[test]
        fn h_l_left_and_right_walk_only_one_path_and_stop_at_both_ends() {
            let place = Place::new("walk");
            place.add("written", "org/a.md", "", "a1\n");
            place.add("written", "org/b.md", "", "b1\n");
            place.add("changed", "org/a.md", "a0\n", "a1\n");
            place.add("patched", "org/a.md", "a1\n", "a2\n");
            place.add("written", "org/b.md", "b1\n", "b2\n");
            let snap = place.snap();
            let diffs = Diffs::Kept(Versions::none());
            let mut w = place.mac();
            let at = |w: &Watch| w.choice.index(&snap.entries).unwrap();
            assert_eq!(at(&w), 4);
            w.key(KeyCode::Char('h'), false, &snap, &diffs);
            assert_eq!(at(&w), 1, "the older b, past the lines of a");
            w.key(KeyCode::Left, false, &snap, &diffs);
            assert_eq!(at(&w), 1, "it stops at the oldest");
            assert!(shown(&w, &snap).contains(rv::NO_OLDER));
            w.key(KeyCode::Right, false, &snap, &diffs);
            assert_eq!(at(&w), 4);
            w.key(KeyCode::Char('l'), false, &snap, &diffs);
            assert_eq!(at(&w), 4, "it stops at the newest");
            assert!(shown(&w, &snap).contains(rv::NO_NEWER));
            // In an open diff, the diff follows, and the refusal is skipped.
            w.key(KeyCode::Down, false, &snap, &diffs);
            w.key(KeyCode::Enter, false, &snap, &diffs);
            assert_eq!(w.open.as_ref().unwrap().entry, snap.entries[3]);
            w.key(KeyCode::Char('h'), false, &snap, &diffs);
            assert_eq!(w.open.as_ref().unwrap().entry, snap.entries[0]);
            w.key(KeyCode::Left, false, &snap, &diffs);
            assert_eq!(w.open.as_ref().unwrap().entry, snap.entries[0]);
            w.key(KeyCode::Right, false, &snap, &diffs);
            assert_eq!(w.open.as_ref().unwrap().entry, snap.entries[3]);
            w.key(KeyCode::Esc, false, &snap, &diffs);
            assert!(w.open.is_none());
            assert_eq!(at(&w), 3, "back on the line of the diff");
        }

        #[test]
        fn a_revert_on_dropbox_stops_waiting_after_its_time_limit() {
            let place = Place::new("slow");
            place.add("patched", PATH, OLD, NEW);
            let snap = place.snap();
            let diffs = dropbox(Fake::with(&[OLD, NEW]));
            let (open_gate, gate) = std::sync::mpsc::channel::<()>();
            let gate = Mutex::new(gate);
            let open: OpenStorage = Arc::new(move || {
                let _ = gate.lock().unwrap().recv();
                Err("no answer".to_string())
            });
            let mut w = Watch::new(Some(Reverter::new(
                open,
                WHO.to_string(),
                place.log(),
                None,
                true,
            )));
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            settle(&mut w);
            w.key(KeyCode::Char('y'), false, &snap, &diffs);
            assert!(w.tick(Instant::now()), "the write waits at its gate");
            assert!(shown(&w, &snap).contains("Reverting"));
            // Keys still work while it waits.
            w.key(KeyCode::Char('?'), false, &snap, &diffs);
            w.key(KeyCode::Esc, false, &snap, &diffs);
            w.key(KeyCode::Char('r'), false, &snap, &diffs);
            assert!(shown(&w, &snap).contains(rv::BUSY));
            assert!(!w.tick(Instant::now() + rv::WAIT));
            assert!(shown(&w, &snap).contains("did not answer in 60 seconds"));
            open_gate.send(()).unwrap();
            assert_eq!(place.file(), NEW);
        }

        #[test]
        fn the_head_and_the_help_say_that_r_reverts_after_y() {
            let mut w = Watch::default();
            let snap = Snapshot::default();
            let head = shown(&w, &snap);
            assert!(
                head.lines().next().unwrap().contains("r, then y: revert"),
                "{head}"
            );
            assert!(!head.contains("read only"));
            w.key(
                KeyCode::Char('?'),
                false,
                &snap,
                &Diffs::Kept(Versions::none()),
            );
            let mut t = Terminal::new(TestBackend::new(72, 60)).unwrap();
            t.draw(|f| w.draw(f, &snap, &local())).unwrap();
            let help = text_of(&t);
            for key in ["h, Left", "l, Right", "r ", "y "] {
                assert!(help.contains(key), "{key}: {help}");
            }
            assert!(help.contains("reverted line"), "{help}");
            assert!(!help.contains("only reads"), "{help}");
        }
    }
}
