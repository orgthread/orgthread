//! `orgthread config`: the settings on a screen, in a real terminal only, and
//! `orgthread config --check`, as plain text. The screen changes one file: the
//! `.env` beside the program, through `envfile.rs`. It never runs a restart and
//! never shows a secret.

use crate::envfile::{self, EnvFile, Place, Unused};
use crate::screen::{BLUE, ORANGE, draw_panel, labelled, plain, wrap};
use crate::settings::{self, ALL, Group, Setting};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState, Padding, Paragraph};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The command that restarts the door on a server. The screen shows it and
/// never runs it.
pub const RESTART_SERVER: &str = "systemctl --user restart orgthread.service";

/// Where the value in effect of a setting comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The environment holds the name. It wins over `.env`.
    Environment(OsString),
    /// The first line of the name in `.env`.
    File(String),
    /// Neither: the default holds.
    Default,
}

impl Source {
    pub fn word(&self) -> &'static str {
        match self {
            Source::Environment(_) => "environment",
            Source::File(_) => ".env",
            Source::Default => "default",
        }
    }
}

/// The source of a setting, given the file and the names of the environment.
pub fn source_of(s: &Setting, file: &EnvFile, env: &[(String, OsString)]) -> Source {
    if let Some((_, value)) = env.iter().find(|(name, _)| name == s.name) {
        Source::Environment(value.clone())
    } else if let Some(value) = file.value(s.name) {
        Source::File(value.to_string())
    } else {
        Source::Default
    }
}

/// The reason when the value in effect is wrong.
pub fn problem_of(s: &Setting, source: &Source) -> Option<String> {
    match source {
        Source::Environment(value) => settings::check(s, value).err(),
        Source::File(value) => settings::check(s, value.as_ref()).err(),
        Source::Default => None,
    }
}

/// The value as the screen and the check show it, with no word on its
/// source. A secret shows only as set or not set.
pub fn value_text(s: &Setting, source: &Source) -> String {
    let value = match source {
        Source::Environment(value) => Some(value.to_string_lossy().into_owned()),
        Source::File(value) => Some(value.clone()),
        Source::Default => None,
    };
    match value {
        _ if s.secret => {
            let set = value.is_some_and(|v| !v.is_empty());
            if set {
                "secret: set"
            } else {
                "secret: not set"
            }
            .to_string()
        }
        Some(v) if v.is_empty() => "(empty)".to_string(),
        Some(v) => v,
        None => s.default.to_string(),
    }
}

/// The value with its source, as one row of the screen shows it.
fn row_value(s: &Setting, source: &Source) -> String {
    let value = value_text(s, source);
    match source {
        Source::Environment(_) => format!("{value} (from the environment)"),
        Source::Default if !s.secret => format!("{value} (default)"),
        _ => value,
    }
}

/// The text of `orgthread config --check`, and true when every value is right.
pub fn check(file: &EnvFile, env: &[(String, OsString)], env_path: &Path) -> (String, bool) {
    const NAME: usize = 30;
    let mut out = format!(
        "orgthread {} · settings of {}\n",
        crate::VERSION,
        env_path.display()
    );
    let mut wrong = 0;
    for group in Group::ALL {
        out.push_str(&format!("\n{}\n", group.title()));
        for s in ALL.iter().filter(|s| s.group == group) {
            let source = source_of(s, file, env);
            out.push_str(&format!(
                "  {:<NAME$} {}  ({})\n",
                s.name,
                value_text(s, &source),
                source.word()
            ));
            if let Some(reason) = problem_of(s, &source) {
                wrong += 1;
                out.push_str(&format!("    wrong: {reason}\n"));
            }
        }
    }
    let unused = file.unused();
    if !unused.is_empty() {
        out.push_str("\nNot used\n");
        for u in &unused {
            out.push_str(&format!("  {:<NAME$} {}\n", u.name, unused_words(u)));
        }
    }
    out.push('\n');
    if wrong == 0 {
        out.push_str(&format!("All {} settings are fine.\n", ALL.len()));
    } else {
        out.push_str(&format!(
            "{wrong} wrong. Every command but config stops with code 2 until it is fixed.\n"
        ));
    }
    (out, wrong == 0)
}

fn unused_words(u: &Unused) -> &'static str {
    if u.again {
        "not used: a second line, the first one wins"
    } else {
        "not used: no setting has this name"
    }
}

/// The places a save needs, and the kind of door for the restart line.
pub struct Paths {
    pub env: PathBuf,
    pub log: PathBuf,
    /// True on a server with the Dropbox API.
    pub dropbox: bool,
}

enum Mode {
    List,
    /// The new value, as typed so far.
    Edit(String),
    Help,
    /// The question before a quit with changes not saved.
    Quit,
}

