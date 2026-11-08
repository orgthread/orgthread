//! The read-only terminal screen: `orgthread watch`. It draws what `watch.rs`
//! reads, and it has no way to change a file.

use crate::versions::Versions;
use crate::watch::{self, Entry, Row, Snapshot, snapshot};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use std::cell::Cell;
use std::io;
use std::path::Path;
use std::time::Duration;

const BLUE: Color = Color::Rgb(0x22, 0x6D, 0xBD);
const ORANGE: Color = Color::Rgb(0xE8, 0x74, 0x0C);
const REFRESH: Duration = Duration::from_secs(1);
/// What the screen tells about itself. Each part is fixed at build time, one word
/// from a short list, or the path of the log, so no address, secret path, token or
/// login hash can reach the screen through it.
pub struct Info {
    storage: &'static str,
    log: String,
}

impl Info {
    pub fn new(dropbox: bool, log: &Path) -> Info {
        Info {
            storage: crate::storage::kind(dropbox),
            log: log.display().to_string(),
        }
    }
}

/// The name, the version, the commit of this build and the kind of storage.
fn title(info: &Info) -> String {
    format!("orgthread {} · {}", crate::VERSION, info.storage)
}

fn head_line(info: &Info) -> Line<'static> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(title(info), bold),
        Span::raw("   read only   ?: help"),
    ])
}

/// Which line is chosen. `None` follows the newest line, and a number stays on
/// the same entry while new ones arrive.
#[derive(Default)]
pub struct Choice(Option<usize>);

impl Choice {
    /// The place on the screen, where 0 is the newest line.
    fn place(&self, count: usize) -> usize {
        match self.0 {
            Some(i) if i < count => count - 1 - i,
            _ => 0,
        }
    }

    fn set_place(&mut self, place: usize, count: usize) {
        self.0 = (place > 0 && place < count).then(|| count - 1 - place);
    }

    /// Applies one key. True means quit.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, count: usize) -> bool {
        let place = self.place(count);
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Up | KeyCode::Char('k') => self.set_place(place.saturating_sub(1), count),
            KeyCode::Down | KeyCode::Char('j') => {
                self.set_place((place + 1).min(count.saturating_sub(1)), count);
            }
            KeyCode::Home | KeyCode::Char('g') => self.0 = None,
            _ => {}
        }
        false
    }
}

