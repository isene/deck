//! deck: a paned front for Claude Code. The answer gets the big pane, each
//! tool call one line in the log beside it, and the status line keeps the
//! bottom row. When a stop hook sends an answer back, only the rewrite is
//! shown, with one line in the log saying so.

mod claude;
mod session;

use claude::{Claude, Event, WAKE};
use crust::{seq, style, Crust, Cursor, Input, Pane};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

const HELP: &str = "deck: a paned front for Claude Code

Usage:
  deck              start a new session in this directory
  deck <tag>        open the session with this bookmark tag
  deck <id>         open the session with this id

Keys:
  Enter             send (while Claude works, it waits its turn)
  Esc               stop the turn Claude is on
  PgUp, PgDn        scroll the answers
  C-PgUp, C-PgDn    scroll the tool log
  C-O               the last turn's tool calls in full, and back
  C-L               draw the screen again
  C-D               quit
";

const BLUE: u8 = 39;
const CODE: u8 = 179;
const GREEN: u8 = 78;
const RED: u8 = 203;
const AMBER: u8 = 214;

struct Tool {
    id: String,
    name: String,
    what: String,
    input: Value,
    /// None while it runs.
    failed: Option<bool>,
    output: String,
}

enum Line {
    Prompt(String),
    Tool(Tool),
    Note(String),
    Rewrite(String),
    Stopped,
    Ended,
}

struct Turn {
    prompt: String,
    answer: Option<String>,
    failed: bool,
    /// Where its lines start in the log.
    log_at: usize,
    /// The answer as drawn, and the width it was drawn for.
    drawn: Option<(usize, String)>,
}

struct App {
    s: session::Session,
    claude: Claude,
    tx: Sender<Event>,
    model: String,
    version: String,
    context: u64,
    window: u64,
    limits: Option<(f64, f64, u64)>,
    turns: Vec<Turn>,
    log: Vec<Line>,
    waiting: VecDeque<String>,
    asks: VecDeque<(String, String, Value)>,
    busy: Option<Instant>,
    ended: Option<String>,
    errors: String,
    status: String,
    input: Vec<char>,
    at: usize,
    history: Vec<String>,
    back: usize,
    detail: bool,
    rows: u16,
    bar: Pane,
    answer: Pane,
    side: Option<Pane>,
    line: Pane,
    foot: Pane,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => return print!("{HELP}"),
        Some("-v" | "--version") => return println!("deck {}", env!("CARGO_PKG_VERSION")),
        _ => {}
    }
    let s = session::find(args.first().map(String::as_str)).unwrap_or_else(|e| quit(&e));
    if s.resume {
        if let Some(pid) = session::open_elsewhere(&s.id) {
            quit(&format!("That session is open in Claude Code already (pid {pid}). Close it there first."));
        }
    }
    let last = if s.resume { session::last(&s.id) } else { session::Last::default() };
    let errors = format!("{}/deck-{}.err", std::env::var("XDG_RUNTIME_DIR").unwrap_or("/tmp".into()), std::process::id());
    let file = fs::File::create(&errors).unwrap_or_else(|e| quit(&format!("{errors}: {e}")));
    let mut env = vec![("CC_SESSION_ID", s.id.clone())];
    if !s.tags.is_empty() {
        env.push(("CC_RESUME_TAGS", s.tags.join(" ")));
    }
    let (tx, rx) = mpsc::channel();
    let claude = Claude::start(&s.id, s.resume, &s.cwd, &env, file, tx.clone())
        .unwrap_or_else(|e| quit(&format!("Could not start Claude Code: {e}")));

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        Crust::cleanup();
        default_hook(p);
    }));
    Crust::init();
    Crust::set_app_identity("Deck");
    Crust::enable_bracketed_paste();

    let mut app = App::new(s, last, claude, tx, errors);
    app.layout();
    app.run_status();
    loop {
        // While Claude works the bar shows the time, so wake once a second;
        // otherwise sleep until a key, or until Claude's output arrives.
        let key = Input::getchr(app.busy.map(|_| 1));
        if let Some(k) = key.as_deref() {
            if k == "RESIZE" {
                if Crust::terminal_size() != (app.bar.w, app.rows) {
                    app.layout();
                }
            } else if app.key(k) {
                break;
            }
        }
        // Cleared before the drain, so an event sent after it wakes again.
        WAKE.store(false, Ordering::SeqCst);
        app.drain(&rx);
        app.draw_bar();
        app.place_cursor();
    }
    app.claude.stop();
    Crust::disable_bracketed_paste();
    Crust::cleanup();
    let _ = fs::remove_file(&app.errors);
}