/// One line of the answer on the last lines, blue when good, else orange.
struct Note(Vec<(String, bool)>);

/// All that the screen holds between two keys.
pub struct Config {
    /// The file as the screen changes it.
    file: EnvFile,
    /// The file as it is on the disk: at the start, or after the last save.
    loaded: EnvFile,
    /// The names that the environment holds, which `.env` cannot change.
    env: Vec<(String, OsString)>,
    /// The value of `ORGTHREAD_OWNER` in effect, for the log.
    owner: Option<String>,
    /// The index in `ALL` of the chosen setting.
    chosen: usize,
    mode: Mode,
    note: Option<Note>,
    paths: Paths,
}

impl Config {
    pub fn new(
        file: EnvFile,
        env: Vec<(String, OsString)>,
        owner: Option<String>,
        paths: Paths,
    ) -> Config {
        Config {
            loaded: file.clone(),
            file,
            env,
            owner,
            chosen: 0,
            mode: Mode::List,
            note: None,
            paths,
        }
    }

    fn setting(&self) -> &'static Setting {
        &ALL[self.chosen]
    }

    fn source(&self, s: &Setting) -> Source {
        source_of(s, &self.file, &self.env)
    }

    fn in_environment(&self, name: &str) -> bool {
        self.env.iter().any(|(n, _)| n == name)
    }

    fn say(&mut self, text: &str, good: bool) {
        self.note = Some(Note(vec![(text.to_string(), good)]));
    }

    /// The settings changed and not saved yet.
    pub fn unsaved(&self) -> Vec<&'static str> {
        self.file.changed(&self.loaded)
    }

    /// Applies one key. True means quit.
    pub fn key(&mut self, code: KeyCode, ctrl: bool) -> bool {
        match self.mode {
            Mode::Quit => {
                if code == KeyCode::Char('y') {
                    return true;
                }
                self.mode = Mode::List;
                self.say("Not quit. The changes wait for s.", true);
                return false;
            }
            Mode::Help => {
                match code {
                    KeyCode::Char('?') | KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Char('q') => return self.quit(),
                    KeyCode::Char('c') if ctrl => return self.quit(),
                    _ => {}
                }
                return false;
            }
            Mode::Edit(_) => {
                self.edit_key(code, ctrl);
                return false;
            }
            Mode::List => {}
        }
        self.note = None;
        match code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('c') if ctrl => return self.quit(),
            KeyCode::Up | KeyCode::Char('k') => self.chosen = self.chosen.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.chosen = (self.chosen + 1).min(ALL.len() - 1)
            }
            KeyCode::Home | KeyCode::Char('g') => self.chosen = 0,
            KeyCode::End | KeyCode::Char('G') => self.chosen = ALL.len() - 1,
            KeyCode::Enter => self.edit(),
            KeyCode::Char('d') => self.reset(),
            KeyCode::Char('s') => self.save(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        false
    }

    /// A key while a value is typed: Esc and Ctrl+C go back, Enter keeps it.
    fn edit_key(&mut self, code: KeyCode, ctrl: bool) {
        let back = code == KeyCode::Esc || (ctrl && code == KeyCode::Char('c'));
        if back {
            self.mode = Mode::List;
            return self.say("Nothing changed.", true);
        }
        if code == KeyCode::Enter {
            return self.keep();
        }
        let Mode::Edit(typed) = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Backspace => {
                typed.pop();
            }
            KeyCode::Char(c) if !ctrl => typed.push(c),
            _ => {}
        }
    }

    fn quit(&mut self) -> bool {
        if self.unsaved().is_empty() {
            return true;
        }
        self.mode = Mode::Quit;
        false
    }

    /// The words when the environment holds the chosen setting.
    fn locked(&mut self) -> bool {
        let name = self.setting().name;
        if self.in_environment(name) {
            self.say(
                &format!("{name} comes from the environment, which wins over .env. Change it where the environment is set."),
                false,
            );
            return true;
        }
        false
    }

    /// Enter: a new value. A secret starts empty, so the old one never shows.
    fn edit(&mut self) {
        if self.locked() {
            return;
        }
        let s = self.setting();
        let start = if s.secret {
            String::new()
        } else {
            self.file.value(s.name).unwrap_or_default().to_string()
        };
        self.mode = Mode::Edit(start);
    }

    /// Enter in the edit: checks the value and keeps it in the file on the
    /// screen. Only `s` writes it to the disk.
    fn keep(&mut self) {
        let Mode::Edit(typed) = &self.mode else {
            return;
        };
        let value = typed.trim().to_string();
        let s = self.setting();
        if value.is_empty() {
            if s.secret {
                self.mode = Mode::List;
                return self.say("Nothing changed. d takes the secret away.", true);
            }
            self.mode = Mode::List;
            return self.reset();
        }
        if let Err(reason) = settings::check(s, value.as_ref()) {
            // The typing stays, so the value can be put right.
            return self.say(&reason, false);
        }
        self.file.set(s.name, &value);
        self.mode = Mode::List;
        self.note = None;
    }

    /// `d`: back to the default. The line goes from the file on the screen.
    fn reset(&mut self) {
        if self.locked() {
            return;
        }
        let s = self.setting();
        self.file.remove(s.name);
        self.say(
            &format!("{} is back to its default: {}.", s.name, s.default),
            true,
        );
    }

    /// `s`: every value is checked, then the file is saved with a copy and a
    /// log line per changed name.
    fn save(&mut self) {
        let changed = self.unsaved();
        if changed.is_empty() {
            return self.say("Nothing to save: no setting changed.", true);
        }
        let who = match settings::name_of(self.owner.as_deref()) {
            Ok(name) => format!("{name} (config)"),
            Err(reason) => return self.say(&format!("Nothing saved. {reason}."), false),
        };
        let (stamp, now) = (envfile::stamp_now(), crate::log::utc_now());
        let place = Place {
            file: &self.paths.env,
            log: &self.paths.log,
            who: &who,
            stamp: &stamp,
            now: &now,
        };
        let env = &self.env;
        let skip = |name: &str| env.iter().any(|(n, _)| n == name);
        match envfile::save(&place, &self.loaded, &self.file, &skip) {
            Ok(saved) => {
                self.loaded = self.file.clone();
                let mut lines = vec![(format!("Saved: {}.", saved.changed.join(", ")), true)];
                lines.push(match &saved.copy {
                    Some(copy) => (
                        format!(
                            "The old file stays as {}.",
                            copy.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        true,
                    ),
                    None => (
                        "There was no .env before, so there is no copy.".to_string(),
                        true,
                    ),
                });
                lines.push((self.restart().to_string(), true));
                lines.extend(saved.log_failures.into_iter().map(|f| (f, false)));
                self.note = Some(Note(lines));
            }
            Err(reason) => self.say(&format!("Nothing saved. {reason}."), false),
        }
    }

    /// What starts the new values. The screen never runs it.
    fn restart(&self) -> String {
        if self.paths.dropbox {
            format!("To use them, restart the door yourself: {RESTART_SERVER}")
        } else {
            "Each new command reads .env again. Restart the desktop app, so that its MCP server reads it too.".to_string()
        }
    }

    /// The rows of the list, and the row of the chosen setting.
    fn items(&self, width: usize) -> (Vec<ListItem<'static>>, usize) {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let wide = width >= 72;
        const NAME: usize = 31;
        let changed = self.unsaved();
        let mut items = Vec::new();
        let mut chosen_row = 0;
        for group in Group::ALL {
            items.push(ListItem::new(Line::styled(group.title().to_string(), bold)));
            for (i, s) in ALL.iter().enumerate().filter(|(_, s)| s.group == group) {
                if i == self.chosen {
                    chosen_row = items.len();
                }
                let source = self.source(s);
                let mark = if changed.contains(&s.name) {
                    "* "
                } else {
                    "  "
                };
                let value = row_value(s, &source);
                let mut lines: Vec<Line<'static>> = Vec::new();
                if wide {
                    for (n, row) in wrap(&value, width - NAME).into_iter().enumerate() {
                        let head = if n == 0 {
                            format!("{mark}{}", s.name)
                        } else {
                            String::new()
                        };
                        lines.push(Line::raw(format!("{head:<NAME$}{row}")));
                    }
                } else {
                    lines.push(Line::raw(format!("{mark}{}", s.name)));
                    for row in wrap(&value, width.saturating_sub(4)) {
                        lines.push(Line::raw(format!("    {row}")));
                    }
                }
                let style = if problem_of(s, &source).is_some() {
                    Style::new().fg(ORANGE)
                } else if changed.contains(&s.name) {
                    Style::new().fg(BLUE)
                } else {
                    Style::new()
                };
                items.push(ListItem::new(Text::from(lines)).style(style));
            }
        }
        let unused = self.file.unused();
        if !unused.is_empty() {
            items.push(ListItem::new(Line::styled("Not used", bold)));
            for u in &unused {
                let mut lines = vec![Line::raw(format!("  {}", u.name))];
                for row in wrap(unused_words(u), width.saturating_sub(4)) {
                    lines.push(Line::raw(format!("    {row}")));
                }
                items.push(ListItem::new(Text::from(lines)).style(Style::new().fg(ORANGE)));
            }
        }
        (items, chosen_row)
    }

    /// The panel below the list: all about the chosen setting.
    fn details(&self, width: usize) -> Vec<Line<'static>> {
        let s = self.setting();
        let source = self.source(s);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut lines = plain(s.name, width, bold);
        lines.extend(plain(s.help, width, Style::new()));
        lines.extend(labelled("Value", &row_value(s, &source), width));
        lines.extend(labelled("Default", s.default, width));
        lines.extend(labelled("Right", s.kind.rule(), width));
        lines.extend(labelled("Doors", s.doors.words(), width));
        if let Source::Environment(_) = source {
            lines.extend(plain(
                "The environment holds it, and wins over .env: it cannot be changed here.",
                width,
                Style::new().fg(ORANGE),
            ));
        }
        if let Some(reason) = problem_of(s, &source) {
            lines.extend(plain(
                &format!("Wrong: {reason}"),
                width,
                Style::new().fg(ORANGE),
            ));
        }
        lines
    }

    /// The last lines: the typing, an answer, or the keys.
    fn foot(&self, width: u16) -> Vec<Line<'static>> {
        let w = usize::from(width);
        let mut lines = Vec::new();
        if let Mode::Edit(typed) = &self.mode {
            let s = self.setting();
            // A secret shows nothing while it is typed or pasted, not even
            // its length: the line stays the same for every sign.
            let shown = if s.secret { "" } else { typed.as_str() };
            let bold = Style::new().add_modifier(Modifier::BOLD);
            lines.extend(plain(&format!("New value: {shown}_"), w, bold));
            let keys = if s.secret {
                "enter: keep  esc: back. The typing is hidden."
            } else {
                "enter: keep  esc: back  empty: default"
            };
            lines.extend(plain(keys, w, Style::new()));
        }
        if let Some(Note(notes)) = &self.note {
            for (text, good) in notes {
                let color = if *good { BLUE } else { ORANGE };
                lines.extend(plain(text, w, Style::new().fg(color)));
            }
        }
        if lines.is_empty() {
            let keys = if width >= 80 {
                "up, down: setting  enter: change  d: default  s: save  ?: help  q: quit"
            } else {
                "enter: change  s: save  ?: help  q: quit"
            };
            lines.extend(plain(keys, w, Style::new()));
        }
        lines
    }

    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = usize::from(area.width.saturating_sub(2));
        let foot = self.foot(area.width);
        let details = self.details(width);
        let most = (area.height / 3).max(4);
        let [head, list, below, foot_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length((details.len() as u16 + 2).min(most)),
            Constraint::Length(foot.len() as u16),
        ])
        .areas(area);

        let bold = Style::new().add_modifier(Modifier::BOLD);
        let title = format!("orgthread {} · settings", crate::VERSION);
        let head_line = if area.width >= 60 {
            Line::from(vec![
                Span::styled(title, bold),
                Span::raw("   s: save   ?: help"),
            ])
        } else {
            Line::styled(title, bold)
        };
        frame.render_widget(head_line, head);

        let (items, chosen_row) = self.items(width);
        let count = self.unsaved().len();
        let title = if count == 0 {
            "Settings in .env".to_string()
        } else {
            format!("Settings in .env ({count} not saved)")
        };
        let widget = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default().with_selected(Some(chosen_row));
        frame.render_stateful_widget(widget, list, &mut state);

        frame.render_widget(
            Paragraph::new(details).block(Block::bordered().title("Chosen setting")),
            below,
        );
        frame.render_widget(Paragraph::new(foot), foot_area);

        match self.mode {
            Mode::Help => {
                let block = Block::bordered().title("Help   ?, Esc: close");
                draw_panel(frame, 72, block, |width| self.help_lines(width));
            }
            Mode::Quit => self.draw_quit(frame),
            _ => {}
        }
    }

    fn help_lines(&self, width: usize) -> Vec<Line<'static>> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut lines = labelled("Version", crate::VERSION, width);
        lines.extend(labelled(
            "File",
            &self.paths.env.display().to_string(),
            width,
        ));
        lines.extend(labelled(
            "Log",
            &self.paths.log.display().to_string(),
            width,
        ));
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
        for text in HELP {
            lines.push(Line::raw(""));
            lines.extend(plain(text, width, Style::new()));
        }
        lines
    }

    fn draw_quit(&self, frame: &mut Frame) {
        let title = Span::styled(
            " Quit without saving? ",
            Style::new().fg(ORANGE).add_modifier(Modifier::BOLD),
        );
        let block = Block::bordered()
            .border_style(Style::new().fg(ORANGE))
            .padding(Padding::horizontal(1))
            .title(title);
        let names = self.unsaved().join(", ");
        draw_panel(frame, 64, block, |width| {
            let width = width.saturating_sub(2);
            let mut lines = vec![Line::raw("")];
            lines.extend(plain(&format!("Not saved: {names}."), width, Style::new()));
            lines.push(Line::raw(""));
            let bold = Modifier::BOLD;
            lines.extend(plain(
                "y: quit, and the changes are lost",
                width,
                Style::new().fg(ORANGE).add_modifier(bold),
            ));
            lines.extend(plain(
                "any other key: stay, then s saves",
                width,
                Style::new().fg(BLUE),
            ));
            lines
        });
    }
}