fn color_of(outcome: &str) -> Color {
    if watch::is_written(outcome) {
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
    lines.extend(labelled("Who", &entry.who, width));
    let time = format!("{} ({} ago)", entry.time, watch::ago(&entry.time));
    lines.extend(labelled("Time", &time, width));
    let color = Style::new().fg(color_of(&entry.outcome));
    lines.extend(plain(watch::why(&entry.outcome), width, color));
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
    let width = usize::from(frame.area().width.saturating_sub(2));
    let top = top_lines(snap, width);
    let count = snap.entries.len();
    let place = choice.place(count);
    let chosen = snap.entries.iter().rev().nth(place);
    let detail = chosen.map_or_else(Vec::new, |e| details(e, width));

    let [head, locks, list, below, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length((top.len() as u16 + 2).min(10)),
        Constraint::Min(4),
        Constraint::Length(detail.len() as u16 + 2),
        Constraint::Length(1),
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
    frame.render_widget(
        Line::raw("up, down: line  enter: diff  ?: help  q: quit"),
        foot,
    );
}

/// The open diff screen.
pub struct DiffView {
    title: String,
    body: Result<Vec<Row>, String>,
    scroll: usize,
    /// How far it can scroll, known once it is drawn at the terminal's size.
    max_scroll: Cell<usize>,
}

impl DiffView {
    pub fn open(entry: &Entry, versions: &Versions) -> DiffView {
        DiffView {
            title: format!("{}   {} ago", entry.path, watch::ago(&entry.time)),
            body: watch::diff_of(entry, versions),
            scroll: 0,
            max_scroll: Cell::new(0),
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
    let width = usize::from(frame.area().width.saturating_sub(2));
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let lines: Vec<Line> = match &view.body {
        Ok(rows) => rows.iter().flat_map(|r| diff_lines(r, width)).collect(),
        Err(reason) => plain(reason, width, Style::new().fg(ORANGE)),
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
    frame.render_widget(
        Line::raw("up, down: scroll   Esc: back   ?: help   q: quit"),
        foot,
    );
}

/// The keys of the screen, as `Watch::key`, `Choice::key` and `DiffView::key`
/// read them.
const KEYS: [(&str, &str); 9] = [
    ("Up, k", "one line up, or scroll up in a diff"),
    ("Down, j", "one line down, or scroll down in a diff"),
    ("Home, g", "the newest line, or the top of a diff"),
    ("End, G", "the end of a diff"),
    ("PgUp, PgDn", "a page up or down in a diff, also Space"),
    ("Enter", "what changed in the chosen write"),
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
    lines.push(Line::raw("The screen only reads. It cannot change a file."));
    lines
}

/// The help panel, over the middle of the screen.
pub fn draw_help(frame: &mut Frame, info: &Info) {
    let area = frame.area();
    let width = area.width.min(72);
    let lines = help_lines(info, usize::from(width.saturating_sub(2)));
    let height = (lines.len() as u16 + 2).min(area.height);
    let place = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, place);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Help   ?, Esc: close")),
        place,
    );
}

/// All that the screen holds between two keys.
#[derive(Default)]
pub struct Watch {
    choice: Choice,
    open: Option<DiffView>,
    help: bool,
}

impl Watch {
    /// Applies one key. True means quit.
    pub fn key(&mut self, code: KeyCode, ctrl: bool, snap: &Snapshot, versions: &Versions) -> bool {
        if code == KeyCode::Char('q') || (ctrl && code == KeyCode::Char('c')) {
            return true;
        }
        if self.help {
            if matches!(code, KeyCode::Char('?') | KeyCode::Esc) {
                self.help = false;
            }
            return false;
        }
        if code == KeyCode::Char('?') {
            self.help = true;
        } else if let Some(view) = &mut self.open {
            if view.key(code) {
                self.open = None;
            }
        } else if code == KeyCode::Enter {
            let place = self.choice.place(snap.entries.len());
            if let Some(entry) = snap.entries.iter().rev().nth(place) {
                self.open = Some(DiffView::open(entry, versions));
            }
        } else {
            self.choice.key(code, ctrl, snap.entries.len());
        }
        false
    }

    pub fn draw(&self, frame: &mut Frame, snap: &Snapshot, info: &Info) {
        match &self.open {
            Some(view) => draw_diff(frame, view, info),
            None => draw(frame, snap, &self.choice, info),
        }
        if self.help {
            draw_help(frame, info);
        }
    }
}

/// Runs the screen until a quit key.
pub fn run(lock_dir: &Path, log: &Path, versions: &Versions, info: &Info) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut watch = Watch::default();
    let result = loop {
        let snap = snapshot(lock_dir, log);
        let drawn = terminal.draw(|frame| watch.draw(frame, &snap, info));
        if let Err(e) = drawn {
            break Err(e);
        }
        match event::poll(REFRESH) {
            Ok(false) => {}
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if watch.key(key.code, ctrl, &snap, versions) {
                        break Ok(());
                    }
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
        assert!(text.contains("read only"));
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

        let view = DiffView::open(&entry("written", "a"), &Versions::none());
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
        let none = Versions::none();
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
            w.key(KeyCode::Char('?'), false, &snap, &Versions::none());
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
            &Versions::none(),
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

    #[test]
    fn the_keys_move_and_stay_inside() {
        let mut c = Choice::default();
        assert!(!c.key(KeyCode::Up, false, 3));
        assert_eq!(c.place(3), 0);
        c.key(KeyCode::Down, false, 3);
        c.key(KeyCode::Char('j'), false, 3);
        c.key(KeyCode::Down, false, 3);
        assert_eq!(c.place(3), 2);
        c.key(KeyCode::Char('k'), false, 3);
        assert_eq!(c.place(3), 1);
        c.key(KeyCode::Home, false, 3);
        assert_eq!(c.place(3), 0);
        assert!(c.key(KeyCode::Char('q'), false, 3));
        assert!(
            !c.key(KeyCode::Esc, false, 3),
            "Esc never leaves the main screen"
        );
        assert!(c.key(KeyCode::Char('c'), true, 3));
        assert!(!c.key(KeyCode::Char('c'), false, 3));
        assert!(!c.key(KeyCode::Down, false, 0), "an empty list is safe");
    }

    #[test]
    fn a_chosen_entry_stays_when_a_new_one_arrives() {
        let mut c = Choice::default();
        c.key(KeyCode::Down, false, 3);
        assert_eq!(c.place(3), 1);
        assert_eq!(c.place(4), 2, "one line further down, the same entry");
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
        DiffView {
            title: "org/notes/a.md".to_string(),
            body: Ok(rows),
            scroll: 0,
            max_scroll: Cell::new(0),
        }
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
        let view = DiffView {
            title: "org/notes/a.md".to_string(),
            body: Err("No copy of this version.".to_string()),
            scroll: 0,
            max_scroll: Cell::new(0),
        };
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
}
