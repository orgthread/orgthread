//! The read-only terminal screen: `orgthread watch`. It draws what `watch.rs`
//! reads, and it has no way to change a file.

use crate::watch::{self, Entry, Snapshot, snapshot};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use std::io;
use std::path::Path;
use std::time::Duration;

const BLUE: Color = Color::Rgb(0x22, 0x6D, 0xBD);
const ORANGE: Color = Color::Rgb(0xE8, 0x74, 0x0C);
const REFRESH: Duration = Duration::from_secs(1);

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
            KeyCode::Char('q') | KeyCode::Esc => return true,
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

fn details(entry: &Entry) -> Vec<Line<'static>> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let row = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<10}"), bold),
            Span::raw(value),
        ])
    };
    let (first, second) = if entry.outcome == "changed" {
        ("Expected", "Found")
    } else {
        ("Before", "After")
    };
    let mut lines = vec![
        row("File", entry.path.clone()),
        row("Who", entry.who.clone()),
        row(
            "Time",
            format!("{} ({} ago)", entry.time, watch::ago(&entry.time)),
        ),
        Line::styled(
            watch::why(&entry.outcome),
            Style::new().fg(color_of(&entry.outcome)),
        ),
    ];
    if !entry.before.is_empty() {
        lines.push(row(first, entry.before.clone()));
    }
    if !entry.after.is_empty() {
        lines.push(row(second, entry.after.clone()));
    }
    lines
}

fn top_lines(snap: &Snapshot) -> Vec<Line<'static>> {
    let orange = Style::new().fg(ORANGE);
    let mut lines = Vec::new();
    for lock in &snap.locks {
        let state = if lock.stale { ", STALE" } else { "" };
        lines.push(Line::styled(
            format!(
                "{}  since {}{state}  {}",
                lock.path,
                watch::short_age(lock.age.as_secs()),
                lock.holder
            ),
            orange,
        ));
    }
    for name in &snap.left_behind {
        lines.push(Line::styled(format!("left behind: {name}"), orange));
    }
    for problem in &snap.problems {
        lines.push(Line::styled(format!("cannot read {problem}"), orange));
    }
    if lines.is_empty() {
        lines.push(Line::raw("none"));
    }
    lines
}

pub fn draw(frame: &mut Frame, snap: &Snapshot, choice: &Choice) {
    let top = top_lines(snap);
    let count = snap.entries.len();
    let place = choice.place(count);
    let chosen = snap.entries.iter().rev().nth(place);
    let detail = chosen.map_or_else(Vec::new, details);

    let [head, locks, list, info, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length((top.len() as u16 + 2).min(10)),
        Constraint::Min(4),
        Constraint::Length(detail.len() as u16 + 2),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let bold = Style::new().add_modifier(Modifier::BOLD);
    frame.render_widget(
        Line::from(vec![
            Span::styled("orgthread watch", bold),
            Span::raw("   read only"),
        ]),
        head,
    );
    frame.render_widget(
        Paragraph::new(top).block(Block::bordered().title("Locks held now")),
        locks,
    );

    let items: Vec<ListItem> = snap
        .entries
        .iter()
        .rev()
        .map(|e| {
            let text = format!(
                "{:>7} ago  {:<12}  {:>9}  {}",
                watch::ago(&e.time),
                e.outcome,
                if e.size.is_empty() {
                    String::new()
                } else {
                    format!("{} B", e.size)
                },
                e.path
            );
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
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title("Chosen line")),
        info,
    );
    frame.render_widget(
        Line::raw("up, down: choose a line    g: newest    q: quit"),
        foot,
    );
}

/// Runs the screen until a quit key.
pub fn run(lock_dir: &Path, log: &Path) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut choice = Choice::default();
    let result = loop {
        let snap = snapshot(lock_dir, log);
        if let Err(e) = terminal.draw(|frame| draw(frame, &snap, &choice)) {
            break Err(e);
        }
        match event::poll(REFRESH) {
            Ok(false) => {}
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if choice.key(key.code, ctrl, snap.entries.len()) {
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
pub fn once(lock_dir: &Path, log: &Path, width: u16, height: u16) -> io::Result<String> {
    use ratatui::{Terminal, backend::TestBackend};
    let snap = snapshot(lock_dir, log);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).map_err(io::Error::other)?;
    terminal
        .draw(|frame| draw(frame, &snap, &Choice::default()))
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

    fn screen(snap: &Snapshot, choice: &Choice) -> String {
        let mut t = Terminal::new(TestBackend::new(100, 28)).unwrap();
        t.draw(|f| draw(f, snap, choice)).unwrap();
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
    fn the_newest_line_is_first_and_chosen() {
        let snap = Snapshot {
            entries: vec![entry("written", "older"), entry("changed", "newer")],
            ..Snapshot::default()
        };
        let text = screen(&snap, &Choice::default());
        assert!(text.find("newer").unwrap() < text.find("older").unwrap());
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
        assert!(c.key(KeyCode::Esc, false, 3));
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
    fn a_small_terminal_does_not_crash() {
        let snap = Snapshot {
            entries: vec![entry("written", "a")],
            ..Snapshot::default()
        };
        let mut t = Terminal::new(TestBackend::new(20, 5)).unwrap();
        t.draw(|f| draw(f, &snap, &Choice::default())).unwrap();
    }
}