const KEYS: [(&str, &str); 7] = [
    ("up, k", "the setting above"),
    ("down, j", "the setting below"),
    (
        "Enter",
        "change the value. Enter again keeps it, Esc goes back. An empty value is the default",
    ),
    ("d", "back to the default: the line goes from .env"),
    (
        "s",
        "save: check every value, keep the old file as a copy, write, and log",
    ),
    ("?, Esc", "open or close this help"),
    ("q, Ctrl+C", "quit. With a change not saved, it asks first"),
];

const HELP: [&str; 4] = [
    "A secret shows only as set or not set. Enter replaces it. The typing shows nothing, not even its length. The old value is never shown, never logged and never listed.",
    "A setting that the environment holds wins over .env, and cannot be changed here. A name of .env that no setting has, such as a typo, shows at the bottom as not used. Such lines and the comments stay in place.",
    "s keeps the old file as .env.before-config-<date>-<time>, the way back by hand. Then it writes the new file in one step, with the rights 600, and logs one configured line per changed name, never a value.",
    "The screen shows how to restart, and never restarts anything itself.",
];

/// Runs the screen until it quits.
pub fn run(mut config: Config) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = loop {
        if let Err(e) = terminal.draw(|frame| config.draw(frame)) {
            break Err(e);
        }
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if config.key(key.code, ctrl) {
                    break Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::text_of;
    use crate::settings::{DENY, DROPBOX_TOKEN, LOGIN_HASH, OWNER, ROOT, STORAGE, UPLOAD_MB};
    use ratatui::{Terminal, backend::TestBackend};
    use std::fs;

    const TOKEN: &str = "invented-secret-token-91f2";

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Folder {
            let dir = std::env::temp_dir()
                .join(format!("orgthread-config-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Folder(fs::canonicalize(dir).unwrap())
        }
        fn paths(&self, dropbox: bool) -> Paths {
            Paths {
                env: self.0.join(".env"),
                log: self.0.join("state/writes.log"),
                dropbox,
            }
        }
        fn config(&self, text: &str, env: &[(&str, &str)]) -> Config {
            fs::write(self.0.join(".env"), text).unwrap();
            let file = EnvFile::read(&self.0.join(".env")).unwrap();
            let env = env
                .iter()
                .map(|(n, v)| (n.to_string(), OsString::from(v)))
                .collect();
            Config::new(file, env, Some("Ana".into()), self.paths(false))
        }
        fn env_text(&self) -> String {
            fs::read_to_string(self.0.join(".env")).unwrap()
        }
        fn log(&self) -> String {
            fs::read_to_string(self.0.join("state/writes.log")).unwrap_or_default()
        }
    }

    fn screen(c: &Config, width: u16, height: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| c.draw(f)).unwrap();
        text_of(&t)
    }

    fn keys(c: &mut Config, text: &str) {
        for ch in text.chars() {
            c.key(KeyCode::Char(ch), false);
        }
    }

    fn choose(c: &mut Config, name: &str) {
        c.key(KeyCode::Char('g'), false);
        while c.setting().name != name {
            c.key(KeyCode::Char('j'), false);
        }
    }

    fn type_value(c: &mut Config, name: &str, value: &str) {
        choose(c, name);
        c.key(KeyCode::Enter, false);
        // Clear what the edit starts with.
        for _ in 0..200 {
            c.key(KeyCode::Backspace, false);
        }
        keys(c, value);
        c.key(KeyCode::Enter, false);
    }

    const FILE: &str = "# invented door\nORGTHREAD_OWNER=Ana\nORGTHREAD_OWNR=typo\nORGTHREAD_DROPBOX_TOKEN=invented-secret-token-91f2\n";

    #[test]
    fn the_screen_shows_every_setting_in_its_group() {
        let f = Folder::new("groups");
        let c = f.config(FILE, &[]);
        let text = screen(&c, 110, 80);
        let mut at = 0;
        for group in Group::ALL {
            let found = text[at..]
                .find(group.title())
                .unwrap_or_else(|| panic!("{}", group.title()));
            at += found;
            for s in ALL.iter().filter(|s| s.group == group) {
                assert!(
                    text[at..].contains(s.name),
                    "{} in {}",
                    s.name,
                    group.title()
                );
            }
        }
        assert!(text.contains("20 (default)"), "{text}");
        assert!(text.contains("Ana"), "{text}");
    }

    #[test]
    fn a_secret_shows_only_as_set_or_not_set() {
        let f = Folder::new("secret");
        let mut c = f.config(FILE, &[(LOGIN_HASH, &"ab".repeat(32))]);
        for width in [110, 40] {
            let text = screen(&c, width, 200);
            assert!(!text.contains(TOKEN) && !text.contains("abab"), "{text}");
            assert!(text.contains("secret: set"), "{text}");
            assert!(text.contains("secret: not set"), "{text}");
        }
        // Enter on a secret starts empty, and the typing stays hidden.
        choose(&mut c, DROPBOX_TOKEN);
        c.key(KeyCode::Enter, false);
        let text = screen(&c, 110, 80);
        assert!(text.contains("New value: _"), "{text}");
        keys(&mut c, "new-invented-token");
        let text = screen(&c, 110, 80);
        assert!(!text.contains("new-invented"), "{text}");
        c.key(KeyCode::Enter, false);
        c.key(KeyCode::Char('s'), false);
        assert!(
            f.env_text()
                .contains("ORGTHREAD_DROPBOX_TOKEN=new-invented-token")
        );
        let text = screen(&c, 110, 80);
        assert!(
            !text.contains("new-invented") && !text.contains(TOKEN),
            "{text}"
        );
        let log = f.log();
        assert!(
            log.contains("ORGTHREAD_DROPBOX_TOKEN") && !log.contains("invented-token"),
            "{log}"
        );
    }

    /// While a secret is typed or pasted, the screen shows nothing of it: no
    /// sign, no `*`, and nothing that changes with its length.
    #[test]
    fn the_typing_of_a_secret_shows_nothing_not_even_its_length() {
        let f = Folder::new("secret-typing");
        for width in [110, 40] {
            let mut c = f.config("", &[]);
            choose(&mut c, DROPBOX_TOKEN);
            c.key(KeyCode::Enter, false);
            let empty = screen(&c, width, 60);
            assert!(!empty.contains('*'), "{empty}");
            for typed in ["Z", "invented-token-of-24-sig", &"p".repeat(64)] {
                keys(&mut c, typed);
                let text = screen(&c, width, 60);
                assert!(!text.contains('*'), "{text}");
                assert!(!text.contains(typed), "{text}");
                assert_eq!(text, empty, "the screen changed with {} signs", typed.len());
                for _ in 0..typed.len() {
                    c.key(KeyCode::Backspace, false);
                }
            }
            // After Enter, the row says set, and still shows nothing more.
            keys(&mut c, "invented-token");
            c.key(KeyCode::Enter, false);
            let text = screen(&c, width, 200);
            assert!(text.contains("secret: set"), "{text}");
            assert!(!text.contains("invented-token"), "{text}");
        }
    }

    #[test]
    fn an_empty_enter_on_a_secret_keeps_it() {
        let f = Folder::new("secret-empty");
        let mut c = f.config(FILE, &[]);
        choose(&mut c, DROPBOX_TOKEN);
        c.key(KeyCode::Enter, false);
        c.key(KeyCode::Enter, false);
        assert!(c.unsaved().is_empty());
        c.key(KeyCode::Char('d'), false);
        assert_eq!(c.unsaved(), [DROPBOX_TOKEN]);
    }

    #[test]
    fn a_change_saves_with_a_copy_a_log_line_and_the_restart_command() {
        let f = Folder::new("save");
        let mut c = f.config(FILE, &[]);
        type_value(&mut c, UPLOAD_MB, "50");
        assert_eq!(c.unsaved(), [UPLOAD_MB]);
        assert!(screen(&c, 110, 80).contains("* ORGTHREAD_UPLOAD_MB"));
        // Nothing is on the disk before s.
        assert_eq!(f.env_text(), FILE);
        c.key(KeyCode::Char('s'), false);
        assert_eq!(f.env_text(), format!("{FILE}ORGTHREAD_UPLOAD_MB=50\n"));
        let copies: Vec<_> = fs::read_dir(&f.0)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                name.starts_with(".env.before-config-").then_some(name)
            })
            .collect();
        assert_eq!(copies.len(), 1, "{copies:?}");
        assert_eq!(fs::read_to_string(f.0.join(&copies[0])).unwrap(), FILE);
        let log = f.log();
        assert!(
            log.contains("\tconfigured\tORGTHREAD_UPLOAD_MB\tAna (config)\t"),
            "{log}"
        );
        assert!(!log.contains("50"), "{log}");
        let text = screen(&c, 110, 80);
        assert!(text.contains("Saved: ORGTHREAD_UPLOAD_MB."), "{text}");
        assert!(text.contains(&copies[0]), "{text}");
        assert!(text.contains("Restart the desktop app"), "{text}");
        assert!(c.unsaved().is_empty());
        // On a server the screen names the command and does not run it.
        let mut c = Config::new(
            EnvFile::read(&f.0.join(".env")).unwrap(),
            vec![],
            None,
            f.paths(true),
        );
        type_value(&mut c, UPLOAD_MB, "60");
        c.key(KeyCode::Char('s'), false);
        assert!(screen(&c, 110, 80).contains(RESTART_SERVER));
        assert!(
            f.log()
                .contains("\tconfigured\tORGTHREAD_UPLOAD_MB\towner (config)\t")
        );
    }

    #[test]
    fn a_wrong_value_is_refused_at_enter_and_at_save() {
        let f = Folder::new("wrong");
        let mut c = f.config(FILE, &[]);
        type_value(&mut c, UPLOAD_MB, "abc");
        let text = screen(&c, 110, 80);
        assert!(
            text.contains("ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc"),
            "{text}"
        );
        assert!(matches!(c.mode, Mode::Edit(_)), "the typing stays");
        assert!(c.unsaved().is_empty());
        // A wrong value already in the file blocks a save until it is put right.
        let mut c = f.config("ORGTHREAD_UPLOAD_MB=abc\n", &[]);
        choose(&mut c, UPLOAD_MB);
        assert!(screen(&c, 110, 80).contains("Wrong: ORGTHREAD_UPLOAD_MB"));
        type_value(&mut c, OWNER, "Bo");
        c.key(KeyCode::Char('s'), false);
        assert!(screen(&c, 110, 80).contains("Nothing saved."));
        assert_eq!(f.env_text(), "ORGTHREAD_UPLOAD_MB=abc\n");
        assert_eq!(f.log(), "");
        choose(&mut c, UPLOAD_MB);
        c.key(KeyCode::Char('d'), false);
        c.key(KeyCode::Char('s'), false);
        assert_eq!(f.env_text(), "ORGTHREAD_OWNER=Bo\n");
    }

    #[test]
    fn d_resets_to_the_default_and_the_line_goes() {
        let f = Folder::new("default");
        let mut c = f.config(FILE, &[]);
        choose(&mut c, OWNER);
        c.key(KeyCode::Char('d'), false);
        c.key(KeyCode::Char('s'), false);
        assert!(!f.env_text().contains("ORGTHREAD_OWNER="));
        assert!(
            f.env_text()
                .starts_with("# invented door\nORGTHREAD_OWNR=typo\n")
        );
        // An empty value is the default too.
        type_value(&mut c, DENY, "private");
        c.key(KeyCode::Char('s'), false);
        type_value(&mut c, DENY, "");
        c.key(KeyCode::Char('s'), false);
        assert!(!f.env_text().contains("ORGTHREAD_DENY"));
    }

    #[test]
    fn the_environment_wins_and_cannot_be_changed_here() {
        let f = Folder::new("environment");
        let mut c = f.config(FILE, &[(OWNER, "From Env"), (STORAGE, "dropbox")]);
        let text = screen(&c, 110, 80);
        assert!(text.contains("From Env (from the environment)"), "{text}");
        assert!(text.contains("dropbox (from the environment)"), "{text}");
        choose(&mut c, OWNER);
        c.key(KeyCode::Enter, false);
        assert!(matches!(c.mode, Mode::List));
        assert!(screen(&c, 110, 80).contains("comes from the environment"));
        c.key(KeyCode::Char('d'), false);
        assert!(c.unsaved().is_empty());
        assert_eq!(f.env_text(), FILE);
    }

    #[test]
    fn unknown_names_show_as_not_used_and_stay_on_save() {
        let f = Folder::new("unknown");
        let mut c = f.config(FILE, &[]);
        let text = screen(&c, 110, 80);
        let at = text.find("Not used").expect("the group Not used");
        assert!(text[at..].contains("ORGTHREAD_OWNR"), "{text}");
        type_value(&mut c, ROOT, "/srv/invented");
        c.key(KeyCode::Char('s'), false);
        assert_eq!(
            f.env_text(),
            format!("{FILE}ORGTHREAD_ROOT=/srv/invented\n")
        );
    }

    #[test]
    fn q_asks_first_when_a_change_is_not_saved() {
        let f = Folder::new("quit");
        let mut c = f.config(FILE, &[]);
        assert!(c.key(KeyCode::Char('q'), false), "nothing changed: q quits");
        type_value(&mut c, UPLOAD_MB, "50");
        assert!(!c.key(KeyCode::Char('q'), false));
        let text = screen(&c, 110, 40);
        assert!(text.contains("Quit without saving?"), "{text}");
        assert!(!c.key(KeyCode::Char('n'), false), "any other key stays");
        assert!(!c.key(KeyCode::Char('c'), true));
        assert!(c.key(KeyCode::Char('y'), false));
        assert_eq!(f.env_text(), FILE, "a quit saves nothing");
    }

    #[test]
    fn the_keys_move_and_stay_inside_the_list() {
        let f = Folder::new("move");
        let mut c = f.config("", &[]);
        c.key(KeyCode::Up, false);
        assert_eq!(c.chosen, 0);
        for _ in 0..100 {
            c.key(KeyCode::Down, false);
        }
        assert_eq!(c.chosen, ALL.len() - 1);
        c.key(KeyCode::Char('k'), false);
        assert_eq!(c.chosen, ALL.len() - 2);
        c.key(KeyCode::Char('?'), false);
        assert!(screen(&c, 110, 60).contains("Help"));
        c.key(KeyCode::Esc, false);
        assert!(matches!(c.mode, Mode::List));
    }

    /// A narrow terminal, as for the popup of watch: the list, the details,
    /// the typing, the help and the quit question all fit at 40 columns, and
    /// no word is lost at the edge.
    #[test]
    fn the_screen_fits_40_columns() {
        let f = Folder::new("narrow");
        let mut c = f.config(FILE, &[]);
        let text = screen(&c, 40, 30);
        assert!(text.contains("ORGTHREAD_ROOT"), "{text}");
        assert!(
            text.contains("enter: change  s: save  ?: help  q: quit"),
            "{text}"
        );
        for line in text.lines() {
            assert!(line.chars().count() <= 40, "{line}");
        }
        // Each setting has its full name on the screen when chosen.
        for s in &ALL {
            choose(&mut c, s.name);
            let text = screen(&c, 40, 30);
            assert!(text.contains(s.name), "{}: {text}", s.name);
        }
        type_value(&mut c, UPLOAD_MB, "50");
        choose(&mut c, OWNER);
        c.key(KeyCode::Enter, false);
        let text = screen(&c, 40, 30);
        assert!(text.contains("New value: Ana_"), "{text}");
        assert!(text.contains("enter: keep  esc: back  empty:"), "{text}");
        c.key(KeyCode::Esc, false);
        c.key(KeyCode::Char('?'), false);
        let text = screen(&c, 40, 60);
        assert!(text.contains("Help"), "{text}");
        assert!(text.contains("q, Ctrl+C"), "{text}");
        c.key(KeyCode::Esc, false);
        c.key(KeyCode::Char('q'), false);
        let text = screen(&c, 40, 30);
        assert!(text.contains("Quit without saving?"), "{text}");
        assert!(text.contains("y: quit, and the"), "{text}");
        assert!(text.contains("any other key: stay"), "{text}");
        assert!(text.contains("ORGTHREAD_UPLOAD_MB"), "{text}");
    }

    #[test]
    fn a_tiny_terminal_does_not_crash() {
        let f = Folder::new("tiny");
        let mut c = f.config(FILE, &[]);
        screen(&c, 10, 4);
        c.key(KeyCode::Char('?'), false);
        screen(&c, 10, 4);
    }

    #[test]
    fn the_check_lists_every_setting_and_hides_the_secrets() {
        let file = EnvFile::parse(FILE);
        let env = vec![(UPLOAD_MB.to_string(), OsString::from("30"))];
        let (text, fine) = check(&file, &env, Path::new("/invented/.env"));
        assert!(fine, "{text}");
        for s in &ALL {
            assert!(text.contains(s.name), "{}", s.name);
        }
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("ORGTHREAD_DROPBOX_TOKEN"), "{text}");
        assert!(text.contains("secret: set  (.env)"), "{text}");
        assert!(text.contains("30  (environment)"), "{text}");
        assert!(text.contains("Ana  (.env)"), "{text}");
        assert!(text.contains("250  (default)"), "{text}");
        assert!(text.contains("ORGTHREAD_OWNR"), "{text}");
        assert!(text.ends_with("All 27 settings are fine.\n"), "{text}");

        let file = EnvFile::parse("ORGTHREAD_UPLOAD_MB=abc\nORGTHREAD_LOGIN_HASH=invented-wrong\n");
        let (text, fine) = check(&file, &[], Path::new("/invented/.env"));
        assert!(!fine);
        assert!(
            text.contains("wrong: ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc"),
            "{text}"
        );
        assert!(text.contains("wrong: ORGTHREAD_LOGIN_HASH"), "{text}");
        assert!(!text.contains("invented-wrong"), "{text}");
    }
}
