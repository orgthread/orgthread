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

/// True when `ORGTHREAD_STORAGE` in effect names the Dropbox API, as the
/// reader of the door reads it.
pub fn dropbox_of(file: &EnvFile, env: &[(String, OsString)]) -> bool {
    let storage = settings::find(settings::STORAGE).expect("STORAGE is in the list");
    match source_of(storage, file, env) {
        Source::Environment(value) => value == "dropbox",
        Source::File(value) => value == "dropbox",
        Source::Default => false,
    }
}

/// The settings of the other door that have a value: hidden, and untouched.
pub fn hidden_set(file: &EnvFile, env: &[(String, OsString)]) -> Vec<&'static Setting> {
    let dropbox = dropbox_of(file, env);
    ALL.iter()
        .filter(|s| !s.doors.fits(dropbox) && source_of(s, file, env) != Source::Default)
        .collect()
}

/// The quiet line on the hidden settings that are set, for example
/// "2 Dropbox settings are set but not used with local".
pub fn hidden_line(count: usize, dropbox: bool) -> Option<String> {
    let (other, storage) = if dropbox {
        ("local folder", "dropbox")
    } else {
        ("Dropbox", "local")
    };
    match count {
        0 => None,
        1 => Some(format!(
            "1 {other} setting is set but not used with {storage}"
        )),
        n => Some(format!(
            "{n} {other} settings are set but not used with {storage}"
        )),
    }
}