fn quit(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(1)
}

impl App {
    fn new(s: session::Session, last: session::Last, claude: Claude, tx: Sender<Event>, errors: String) -> App {
        let mut app = App {
            s,
            claude,
            tx,
            model: last.model,
            version: String::new(),
            context: last.context,
            window: 0,
            limits: None,
            turns: Vec::new(),
            log: Vec::new(),
            waiting: VecDeque::new(),
            asks: VecDeque::new(),
            busy: None,
            ended: None,
            errors,
            status: String::new(),
            input: Vec::new(),
            at: 0,
            history: Vec::new(),
            back: 0,
            detail: false,
            rows: 0,
            bar: Pane::new(1, 1, 1, 1, 255, 234),
            answer: Pane::new(1, 1, 1, 1, 252, 0),
            side: None,
            line: Pane::new(1, 1, 1, 1, 231, 0),
            foot: Pane::new(1, 1, 1, 1, 252, 0),
        };
        if !last.prompt.is_empty() {
            app.log.push(Line::Prompt(last.prompt.clone()));
            app.turns.push(Turn { prompt: last.prompt, answer: Some(last.answer), failed: false, log_at: 0, drawn: None });
        }
        app
    }

    fn name(&self) -> String {
        match self.s.tags.first() {
            Some(t) => format!("#{t}"),
            None => self.s.id[..8].to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Claude's side

    fn drain(&mut self, rx: &Receiver<Event>) {
        let (mut answer, mut log, mut status) = (false, false, false);
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Init { session, model, version } => {
                    self.s.id = session;
                    self.model = model;
                    self.version = version;
                    status = true;
                }
                Event::Tool { id, name, what, input } => {
                    self.log.push(Line::Tool(Tool { id, name, what, input, failed: None, output: String::new() }));
                    log = true;
                    answer |= self.detail;
                }
                Event::Done { id, failed, output } => {
                    if let Some(t) = self.log.iter_mut().rev().find_map(|l| match l {
                        Line::Tool(t) if t.id == id => Some(t),
                        _ => None,
                    }) {
                        t.failed = Some(failed);
                        t.output = output;
                    }
                    log = true;
                    answer |= self.detail;
                }
                Event::Note(t) => {
                    self.log.push(Line::Note(t));
                    log = true;
                }
                Event::Rewrite(t) => {
                    self.log.push(Line::Rewrite(t));
                    log = true;
                }
                Event::Stopped => {
                    self.log.push(Line::Stopped);
                    log = true;
                }
                Event::Context(n) => self.context = n,
                Event::Limits { hours, week, week_reset } => self.limits = Some((hours, week, week_reset)),
                Event::Ask { id, tool, input } => {
                    if tool == "AskUserQuestion" || tool == "ExitPlanMode" {
                        self.claude.answer(&id, &input, false, "deck cannot show this dialog. Ask in plain text instead.");
                    } else {
                        self.asks.push_back((id, tool, input));
                    }
                }
                Event::Answer { text, failed, window } => {
                    if window > 0 {
                        self.window = window;
                    }
                    // The final text came as a note too; the answer pane has it.
                    if let Some(Line::Note(n)) = self.log.last() {
                        if n.trim() == text.trim() {
                            self.log.pop();
                        }
                    }
                    if let Some(t) = self.turns.last_mut() {
                        t.answer = Some(text);
                        t.failed = failed;
                        t.drawn = None;
                    }
                    self.busy = None;
                    answer = true;
                    log = true;
                    status = true;
                    if let Some(next) = self.waiting.pop_front() {
                        self.send(next);
                    }
                }
                Event::Status(s) => {
                    self.status = s;
                    self.draw_foot();
                }
                Event::Gone => {
                    let text = fs::read_to_string(&self.errors).unwrap_or_default();
                    let tail: Vec<&str> = text.lines().rev().take(15).collect();
                    self.ended = Some(tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
                    self.busy = None;
                    self.asks.clear();
                    self.log.push(Line::Ended);
                    answer = true;
                    log = true;
                }
            }
        }
        if answer {
            self.fill_answer(true);
        }
        if log {
            self.fill_side();
        }
        if status {
            self.run_status();
        }
        if answer || log {
            self.draw_line();
        }
    }

    fn send(&mut self, text: String) {
        self.turns.push(Turn { prompt: text.clone(), answer: None, failed: false, log_at: self.log.len(), drawn: None });
        self.log.push(Line::Prompt(text.clone()));
        self.claude.say(&text);
        self.busy = Some(Instant::now());
        self.detail = false;
        self.fill_answer(true);
        self.fill_side();
    }

    /// The status line: the command in Claude Code's settings, fed what
    /// Claude Code would feed it. It runs after a turn, never on a timer.
    fn run_status(&self) {
        let home = std::env::var("HOME").unwrap_or_default();
        let settings: Value = fs::read_to_string(format!("{home}/.claude/settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        let mut input = json!({
            "session_id": self.s.id,
            "cwd": self.s.cwd,
            "version": self.version,
            "model": {"id": self.model, "display_name": model_name(&self.model)},
            "workspace": {"current_dir": self.s.cwd, "project_dir": self.s.cwd},
            "context_window": {"context_window_size": self.window, "current_usage": {"input_tokens": self.context}},
        });
        if let Some((hours, week, reset)) = self.limits {
            input["rate_limits"] = json!({
                "five_hour": {"used_percentage": hours.round()},
                "seven_day": {"used_percentage": week.round(), "resets_at": reset},
            });
        }
        let tx = self.tx.clone();
        let Some(cmd) = settings["statusLine"]["command"].as_str().map(str::to_string) else {
            let pct = (self.window > 0).then(|| format!(" · {}%", self.context * 100 / self.window));
            let _ = tx.send(Event::Status(format!("{}{}", model_name(&self.model), pct.unwrap_or_default())));
            return claude::wake();
        };
        let id = self.s.id.clone();
        std::thread::spawn(move || {
            let child = Command::new("sh")
                .args(["-c", &cmd])
                .env("CC_SESSION_ID", id)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else { return };
            if let Some(mut i) = child.stdin.take() {
                let _ = i.write_all(input.to_string().as_bytes());
            }
            if let Ok(out) = child.wait_with_output() {
                let text = String::from_utf8_lossy(&out.stdout);
                let _ = tx.send(Event::Status(text.lines().next().unwrap_or("").to_string()));
                claude::wake();
            }
        });
    }

    // ------------------------------------------------------------------
    // Keys

    /// True when the user quits.
    fn key(&mut self, k: &str) -> bool {
        if let Some((id, _, input)) = self.asks.front().cloned() {
            match k {
                "y" | "Y" | "n" | "N" | "ESC" => {
                    let allow = k.eq_ignore_ascii_case("y");
                    self.claude.answer(&id, &input, allow, "The user said no.");
                    self.asks.pop_front();
                    self.draw_line();
                }
                "C-D" => return true,
                _ => {}
            }
            return false;
        }
        match k {
            "ENTER" => self.enter(),
            "ESC" | "C-C" if self.busy.is_some() => self.claude.interrupt(),
            "C-C" => self.set_input(""),
            "C-D" => return true,
            "PgUP" => self.answer.pageup(),
            "PgDOWN" => self.answer.pagedown(),
            "C-HOME" => self.answer.top(),
            "C-END" => self.answer.bottom(),
            "C-PgUP" => self.side.iter_mut().for_each(Pane::pageup),
            "C-PgDOWN" => self.side.iter_mut().for_each(Pane::pagedown),
            "C-O" => {
                self.detail = !self.detail;
                self.fill_answer(true);
            }
            "C-L" => self.layout(),
            "UP" if self.back > 0 => {
                self.back -= 1;
                let h = self.history[self.back].clone();
                self.set_input(&h);
            }
            "DOWN" if self.back < self.history.len() => {
                self.back += 1;
                let h = self.history.get(self.back).cloned().unwrap_or_default();
                self.set_input(&h);
            }
            "LEFT" => self.at = self.at.saturating_sub(1),
            "RIGHT" => self.at = (self.at + 1).min(self.input.len()),
            "HOME" | "C-A" => self.at = 0,
            "END" | "C-E" => self.at = self.input.len(),
            "BACK" if self.at > 0 => {
                self.at -= 1;
                self.input.remove(self.at);
            }
            "DEL" if self.at < self.input.len() => {
                self.input.remove(self.at);
            }
            "C-U" => {
                self.input.drain(..self.at);
                self.at = 0;
            }
            "C-K" => self.input.truncate(self.at),
            "C-W" | "WBACK" => {
                let mut i = self.at;
                while i > 0 && self.input[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && self.input[i - 1] != ' ' {
                    i -= 1;
                }
                self.input.drain(i..self.at);
                self.at = i;
            }
            _ => {
                let text = match k.strip_prefix("PASTE\0") {
                    Some(p) => p.replace("\r\n", "\n").replace('\r', "\n"),
                    None if k.chars().count() == 1 => k.to_string(),
                    None => return false,
                };
                for c in text.chars() {
                    self.input.insert(self.at, c);
                    self.at += 1;
                }
            }
        }
        self.draw_line();
        false
    }

    fn set_input(&mut self, s: &str) {
        self.input = s.chars().collect();
        self.at = self.input.len();
    }

    fn enter(&mut self) {
        let text: String = self.input.iter().collect::<String>().trim().to_string();
        if text.is_empty() || self.ended.is_some() {
            return;
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.back = self.history.len();
        self.set_input("");
        if self.busy.is_some() {
            self.waiting.push_back(text);
        } else {
            self.send(text);
        }
    }

    // ------------------------------------------------------------------
    // Screen

    fn layout(&mut self) {
        let (cols, rows) = Crust::terminal_size();
        self.rows = rows;
        let h = rows.saturating_sub(3).max(1);
        let sw = if cols >= 80 { (cols / 3).max(28) } else { 0 };
        let one = |p: &mut Pane| {
            p.wrap = false;
            p.scroll = false;
        };
        self.bar = Pane::new(1, 1, cols, 1, 255, 234);
        one(&mut self.bar);
        // One blank column keeps the answer's scroll mark off the log.
        self.answer = Pane::new(1, 2, cols - sw - u16::from(sw > 0), h, 252, 0);
        self.side = (sw > 0).then(|| {
            let mut p = Pane::new(cols - sw + 1, 2, sw, h, 246, 233);
            p.wrap = false;
            p
        });
        self.line = Pane::new(1, rows.saturating_sub(1).max(1), cols, 1, 231, 0);
        one(&mut self.line);
        // Dark grey sets the status line apart from the answer pane above it.
        self.foot = Pane::new(1, rows, cols, 1, 252, 236);
        one(&mut self.foot);
        for t in &mut self.turns {
            t.drawn = None;
        }
        print!("{}", seq::HIDE);
        Crust::clear_screen();
        self.fill_answer(true);
        self.fill_side();
        self.draw_line();
        self.draw_foot();
        self.draw_bar();
        self.place_cursor();
    }

    fn draw_bar(&mut self) {
        let state = if self.ended.is_some() {
            style::fg("✗ Claude Code ended", RED)
        } else if !self.asks.is_empty() {
            style::fg("▲ waiting for your answer", AMBER)
        } else if let Some(t0) = self.busy {
            let s = t0.elapsed().as_secs();
            let start = self.turns.last().map_or(0, |t| t.log_at);
            let tools = self.log[start..].iter().filter(|l| matches!(l, Line::Tool(_))).count();
            let mut st = format!("● working {}:{:02} · {} tool{}", s / 60, s % 60, tools, if tools == 1 { "" } else { "s" });
            if !self.waiting.is_empty() {
                st += &format!(" · {} waiting", self.waiting.len());
            }
            style::fg(&st, AMBER) + &style::dim("   Esc stops")
        } else {
            style::fg("○ ready", GREEN)
        };
        let left = format!(" {}  {}  {}", style::bold("deck"), style::fg(&self.name(), BLUE), state);
        let right = style::dim("PgUp/PgDn scroll · ^O tools · ^D quit ");
        let room = (self.bar.w as usize).saturating_sub(crust::display_width(&left) + crust::display_width(&right));
        let text = if room > 0 { format!("{left}{}{right}", " ".repeat(room)) } else { left };
        self.bar.set_text(&text);
        self.bar.refresh();
    }

    fn draw_foot(&mut self) {
        let text = self.status.clone();
        self.foot.set_text(&text);
        self.foot.refresh();
        self.place_cursor();
    }

    fn draw_line(&mut self) {
        let w = self.line.w as usize;
        let text = if let Some((_, tool, input)) = self.asks.front() {
            style::fg(&format!("Allow {tool}: {}?", claude::what(tool, input)), AMBER) + "  y yes · n no"
        } else {
            let (shown, _) = input_view(&self.input, self.at, w);
            style::fg("› ", BLUE) + &shown
        };
        self.line.set_text(&text);
        self.line.refresh();
        self.place_cursor();
    }

    fn place_cursor(&self) {
        if !self.asks.is_empty() || self.ended.is_some() {
            print!("{}", seq::HIDE);
        } else {
            let (_, col) = input_view(&self.input, self.at, self.line.w as usize);
            Cursor::set(col as u16 + 1, self.line.y);
            print!("{}", seq::SHOW);
        }
        let _ = std::io::stdout().flush();
    }

    /// The answers, oldest first; `jump` scrolls to the latest one's top.
    fn fill_answer(&mut self, jump: bool) {
        let width = (self.answer.w as usize).saturating_sub(2);
        let (before, latest) = if self.detail {
            (String::new(), self.detail_text())
        } else {
            let mut blocks: Vec<String> = Vec::new();
            for t in &mut self.turns {
                blocks.push(turn_text(t, width));
            }
            match &self.ended {
                Some(e) => blocks.push(style::fg("Claude Code ended.", RED) + "\n" + &style::dim(e)),
                None if blocks.is_empty() => blocks.push(style::dim(&if self.s.resume {
                    format!("Session {}. Type below; Enter sends.", self.name())
                } else {
                    format!("New session in {}. Type below; Enter sends.", self.s.cwd)
                })),
                None => {}
            }
            let latest = blocks.pop().unwrap_or_default();
            (blocks.join("\n"), latest)
        };
        if jump {
            self.answer.set_text(&before);
            let n = if before.is_empty() { 0 } else { self.answer.visual_line_count() + 1 };
            self.answer.ix = n;
        }
        let all = if before.is_empty() { latest } else { format!("{before}\n\n{latest}") };
        self.answer.set_text(&all);
        self.answer.refresh();
    }

    fn fill_side(&mut self) {
        let Some(side) = self.side.as_mut() else { return };
        let mut lines = Vec::new();
        for (i, l) in self.log.iter().enumerate() {
            if i > 0 && matches!(l, Line::Prompt(_)) {
                lines.push(String::new());
            }
            lines.push(log_line(l));
        }
        side.set_text(&lines.join("\n"));
        side.bottom();
    }

    /// The last turn's tool calls, each with what went in and came out.
    fn detail_text(&self) -> String {
        let start = self.turns.last().map_or(0, |t| t.log_at);
        let mut out = Vec::new();
        for l in &self.log[start..] {
            let Line::Tool(t) = l else { continue };
            out.push(log_line(l));
            let input = match t.name.as_str() {
                "Bash" => format!("$ {}", t.input["command"].as_str().unwrap_or("")),
                _ => {
                    let s = t.input.to_string();
                    s.chars().take(400).collect()
                }
            };
            out.push(style::fg(&input, CODE));
            let lines: Vec<&str> = t.output.lines().collect();
            for o in lines.iter().take(12) {
                out.push(style::dim(&format!("  {o}")));
            }
            if lines.len() > 12 {
                out.push(style::dim(&format!("  … {} more lines", lines.len() - 12)));
            }
            out.push(String::new());
        }
        if out.is_empty() {
            out.push(style::dim("No tool calls in this turn. C-O goes back."));
        }
        out.join("\n")
    }
}

/// The input as one row: newlines shown as ⏎, scrolled sideways so the
/// cursor stays in view. Also the cursor's column in that row.
fn input_view(input: &[char], at: usize, w: usize) -> (String, usize) {
    let shown: Vec<char> = input.iter().map(|&c| if c == '\n' { '⏎' } else { c }).collect();
    let width = |cs: &[char]| cs.iter().map(|&c| crust::cell_width(c)).sum::<usize>();
    let room = w.saturating_sub(3);
    let mut start = 0;
    while start < at && width(&shown[start..at]) > room {
        start += 1;
    }
    (shown[start..].iter().collect(), 2 + width(&shown[start..at]))
}

fn turn_text(t: &mut Turn, width: usize) -> String {
    let prompt: Vec<String> = t.prompt.lines().map(|l| style::fg(&format!("› {l}"), BLUE)).collect();
    let body = match &t.answer {
        None => style::dim("…"),
        Some(a) if a.trim().is_empty() && t.failed => style::fg("✗ stopped", RED),
        Some(a) => {
            if t.drawn.as_ref().map(|d| d.0) != Some(width) {
                t.drawn = Some((width, render(a, width)));
            }
            t.drawn.as_ref().map(|d| d.1.clone()).unwrap_or_default()
        }
    };
    format!("{}\n\n{}", prompt.join("\n"), body)
}

fn log_line(l: &Line) -> String {
    let first = |s: &str| s.lines().next().unwrap_or("").to_string();
    match l {
        Line::Prompt(p) => style::fg(&format!("› {}", first(p)), BLUE),
        Line::Tool(t) => {
            let mark = match t.failed {
                None => style::fg("·", AMBER),
                Some(false) => style::fg("✓", GREEN),
                Some(true) => style::fg("✗", RED),
            };
            let name = t.name.rsplit("__").next().unwrap_or(&t.name);
            format!("{mark} {} {}", style::bold(name), t.what)
        }
        Line::Note(n) => style::dim(&format!("  {}", first(n))),
        Line::Rewrite(r) => style::fg(&format!("↻ rewrite: {r}"), AMBER),
        Line::Stopped => style::fg("✗ stopped", RED),
        Line::Ended => style::fg("✗ Claude Code ended", RED),
    }
}

/// "claude-opus-5-5" as "Opus 5.5".
fn model_name(id: &str) -> String {
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    let mut parts = rest.split('-');
    let Some(family) = parts.next().filter(|f| !f.is_empty()) else { return "Claude".into() };
    let nums: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    let mut name: String = family[..1].to_uppercase() + &family[1..];
    if !nums.is_empty() {
        name = format!("{name} {}", nums.join("."));
    }
    name
}

/// An answer as the terminal shows it: tables drawn with lines, then the
/// HyperList drawing when the user has it on, then light markdown.
fn render(md: &str, width: usize) -> String {
    let md = crust::text::format_markdown_tables(md, width);
    let md = hyperlist(&md).unwrap_or(md);
    let mut out = Vec::new();
    let mut code = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        let t = line.trim_start();
        if code {
            out.push(style::fg(line, CODE));
        } else if t.starts_with('#') {
            out.push(style::bold(t.trim_start_matches('#').trim()));
        } else {
            out.push(inline(line));
        }
    }
    out.join("\n")
}

/// `code` and **bold** inside a line.
fn inline(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    loop {
        let tick = rest.find('`').and_then(|i| rest[i + 1..].find('`').map(|j| (i, 1, j)));
        let star = rest.find("**").and_then(|i| rest[i + 2..].find("**").map(|j| (i, 2, j)));
        let Some((i, n, j)) = [tick, star].into_iter().flatten().min_by_key(|m| m.0) else { break };
        let inner = &rest[i + n..i + n + j];
        out.push_str(&rest[..i]);
        out.push_str(&if n == 1 { style::fg(inner, CODE) } else { style::bold(inner) });
        rest = &rest[i + n + j + n..];
    }
    out.push_str(rest);
    out
}

/// The answer through the HyperList display hook, when it is installed and
/// on (`/hl`). Claude Code runs it for the screen; deck does the same.
fn hyperlist(md: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let hook = format!("{home}/.claude/hooks/hyperlist-display-asm");
    if !std::path::Path::new(&hook).exists() || std::path::Path::new(&format!("{home}/.claude/hyperlist-display.off")).exists() {
        return None;
    }
    let mut child = Command::new(hook).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let payload = json!({"delta": md, "index": 0, "final": true}).to_string();
    child.stdin.take()?.write_all(payload.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["hookSpecificOutput"]["displayContent"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_name(""), "Claude");
    }

    #[test]
    fn inline_marks() {
        assert_eq!(inline("a `b` c"), format!("a {} c", style::fg("b", CODE)));
        assert_eq!(inline("**x** and `y`"), format!("{} and {}", style::bold("x"), style::fg("y", CODE)));
        assert_eq!(inline("one ` alone"), "one ` alone");
    }

    #[test]
    fn the_cursor_stays_in_view() {
        let input: Vec<char> = "abcdefghij\n".chars().collect();
        assert_eq!(input_view(&input, 11, 8), ("ghij⏎".to_string(), 7));
        assert_eq!(input_view(&input, 0, 8), ("abcdefghij⏎".to_string(), 2));
    }
}