/// The text of `orgthread config --check`, and true when every value of this
/// door is right. It shows the settings of the door that `ORGTHREAD_STORAGE`
/// names, as the screen does. A wrong value of the other door is a warning.
pub fn check(file: &EnvFile, env: &[(String, OsString)], env_path: &Path) -> (String, bool) {
    const NAME: usize = 30;
    let mut out = format!(
        "orgthread {} · settings of {}\n",
        crate::VERSION,
        env_path.display()
    );
    let dropbox = dropbox_of(file, env);
    let mut wrong = 0;
    for group in Group::ALL {
        let rows: Vec<&Setting> = ALL
            .iter()
            .filter(|s| s.group == group && s.doors.fits(dropbox))
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{}\n", group.title()));
        for s in rows {
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
    let hidden = hidden_set(file, env);
    if let Some(line) = hidden_line(hidden.len(), dropbox) {
        out.push_str(&format!("\n{line}\n"));
    }
    for s in hidden {
        if let Some(reason) = problem_of(s, &source_of(s, file, env)) {
            let storage = if dropbox { "dropbox" } else { "local" };
            out.push_str(&format!(
                "warning: {reason}. It is not used with {storage}, so it stops nothing\n"
            ));
        }
    }
    out.push('\n');
    if wrong == 0 {
        out.push_str("All settings of this door are fine.\n");
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
    /// The popup of a typed value, with the value as typed so far.
    Edit(String),
    /// The popup of a value with fixed choices, with the place of the choice.
    Choose(usize),
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

    /// True when the file on the screen names the Dropbox API. A change of
    /// `ORGTHREAD_STORAGE` changes the rows at once, before the save.
    fn dropbox(&self) -> bool {
        dropbox_of(&self.file, &self.env)
    }

    /// The indexes in `ALL` of the settings of this door, in their order.
    fn visible(&self) -> Vec<usize> {
        let dropbox = self.dropbox();
        (0..ALL.len())
            .filter(|&i| ALL[i].doors.fits(dropbox))
            .collect()
    }

    /// Moves the choice by `by` rows among the settings of this door, and
    /// stays inside the list.
    fn step(&mut self, by: isize) {
        let visible = self.visible();
        let place = visible.iter().position(|&i| i == self.chosen).unwrap_or(0) as isize;
        let next = (place + by).clamp(0, visible.len() as isize - 1);
        self.chosen = visible[next as usize];
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
            Mode::Choose(_) => {
                self.choose_key(code, ctrl);
                return false;
            }
            Mode::List => {}
        }
        self.note = None;
        match code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('c') if ctrl => return self.quit(),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Home | KeyCode::Char('g') => self.step(-(ALL.len() as isize)),
            KeyCode::End | KeyCode::Char('G') => self.step(ALL.len() as isize),
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

    /// A key in the popup of choices: up and down choose, Enter keeps.
    fn choose_key(&mut self, code: KeyCode, ctrl: bool) {
        let count = self.setting().kind.choices().len();
        let Mode::Choose(place) = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up | KeyCode::Char('k') => *place = place.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *place = (*place + 1).min(count - 1),
            KeyCode::Enter => {
                let place = *place;
                self.keep_choice(place);
            }
            KeyCode::Esc => {
                self.mode = Mode::List;
                self.say("Nothing changed.", true);
            }
            KeyCode::Char('c') if ctrl => {
                self.mode = Mode::List;
                self.say("Nothing changed.", true);
            }
            _ => {}
        }
    }

    /// Enter on a choice. The value that holds already changes nothing, so
    /// the default stays a missing line.
    fn keep_choice(&mut self, place: usize) {
        let s = self.setting();
        let value = s.kind.choices()[place];
        let now = self.file.value(s.name).unwrap_or(default_choice(s));
        if value != now {
            self.file.set(s.name, value);
        }
        self.mode = Mode::List;
        self.note = None;
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
        let choices = s.kind.choices();
        if !choices.is_empty() {
            let now = self.file.value(s.name).unwrap_or(default_choice(s));
            let place = choices.iter().position(|c| *c == now);
            let place = place.or_else(|| choices.iter().position(|c| *c == default_choice(s)));
            self.mode = Mode::Choose(place.unwrap_or(0));
            return;
        }
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
        // A setting that the environment holds, or that the door does not
        // read, stays as it is and is not checked.
        let env = &self.env;
        let dropbox = self.dropbox();
        let skip = |name: &str| {
            env.iter().any(|(n, _)| n == name)
                || settings::find(name).is_some_and(|s| !s.doors.fits(dropbox))
        };
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
        let dropbox = self.dropbox();
        for group in Group::ALL {
            let rows: Vec<(usize, &Setting)> = ALL
                .iter()
                .enumerate()
                .filter(|(_, s)| s.group == group && s.doors.fits(dropbox))
                .collect();
            if rows.is_empty() {
                continue;
            }
            items.push(ListItem::new(Line::styled(group.title().to_string(), bold)));
            for (i, s) in rows {
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
        let hidden = hidden_set(&self.file, &self.env).len();
        if let Some(line) = hidden_line(hidden, dropbox) {
            items.push(ListItem::new(Text::from(plain(&line, width, Style::new()))));
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

    /// The last lines: an answer, or the keys.
    fn foot(&self, width: u16) -> Vec<Line<'static>> {
        let w = usize::from(width);
        let mut lines = Vec::new();
        // In the popup, its own lines hold the answer and the keys.
        if matches!(self.mode, Mode::Edit(_) | Mode::Choose(_)) {
            return lines;
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
            Mode::Edit(_) | Mode::Choose(_) => self.draw_edit(frame),
            Mode::List => {}
        }
    }

    /// The lines of the edit popup: what the setting is, its value now, the
    /// field or the choices, why a value is wrong, and the keys.
    fn edit_lines(&self, width: usize) -> Vec<Line<'static>> {
        let s = self.setting();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut lines = plain(s.name, width, bold);
        lines.extend(plain(s.help, width, Style::new()));
        lines.extend(labelled("Now", &row_value(s, &self.source(s)), width));
        lines.extend(labelled("Default", s.default, width));
        lines.push(Line::raw(""));
        let keys = match &self.mode {
            Mode::Choose(place) => {
                for (i, choice) in s.kind.choices().iter().enumerate() {
                    if i == *place {
                        let style = Style::new().fg(ORANGE).add_modifier(Modifier::BOLD);
                        lines.extend(plain(&format!("> {choice}"), width, style));
                    } else {
                        lines.extend(plain(&format!("  {choice}"), width, Style::new()));
                    }
                }
                "up, down: choose  enter: keep  esc: back"
            }
            Mode::Edit(typed) => {
                // A secret shows nothing while it is typed or pasted, not even
                // its length: the field stays the same for every sign.
                let shown = if s.secret { "" } else { typed.as_str() };
                lines.extend(plain(&format!("> {shown}_"), width, bold));
                if s.secret {
                    "enter: keep  esc: back"
                } else {
                    "enter: keep  esc: back  empty: default"
                }
            }
            _ => "",
        };
        if let Some(Note(notes)) = &self.note {
            for (text, _) in notes {
                lines.extend(plain(text, width, Style::new().fg(ORANGE)));
            }
        }
        lines.push(Line::raw(""));
        lines.extend(plain(keys, width, Style::new()));
        lines
    }

    /// The edit popup, centered over the list, with an orange border, as the
    /// revert question of watch.
    fn draw_edit(&self, frame: &mut Frame) {
        let title = Span::styled(
            " Change a setting ",
            Style::new().fg(ORANGE).add_modifier(Modifier::BOLD),
        );
        let block = Block::bordered()
            .border_style(Style::new().fg(ORANGE))
            .padding(Padding::horizontal(1))
            .title(title);
        // The padding takes one column on each side.
        draw_panel(frame, 64, block, |width| {
            self.edit_lines(width.saturating_sub(2))
        });
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

/// The choice that holds when the setting has no line.
fn default_choice(s: &Setting) -> &'static str {
    match s.kind {
        settings::Kind::YesNo => "no",
        _ => s.kind.choices().first().copied().unwrap_or(""),
    }
}

const KEYS: [(&str, &str); 7] = [
    ("up, k", "the setting above"),
    ("down, j", "the setting below"),
    (
        "Enter",
        "change the value in a popup. Enter keeps it, Esc goes back. An empty value is the default. Up and down choose for a setting with fixed choices",
    ),
    ("d", "back to the default: the line goes from .env"),
    (
        "s",
        "save: check every value, keep the old file as a copy, write, and log",
    ),
    ("?, Esc", "open or close this help"),
    ("q, Ctrl+C", "quit. With a change not saved, it asks first"),
];

const HELP: [&str; 5] = [
    "The screen shows the settings of the door that ORGTHREAD_STORAGE names, and those of both doors. Change ORGTHREAD_STORAGE to see the rows of the other door. A hidden setting that is set stays in .env, untouched, and a line at the bottom counts it.",
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
    use crate::settings::{
        DENY, DROPBOX_API, DROPBOX_APP_KEY, DROPBOX_TOKEN, LISTEN_ANYWHERE, LOGIN_HASH, OWNER,
        ROOT, STORAGE, UPLOAD_MB, VERSIONS,
    };
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

    /// Chooses the row of `name`, which must be shown.
    fn choose(c: &mut Config, name: &str) {
        c.key(KeyCode::Char('g'), false);
        for _ in 0..ALL.len() {
            if c.setting().name == name {
                return;
            }
            c.key(KeyCode::Char('j'), false);
        }
        panic!("{name} is not shown");
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

    /// A local door, with a Dropbox token that this door does not read.
    const FILE: &str = "# invented door\nORGTHREAD_OWNER=Ana\nORGTHREAD_OWNR=typo\nORGTHREAD_DROPBOX_TOKEN=invented-secret-token-91f2\n";
    /// A Dropbox door, with two Dropbox secrets.
    const DROPBOX: &str = "ORGTHREAD_STORAGE=dropbox\nORGTHREAD_DROPBOX_TOKEN=invented-secret-token-91f2\nORGTHREAD_DROPBOX_APP_KEY=invented-app-key-77c1\n";

    fn dropbox_names() -> Vec<&'static str> {
        ALL.iter()
            .filter(|s| s.doors == settings::Doors::Dropbox)
            .map(|s| s.name)
            .collect()
    }

    /// Decisions 1 and 2: Basics first, then the groups, with only the rows of
    /// the door that ORGTHREAD_STORAGE names.
    #[test]
    fn the_screen_shows_the_rows_of_its_door_in_their_groups() {
        let f = Folder::new("groups");
        for (text, dropbox) in [(FILE, false), (DROPBOX, true)] {
            let c = f.config(text, &[]);
            let shown = screen(&c, 110, 80);
            let mut at = 0;
            for group in Group::ALL {
                let rows: Vec<&Setting> = ALL
                    .iter()
                    .filter(|s| s.group == group && s.doors.fits(dropbox))
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                at += shown[at..]
                    .find(group.title())
                    .unwrap_or_else(|| panic!("{}: {shown}", group.title()));
                for s in rows {
                    at += shown[at..]
                        .find(s.name)
                        .unwrap_or_else(|| panic!("{} out of order: {shown}", s.name));
                }
            }
            let first = shown.find("Basics").unwrap();
            assert!(first < shown.find("ORGTHREAD_OWNER").unwrap());
            assert!(shown.find("ORGTHREAD_OWNER") < shown.find("ORGTHREAD_STORAGE"));
            assert!(shown.find("ORGTHREAD_STORAGE") < shown.find("Folders and logs"));
            if dropbox {
                for name in [ROOT, VERSIONS] {
                    assert!(!shown.contains(name), "{name}: {shown}");
                }
                assert!(shown.contains(DROPBOX_API), "{shown}");
            } else {
                for name in dropbox_names() {
                    assert!(!shown.contains(name), "{name}: {shown}");
                }
                assert!(!shown.contains("\nDropbox\n") && !shown.contains("│Dropbox "));
                assert!(shown.contains(ROOT), "{shown}");
            }
        }
    }

    /// Decision 3: a change of ORGTHREAD_STORAGE changes the rows at once,
    /// before the save.
    #[test]
    fn a_change_of_storage_changes_the_rows_before_the_save() {
        let f = Folder::new("switch");
        let mut c = f.config("ORGTHREAD_OWNER=Ana\n", &[]);
        assert!(!screen(&c, 110, 80).contains(DROPBOX_API));
        choose(&mut c, STORAGE);
        c.key(KeyCode::Enter, false);
        c.key(KeyCode::Down, false);
        c.key(KeyCode::Enter, false);
        let shown = screen(&c, 110, 80);
        assert!(shown.contains(DROPBOX_API), "{shown}");
        assert!(!shown.contains(ROOT), "{shown}");
        assert_eq!(f.env_text(), "ORGTHREAD_OWNER=Ana\n", "nothing saved yet");
        // The new rows can be filled in the same visit.
        type_value(&mut c, DROPBOX_API, "https://api.example.com");
        c.key(KeyCode::Char('s'), false);
        assert_eq!(
            f.env_text(),
            "ORGTHREAD_OWNER=Ana\nORGTHREAD_STORAGE=dropbox\nORGTHREAD_DROPBOX_API=https://api.example.com\n"
        );
    }

    /// Decision 4: a hidden setting that is set stays untouched, and one quiet
    /// line counts it.
    #[test]
    fn a_hidden_setting_that_is_set_stays_and_a_line_counts_it() {
        let f = Folder::new("hidden");
        let text = "ORGTHREAD_DROPBOX_API=https://api.example.com\nORGTHREAD_OWNER=Ana\nORGTHREAD_DROPBOX_TOKEN=invented-token\n";
        let mut c = f.config(text, &[]);
        let shown = screen(&c, 110, 80);
        assert!(
            shown.contains("2 Dropbox settings are set but not used with local"),
            "{shown}"
        );
        type_value(&mut c, OWNER, "Bo");
        c.key(KeyCode::Char('s'), false);
        assert_eq!(
            f.env_text(),
            "ORGTHREAD_DROPBOX_API=https://api.example.com\nORGTHREAD_OWNER=Bo\nORGTHREAD_DROPBOX_TOKEN=invented-token\n"
        );
        let c = f.config(
            "ORGTHREAD_STORAGE=dropbox\nORGTHREAD_ROOT=/srv/invented\n",
            &[],
        );
        let shown = screen(&c, 110, 80);
        assert!(
            shown.contains("1 local folder setting is set but not used with dropbox"),
            "{shown}"
        );
        let c = f.config("ORGTHREAD_OWNER=Ana\n", &[]);
        assert!(!screen(&c, 110, 80).contains("but not used"));
    }

    /// Decision 5: a wrong value of the other door blocks no save.
    #[test]
    fn a_wrong_hidden_value_blocks_no_save() {
        let f = Folder::new("hidden-wrong");
        let mut c = f.config("ORGTHREAD_DROPBOX_API=ftp://invented\n", &[]);
        type_value(&mut c, OWNER, "Bo");
        c.key(KeyCode::Char('s'), false);
        assert_eq!(
            f.env_text(),
            "ORGTHREAD_DROPBOX_API=ftp://invented\nORGTHREAD_OWNER=Bo\n"
        );
    }

    /// Decision 8: no key shows the rows of the other door.
    #[test]
    fn no_key_shows_the_rows_of_the_other_door() {
        let f = Folder::new("no-key");
        let mut c = f.config(FILE, &[]);
        let mut codes: Vec<KeyCode> = ('!'..='~')
            .filter(|c| *c != 'q')
            .map(KeyCode::Char)
            .collect();
        codes.extend([
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Insert,
            KeyCode::F(1),
            KeyCode::F(2),
        ]);
        for code in codes {
            for ctrl in [false, true] {
                if ctrl && code == KeyCode::Char('c') {
                    continue;
                }
                c.key(code, ctrl);
                let shown = screen(&c, 110, 80);
                for name in dropbox_names() {
                    assert!(!shown.contains(name), "{code:?} shows {name}: {shown}");
                }
                if !matches!(c.mode, Mode::List) {
                    c.key(KeyCode::Esc, false);
                }
            }
        }
    }

    /// Decision 9: the app key is a secret, hidden with local, and never in
    /// clear with dropbox.
    #[test]
    fn the_app_key_is_a_secret() {
        let f = Folder::new("app-key");
        let mut c = f.config(DROPBOX, &[]);
        for width in [110, 40] {
            let shown = screen(&c, width, 200);
            assert!(!shown.contains("invented-app-key"), "{shown}");
            assert!(shown.contains(DROPBOX_APP_KEY), "{shown}");
        }
        let at = screen(&c, 110, 200).find("Secrets").unwrap();
        assert!(screen(&c, 110, 200)[at..].contains(DROPBOX_APP_KEY));
        choose(&mut c, DROPBOX_APP_KEY);
        let shown = screen(&c, 110, 80);
        assert!(shown.contains("secret: set"), "{shown}");
        c.key(KeyCode::Enter, false);
        let shown = screen(&c, 110, 80);
        assert!(!shown.contains("invented-app-key"), "{shown}");
        let (text, _) = check(&EnvFile::parse(DROPBOX), &[], Path::new("/invented/.env"));
        assert!(!text.contains("invented-app-key"), "{text}");
        let c = f.config("ORGTHREAD_DROPBOX_APP_KEY=invented-app-key-77c1\n", &[]);
        assert!(!screen(&c, 110, 80).contains(DROPBOX_APP_KEY));
    }

    /// True when the screen has a corner of a border drawn in orange.
    fn orange_border(c: &Config, width: u16, height: u16) -> bool {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| c.draw(f)).unwrap();
        t.backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "┌" && cell.fg == ORANGE)
    }

    /// Decision 10: Enter opens a centered popup with an orange border: the
    /// name, the help line, the value now and the default, the field, and
    /// the keys.
    #[test]
    fn every_edit_happens_in_a_popup() {
        let f = Folder::new("popup");
        let mut c = f.config(FILE, &[]);
        assert!(!orange_border(&c, 110, 60));
        choose(&mut c, OWNER);
        c.key(KeyCode::Enter, false);
        assert!(orange_border(&c, 110, 60));
        let shown = screen(&c, 110, 60);
        let owner = settings::find(OWNER).unwrap();
        for part in [
            "Change a setting",
            "Now       Ana",
            "Default   owner",
            "> Ana_",
            "enter: keep  esc: back  empty: default",
        ] {
            assert!(shown.contains(part), "{part}: {shown}");
        }
        assert!(shown.contains(&owner.help[..30]), "{shown}");
        // No typing line at the foot: the popup holds it.
        assert!(!shown.contains("New value"), "{shown}");
        c.key(KeyCode::Esc, false);
        assert!(!orange_border(&c, 110, 60));
        // A secret: the field stays empty.
        choose(&mut c, LOGIN_HASH);
        c.key(KeyCode::Enter, false);
        keys(&mut c, "invented");
        let shown = screen(&c, 110, 60);
        assert!(shown.contains("> _"), "{shown}");
        assert!(shown.contains("enter: keep  esc: back"), "{shown}");
        // A wrong value keeps the popup open and says why.
        c.key(KeyCode::Enter, false);
        let shown = screen(&c, 110, 60);
        assert!(matches!(c.mode, Mode::Edit(_)));
        assert!(shown.contains("Change a setting"), "{shown}");
        assert!(
            shown.contains("ORGTHREAD_LOGIN_HASH needs the 64 signs"),
            "{shown}"
        );
        c.key(KeyCode::Esc, false);
        assert!(c.unsaved().is_empty());
    }

    /// Decision 10: STORAGE and LISTEN_ANYWHERE offer their choices with up
    /// and down. The choice that holds already adds no line.
    #[test]
    fn storage_and_listen_anywhere_choose_with_up_and_down() {
        let f = Folder::new("choices");
        let mut c = f.config("", &[]);
        choose(&mut c, STORAGE);
        c.key(KeyCode::Enter, false);
        assert!(matches!(c.mode, Mode::Choose(0)));
        let shown = screen(&c, 110, 60);
        assert!(
            shown.contains("> local") && shown.contains("  dropbox"),
            "{shown}"
        );
        assert!(
            shown.contains("up, down: choose  enter: keep  esc: back"),
            "{shown}"
        );
        c.key(KeyCode::Up, false);
        c.key(KeyCode::Enter, false);
        assert!(c.unsaved().is_empty(), "local holds already: no line");
        choose(&mut c, LISTEN_ANYWHERE);
        c.key(KeyCode::Enter, false);
        let shown = screen(&c, 110, 60);
        assert!(shown.contains("> no") && shown.contains("  yes"), "{shown}");
        c.key(KeyCode::Char('k'), false);
        assert!(screen(&c, 110, 60).contains("> yes"));
        c.key(KeyCode::Enter, false);
        c.key(KeyCode::Char('s'), false);
        assert_eq!(f.env_text(), "ORGTHREAD_LISTEN_ANYWHERE=yes\n");
        // Esc goes back and changes nothing.
        c.key(KeyCode::Enter, false);
        c.key(KeyCode::Down, false);
        c.key(KeyCode::Esc, false);
        assert!(c.unsaved().is_empty());
    }

    #[test]
    fn a_secret_shows_only_as_set_or_not_set() {
        let f = Folder::new("secret");
        let mut c = f.config(DROPBOX, &[(LOGIN_HASH, &"ab".repeat(32))]);
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
        assert!(text.contains("> _"), "{text}");
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
            let mut c = f.config("ORGTHREAD_STORAGE=dropbox\n", &[]);
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
        let mut c = f.config(DROPBOX, &[]);
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
        // The value never shows. Each line starts with the time, which may hold
        // "50" itself, so only the fields after the time are checked.
        for line in log.lines() {
            assert!(
                line.split('\t').skip(1).all(|field| !field.contains("50")),
                "{line}"
            );
        }
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
        // The environment names the door: the Dropbox rows show.
        assert!(text.contains(DROPBOX_API), "{text}");
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
    fn the_keys_move_and_stay_inside_the_rows_of_the_door() {
        let f = Folder::new("move");
        let mut c = f.config("", &[]);
        let visible = c.visible();
        c.key(KeyCode::Up, false);
        assert_eq!(c.chosen, visible[0]);
        for _ in 0..100 {
            c.key(KeyCode::Down, false);
            assert!(visible.contains(&c.chosen), "{}", c.setting().name);
        }
        assert_eq!(c.chosen, visible[visible.len() - 1]);
        c.key(KeyCode::Char('k'), false);
        assert_eq!(c.chosen, visible[visible.len() - 2]);
        c.key(KeyCode::Char('?'), false);
        assert!(screen(&c, 110, 60).contains("Help"));
        c.key(KeyCode::Esc, false);
        assert!(matches!(c.mode, Mode::List));
    }

    /// A narrow terminal, as for the popup of watch: the list, the details,
    /// the edit popup, the choices, the help and the quit question all fit
    /// at 40 columns, and no word is lost at the edge.
    #[test]
    fn the_screen_and_the_popup_fit_40_columns() {
        let f = Folder::new("narrow");
        let mut c = f.config(FILE, &[]);
        let fits = |text: &str| {
            for line in text.lines() {
                assert!(line.chars().count() <= 40, "{line}");
            }
        };
        let text = screen(&c, 40, 30);
        fits(&text);
        assert!(text.contains("ORGTHREAD_ROOT"), "{text}");
        assert!(
            text.contains("enter: change  s: save  ?: help  q: quit"),
            "{text}"
        );
        // Each setting of the door has its full name on the screen when chosen.
        for i in c.visible() {
            choose(&mut c, ALL[i].name);
            let text = screen(&c, 40, 30);
            assert!(text.contains(ALL[i].name), "{}: {text}", ALL[i].name);
        }
        type_value(&mut c, UPLOAD_MB, "50");
        // The popup of a typed value.
        choose(&mut c, OWNER);
        c.key(KeyCode::Enter, false);
        let text = screen(&c, 40, 30);
        fits(&text);
        assert!(orange_border(&c, 40, 30));
        for part in [
            "Change a setting",
            "ORGTHREAD_OWNER",
            "> Ana_",
            "enter: keep  esc: back",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        // A wrong value says why, inside the popup.
        c.key(KeyCode::Esc, false);
        type_value(&mut c, UPLOAD_MB, "abc");
        let text = screen(&c, 40, 30);
        fits(&text);
        assert!(text.contains("ORGTHREAD_UPLOAD_MB"), "{text}");
        assert!(text.contains("number of MB, not abc"), "{text}");
        c.key(KeyCode::Esc, false);
        // The popup of choices.
        choose(&mut c, STORAGE);
        c.key(KeyCode::Enter, false);
        let text = screen(&c, 40, 30);
        fits(&text);
        assert!(
            text.contains("> local") && text.contains("dropbox"),
            "{text}"
        );
        assert!(
            text.contains("up, down: choose  enter: keep  esc:"),
            "{text}"
        );
        assert!(text.contains("│ back"), "{text}");
        c.key(KeyCode::Esc, false);
        c.key(KeyCode::Char('?'), false);
        let text = screen(&c, 40, 60);
        fits(&text);
        assert!(text.contains("Help"), "{text}");
        assert!(text.contains("q, Ctrl+C"), "{text}");
        c.key(KeyCode::Esc, false);
        c.key(KeyCode::Char('q'), false);
        let text = screen(&c, 40, 30);
        fits(&text);
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
        c.key(KeyCode::Esc, false);
        c.key(KeyCode::Enter, false);
        screen(&c, 10, 4);
    }

    /// Decision 6: the check has the order and the filter of the screen, and
    /// the quiet line. A wrong value of the other door is a warning only.
    #[test]
    fn the_check_lists_the_rows_of_its_door_and_hides_the_secrets() {
        let file = EnvFile::parse(FILE);
        let env = vec![(UPLOAD_MB.to_string(), OsString::from("30"))];
        let (text, fine) = check(&file, &env, Path::new("/invented/.env"));
        assert!(fine, "{text}");
        let mut at = 0;
        for s in ALL.iter().filter(|s| s.doors.fits(false)) {
            at += text[at..]
                .find(s.name)
                .unwrap_or_else(|| panic!("{} out of order: {text}", s.name));
        }
        for name in dropbox_names() {
            assert!(!text.contains(name), "{name}: {text}");
        }
        assert!(text.find("\nBasics\n") < text.find("\nFolders and logs\n"));
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("30  (environment)"), "{text}");
        assert!(text.contains("Ana  (.env)"), "{text}");
        assert!(text.contains("250  (default)"), "{text}");
        assert!(text.contains("ORGTHREAD_OWNR"), "{text}");
        assert!(
            text.contains("1 Dropbox setting is set but not used with local"),
            "{text}"
        );
        assert!(
            text.ends_with("All settings of this door are fine.\n"),
            "{text}"
        );

        let (text, _) = check(&EnvFile::parse(DROPBOX), &[], Path::new("/invented/.env"));
        assert!(!text.contains(ROOT) && !text.contains(VERSIONS), "{text}");
        assert!(text.contains("secret: set  (.env)"), "{text}");
        assert!(!text.contains(TOKEN), "{text}");

        let file = EnvFile::parse(
            "ORGTHREAD_UPLOAD_MB=abc\nORGTHREAD_LOGIN_HASH=invented-wrong\nORGTHREAD_DROPBOX_API=ftp://invented\n",
        );
        let (text, fine) = check(&file, &[], Path::new("/invented/.env"));
        assert!(!fine);
        assert!(
            text.contains("wrong: ORGTHREAD_UPLOAD_MB needs a whole number of MB, not abc"),
            "{text}"
        );
        assert!(text.contains("wrong: ORGTHREAD_LOGIN_HASH"), "{text}");
        assert!(!text.contains("invented-wrong"), "{text}");
        assert!(
            text.contains("warning: ORGTHREAD_DROPBOX_API needs an address"),
            "{text}"
        );
        // A wrong value of the other door alone ends with 0.
        let file = EnvFile::parse("ORGTHREAD_DROPBOX_API=ftp://invented\n");
        let (text, fine) = check(&file, &[], Path::new("/invented/.env"));
        assert!(fine, "{text}");
        assert!(text.contains("warning: ORGTHREAD_DROPBOX_API"), "{text}");
    }
}
