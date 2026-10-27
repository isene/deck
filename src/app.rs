//! deck: a paned front for Claude Code. The answer gets the big pane, each
//! tool call one line in the log beside it, and the status line keeps the
//! bottom row. When a stop hook sends an answer back, only the rewrite is
//! shown, with one line in the log saying so.

use crate::claude::{Claude, Event, WAKE};
use crate::{claude, session};
use crust::{seq, style, Crust, Cursor, Input, Pane, Popup};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

const USAGE: &str = "deck: a paned front for Claude Code

Usage:
  deck              start a new session in this directory
  deck <tag>        open the session with this bookmark tag
  deck <id>         open the session with this id

Keys:
";

/// The keys, for `deck --help` and the C-? popup.
const KEYS: &[(&str, &str)] = &[
    ("Enter", "send (while Claude works, it waits its turn, in purple)"),
    ("S-Enter, C-J", "a new line in the prompt; so does \\ before Enter"),
    ("Esc", "stop the turn Claude is on"),
    ("Up, Down", "the row above or below, else earlier prompts"),
    ("! command", "run it in a shell; Claude gets what it printed with the next prompt"),
    ("C-A, C-E", "to the start or end of the prompt"),
    ("C-U, C-K", "delete to the start or end of the prompt"),
    ("C-Backspace", "delete a word"),
    ("C-G", "edit the prompt in $EDITOR"),
    ("PgUp, PgDn", "scroll the answers"),
    ("C-F", "find in the answers; C-F, Enter finds the next"),
    ("C-PgUp, C-PgDn", "scroll the tool log"),
    ("C-O, C-T", "the last turn's tool calls in full, and back"),
    ("C-Y", "copy the last answer, as drawn"),
    ("C-S-Y", "copy what the answer pane shows"),
    ("C-W, C-S-W", "answer pane a tenth wider, narrower"),
    ("C-B", "pick the answer pane's colour (prism)"),
    ("C-L", "draw the screen again"),
    ("C-?", "this help"),
    ("C-D", "quit; during a turn, press it twice"),
];

const VERSION: &str = env!("CARGO_PKG_VERSION");

const BLUE: u8 = 39;
const CODE: u8 = 179;
const GREEN: u8 = 78;
const RED: u8 = 203;
const AMBER: u8 = 214;
const WAIT: u8 = 141;
/// C-D during a turn shows this first; a second C-D stops the turn and quits.
const QUIT_WARN: &str = "Claude is working: C-D again stops the turn and quits";

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

pub struct App {
    s: session::Session,
    claude: Claude,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    running: bool,
    /// Under gluon: its tab names, at the start of the top bar.
    tab_line: Option<String>,
    /// Under gluon: what was on screen when Ctrl+A came here, for Claude
    /// with the next prompt.
    seen: String,
    /// Under gluon: another tab is in front, so the end of a turn shows a
    /// box as when another window is.
    hidden: bool,
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
    /// What was typed before Up went back through the history.
    draft: String,
    /// What `!` commands printed, for Claude with the next prompt.
    shell_out: String,
    detail: bool,
    /// The answer pane's share of the screen, in tenths (5 to 9).
    width: u16,
    /// A short note in the top bar until the next key, such as "copied".
    flash: Option<String>,
    /// The session's background in fleet, for the answer pane.
    colour: Option<String>,
    /// The last text C-F looked for.
    found: String,
    /// True while another window is in front, so the end of a turn shows
    /// a box (herald, through notify-send).
    away: bool,
    rows: u16,
    bar: Pane,
    answer: Pane,
    side: Option<Pane>,
    /// The column between the answer and the log, in the answer's colour.
    gap: Option<Pane>,
    line: Pane,
    foot: Pane,
}

/// The deck program: start, the main loop, finish.
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return KEYS.iter().for_each(|(k, what)| println!("  {k:<16}  {what}"));
        }
        Some("-v" | "--version") => return println!("deck {VERSION}"),
        _ => {}
    }
    let mut app = start(args.first().map(String::as_str), None).unwrap_or_else(|e| quit(&e));
    while app.running {
        let wait = app.before_wait();
        let key = Input::getchr(Some(wait));
        app.after_wait(key);
    }
    finish(app);
}

/// Open the session `arg` names (a tag or an id), or a new one in `dir`
/// (else in the current directory), start Claude Code in it and draw the
/// screen.
pub fn start(arg: Option<&str>, dir: Option<&str>) -> Result<App, String> {
    let mut s = session::find(arg)?;
    if s.resume {
        if let Some(pid) = session::open_elsewhere(&s.id) {
            return Err(format!("That session is open in Claude Code already (pid {pid}). Close it there first."));
        }
    } else if let Some(d) = dir {
        s.cwd = d.to_string();
    }
    let last = if s.resume { session::last(&s.id) } else { session::Last::default() };
    let errors = format!("{}/deck-{}.err", std::env::var("XDG_RUNTIME_DIR").unwrap_or("/tmp".into()), std::process::id());
    let file = fs::File::create(&errors).map_err(|e| format!("{errors}: {e}"))?;
    // HL_RAW: the style hook leaves the HyperList drawing to deck, which
    // knows the pane's width.
    let mut env = vec![("CC_SESSION_ID", s.id.clone()), ("HL_RAW", "1".to_string())];
    if !s.tags.is_empty() {
        env.push(("CC_RESUME_TAGS", s.tags.join(" ")));
    }
    let (tx, rx) = mpsc::channel();
    let claude = Claude::start(&s.id, s.resume, &s.cwd, &env, file, tx.clone())
        .map_err(|e| format!("Could not start Claude Code: {e}"))?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        Crust::disable_focus_reports();
        Crust::disable_modifier_keys();
        Crust::cleanup();
        default_hook(p);
    }));
    Crust::init();
    Crust::set_app_identity(&title(&s));
    Crust::enable_bracketed_paste();
    Crust::enable_ctrl_shift_keys();
    Crust::enable_focus_reports();

    let mut app = App::new(s, last, claude, tx, rx, errors);
    app.layout();
    // The key reader catches the wake-up signal only once it has started.
    // Start it, then take what came before, so nothing waits for a key.
    Input::peek_pending();
    WAKE.store(false, Ordering::SeqCst);
    app.drain();
    app.run_status();
    Ok(app)
}

/// End Claude Code and give the terminal back.
pub fn finish(mut app: App) {
    app.claude.stop(app.busy.is_some());
    Crust::disable_focus_reports();
    Crust::disable_modifier_keys();
    Crust::disable_bracketed_paste();
    Crust::cleanup();
    let _ = fs::remove_file(&app.errors);
}

fn quit(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(1)
}

/// The session's tag, or the start of its id.
fn name(s: &session::Session) -> String {
    match s.tags.first() {
        Some(t) => format!("#{t}"),
        None => s.id[..8].to_string(),
    }
}

/// The window's title: "Deck #pf".
fn title(s: &session::Session) -> String {
    format!("Deck {}", name(s))
}

/// True once the binary this deck runs from was rebuilt, so the bottom
/// row can say so. One readlink per draw of that row.
fn rebuilt() -> bool {
    fs::read_link("/proc/self/exe").is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"))
}

impl App {
    /// One turn of the main loop is `before_wait`, a wait of at most the
    /// seconds it gives for a key, then `after_wait` with the key or None.
    /// While Claude works the bar shows the time, so it wakes once a
    /// second; otherwise it sleeps until a key, or until Claude's output
    /// arrives (the reader then sends SIGWINCH, read as "RESIZE").
    pub fn before_wait(&self) -> u64 {
        if self.busy.is_some() { 1 } else { 86400 }
    }

    pub fn after_wait(&mut self, key: Option<String>) {
        if let Some(k) = key.as_deref() {
            if k == "RESIZE" {
                if Crust::terminal_size() != (self.bar.w, self.rows) {
                    self.layout();
                }
            } else if k == "FOCUS_IN" || k == "FOCUS_OUT" {
                self.away = k == "FOCUS_OUT";
            } else if self.key(k) {
                self.running = false;
                return;
            }
        }
        // Cleared before the drain, so an event sent after it wakes again.
        WAKE.store(false, Ordering::SeqCst);
        self.drain();
        self.draw_bar();
        self.place_cursor();
    }

    pub fn running(&self) -> bool { self.running }

    /// Under gluon: the tab names to start the top bar with.
    pub fn set_tab_line(&mut self, tabs: Option<String>) {
        self.tab_line = tabs;
    }

    /// Draw everything again from nothing, after another tab had the
    /// screen; it also takes up a resize made meanwhile.
    pub fn repaint(&mut self) {
        self.layout();
    }

    /// Under gluon: true while another tab is in front.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    /// Under gluon: what the tab before had on screen. Claude gets it with
    /// the next prompt, and a line in the log says so.
    pub fn attach(&mut self, what: &str, text: &str) {
        self.seen = format!("<on-screen app=\"{what}\">\n{text}\n</on-screen>\n");
        self.log.push(Line::Note(format!("{what} on screen goes with the next prompt")));
        self.fill_side();
    }

    /// The session's id, so gluon can open it again next time.
    pub fn session_id(&self) -> &str {
        &self.s.id
    }

    fn new(s: session::Session, last: session::Last, claude: Claude, tx: Sender<Event>, rx: Receiver<Event>, errors: String) -> App {
        let colour = session::colour(&s.tags);
        let window = known_window(&last.model);
        let history = load_history(&s.cwd);
        let mut app = App {
            s,
            claude,
            tx,
            rx,
            running: true,
            tab_line: None,
            seen: String::new(),
            hidden: false,
            model: last.model,
            version: String::new(),
            context: last.context,
            window,
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
            back: history.len(),
            history,
            shell_out: String::new(),
            draft: String::new(),
            detail: false,
            flash: None,
            width: cache()["width"].as_u64().map_or(7, |w| w.clamp(5, 9) as u16),
            colour,
            found: String::new(),
            away: false,
            rows: 0,
            bar: Pane::new(1, 1, 1, 1, 255, 234),
            answer: Pane::new(1, 1, 1, 1, 252, 0),
            side: None,
            gap: None,
            line: Pane::new(1, 1, 1, 1, 231, 0),
            foot: Pane::new(1, 1, 1, 1, 252, 0),
        };
        for (prompt, answer) in last.turns {
            // A prompt with no answer had its turn cut short.
            let failed = answer.is_empty();
            app.turns.push(Turn { prompt: prompt.clone(), answer: Some(answer), failed, log_at: app.log.len(), drawn: None });
            app.log.push(Line::Prompt(prompt));
        }
        app
    }

    fn name(&self) -> String {
        name(&self.s)
    }

    // ------------------------------------------------------------------
    // Claude's side

    fn drain(&mut self) {
        let (mut answer, mut log, mut status) = (false, false, false);
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Event::Init { session, model, version } => {
                    self.s.id = session;
                    if self.window == 0 {
                        self.window = known_window(&model);
                    }
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
                    if tool == "ExitPlanMode" {
                        self.claude.answer(&id, &input, false, "deck cannot show this dialog. Ask in plain text instead.");
                    } else {
                        let what = match tool.as_str() {
                            "AskUserQuestion" => input["questions"][0]["question"].as_str().unwrap_or("").to_string(),
                            _ => format!("Allow {tool}: {}?", claude::what(&tool, &input)),
                        };
                        self.notify(&what);
                        self.asks.push_back((id, tool, input));
                    }
                }
                Event::Answer { text, failed, window } => {
                    if window > 0 && window != self.window {
                        self.window = window;
                        remember_window(&self.model, window);
                    }
                    // The final text came as a note too; the answer pane has it.
                    if let Some(Line::Note(n)) = self.log.last() {
                        if n.trim() == text.trim() {
                            self.log.pop();
                        }
                    }
                    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
                    self.notify(if failed && first.is_empty() { "stopped" } else { first });
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
            self.fill_answer(false);
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
        if self.asks.front().is_some_and(|a| a.1 == "AskUserQuestion") {
            self.ask_questions();
        }
    }

    /// A box at the top right (herald), only while another window is in
    /// front. One notify-send per end of turn or question, never more.
    fn notify(&self, what: &str) {
        if !self.away && !self.hidden {
            return;
        }
        let what: String = what.chars().take(120).collect();
        let title = title(&self.s);
        std::thread::spawn(move || {
            let _ = Command::new("notify-send").args(["-a", "deck", &title, &what]).status();
        });
    }

    /// Claude's multiple-choice questions, one menu each. Esc on a menu
    /// answers none of them, and Claude is told so.
    fn ask_questions(&mut self) {
        let Some((id, _, mut input)) = self.asks.front().cloned() else { return };
        self.draw_line();
        let mut answers = serde_json::Map::new();
        for q in input["questions"].as_array().cloned().unwrap_or_default() {
            match self.ask_one(&q) {
                Some(a) => {
                    answers.insert(q["question"].as_str().unwrap_or("").to_string(), json!(a));
                }
                None => {
                    self.claude.answer(&id, &input, false, "The user closed the question without an answer.");
                    self.asks.pop_front();
                    return self.layout();
                }
            }
        }
        input["answers"] = Value::Object(answers);
        self.claude.answer(&id, &input, true, "");
        self.asks.pop_front();
        self.layout();
    }

    /// One question as a menu: its options, then "Type an answer". With
    /// several allowed, Enter ticks an option and "Done" sends the ticked.
    fn ask_one(&mut self, q: &Value) -> Option<String> {
        let multi = q["multiSelect"].as_bool() == Some(true);
        let options: Vec<(String, String)> = q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| (o["label"].as_str().unwrap_or("").to_string(), o["description"].as_str().unwrap_or("").to_string()))
            .collect();
        let (cols, rows) = Crust::terminal_size();
        let w = (cols as usize).saturating_sub(8).min(90);
        let head = word_wrap(q["question"].as_str().unwrap_or(""), w.saturating_sub(2));
        let first = head.len() + 1;
        let mut ticked = vec![false; options.len()];
        let mut at = first;
        loop {
            let mut items: Vec<String> = head.iter().map(|l| format!(" {}", style::bold(l))).collect();
            items.push(String::new());
            for (i, (label, about)) in options.iter().enumerate() {
                let tick = match (multi, ticked[i]) {
                    (false, _) => "",
                    (true, true) => "[x] ",
                    (true, false) => "[ ] ",
                };
                items.push(format!(" {tick}{}  {}", style::fg(label, GREEN), style::dim(about)));
            }
            items.push(style::dim(" Type an answer"));
            if multi {
                items.push(" Done".into());
            }
            let h = (items.len() as u16).min(rows.saturating_sub(4));
            let mut pop = Popup::centered(w as u16 + 2, h, 231, 236);
            pop.pane.index = at;
            at = pop.modal(&items.join("\n"))?;
            let n = at.checked_sub(first);
            match n {
                Some(i) if i < options.len() && !multi => return Some(options[i].0.clone()),
                Some(i) if i < options.len() => ticked[i] = !ticked[i],
                Some(i) if i == options.len() => {
                    self.line.set_text("");
                    self.line.refresh();
                    let typed = self.line.ask_or_cancel("Your answer: ", "");
                    self.line.prompt.clear();
                    match typed.map(|t| t.trim().to_string()) {
                        Some(t) if !t.is_empty() => return Some(t),
                        _ => self.draw_line(),
                    }
                }
                Some(_) => {
                    let chosen: Vec<&str> = options.iter().zip(&ticked).filter(|(_, &t)| t).map(|(o, _)| o.0.as_str()).collect();
                    if !chosen.is_empty() {
                        return Some(chosen.join(", "));
                    }
                }
                // The question's own rows: back to the first option.
                None => at = first,
            }
        }
    }

    fn send(&mut self, text: String) {
        self.turns.push(Turn { prompt: text.clone(), answer: None, failed: false, log_at: self.log.len(), drawn: None });
        self.log.push(Line::Prompt(text.clone()));
        let out = std::mem::take(&mut self.shell_out);
        let seen = std::mem::take(&mut self.seen);
        self.claude.say(&format!("{seen}{out}{text}"));
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
        let armed = self.flash.as_deref() == Some(QUIT_WARN);
        self.flash = None;
        if k == "C-D" && self.busy.is_some() && !armed {
            self.flash = Some(QUIT_WARN.into());
            return false;
        }
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
            "C-O" | "C-T" => {
                self.detail = !self.detail;
                self.fill_answer(true);
            }
            "C-L" => {
                // Drawn again from the start, so a /hl switch shows too.
                for t in &mut self.turns {
                    t.drawn = None;
                }
                self.layout();
            }
            "C-G" => self.edit_input(),
            "C-Y" => self.copy_answer(),
            "C-S-Y" => self.copy_view(),
            "C-B" => self.pick_colour(),
            "C-?" => self.show_help(),
            "C-J" | "S-ENTER" => {
                self.input.insert(self.at, '\n');
                self.at += 1;
            }
            "C-F" => self.find(),
            "UP" | "DOWN" => self.up_down(k == "UP"),
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
            "C-W" => self.widen(1),
            "C-S-W" => self.widen(-1),
            "WBACK" => {
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

    /// The answer pane a tenth of the screen wider (or narrower), from half
    /// to nine tenths and round again, as w and W do in kastrup.
    fn widen(&mut self, step: i16) {
        self.width = (self.width as i16 - 5 + step).rem_euclid(5) as u16 + 5;
        let mut v = cache();
        v["width"] = json!(self.width);
        save_cache(&v);
        self.layout();
    }

    /// The last answer as drawn, HyperList and all, as plain text.
    fn copy_answer(&mut self) {
        let width = (self.answer.w as usize).saturating_sub(2);
        let last = self.turns.iter_mut().rev().find(|t| t.answer.as_deref().is_some_and(|a| !a.trim().is_empty()));
        let Some(t) = last else {
            self.flash = Some("no answer to copy".into());
            return;
        };
        let text = crust::strip_ansi(&drawn(t, width));
        let text = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
        self.copy(&text, "the answer");
    }

    /// What the answer pane shows, scrolled or not, as plain text.
    fn copy_view(&mut self) {
        let rows: Vec<String> = self.answer.visual_lines().iter()
            .skip(self.answer.ix)
            .take(self.answer.h as usize)
            .map(|l| crust::strip_ansi(l).trim_end().to_string())
            .collect();
        let text = rows.join("\n").trim_matches('\n').to_string();
        self.copy(&text, "the view");
    }

    /// Look for text in the answers: the next row that has it comes to the
    /// top of the pane. C-F then Enter looks again from there.
    fn find(&mut self) {
        self.line.set_text("");
        self.line.refresh();
        let asked = self.line.ask_or_cancel("Find: ", &self.found);
        self.line.prompt.clear();
        let Some(what) = asked.map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) else {
            return;
        };
        let again = what == self.found;
        self.found = what.clone();
        let want = what.to_lowercase();
        let hits: Vec<usize> = self.answer.visual_lines().iter().enumerate()
            .filter(|(_, l)| crust::strip_ansi(l).to_lowercase().contains(&want))
            .map(|(i, _)| i)
            .collect();
        let from = self.answer.ix + usize::from(again);
        let Some(&row) = hits.iter().find(|&&i| i >= from).or(hits.first()) else {
            self.flash = Some(format!("no \"{what}\" in the answers"));
            return;
        };
        self.answer.ix = row;
        self.answer.refresh();
        let n = hits.iter().position(|&i| i == row).unwrap_or(0) + 1;
        self.flash = Some(format!("\"{what}\" {n} of {}", hits.len()));
    }

    /// Text to the clipboard. OSC 52 hands it to the terminal, so no
    /// program is started; glass takes about 12 KB of text that way.
    fn copy(&mut self, text: &str, what: &str) {
        if text.len().div_ceil(3) * 4 > 16_000 {
            self.flash = Some(format!("{what} is too long for the clipboard"));
            return;
        }
        crust::clipboard_copy(text, "clipboard");
        self.flash = Some(format!("copied {what}, {} lines", text.lines().count()));
    }

    /// The answer pane's colour, picked in prism and kept in ~/.fleetrc,
    /// where fleet's b keeps it too.
    fn pick_colour(&mut self) {
        let out = self.errors.replace(".err", ".prism");
        let mut cmd = Command::new("prism");
        cmd.args(["--pick", "--hex", &format!("--out={out}")]);
        if let Some(c) = &self.colour {
            cmd.arg(format!("#{c}"));
        }
        self.hand_over(&mut cmd);
        // prism writes "fg=#RRGGBB"; --pick fills the fg slot.
        let hex = fs::read_to_string(&out)
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("fg=")).map(|h| h.trim().trim_start_matches('#').to_lowercase()))
            .filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = fs::remove_file(&out);
        self.flash = Some(match &hex {
            Some(h) if session::set_colour(&self.s.tags, h) => format!("colour #{h}"),
            Some(h) => format!("colour #{h} until deck quits: the session has no tag"),
            None => "no colour picked".into(),
        });
        if hex.is_some() {
            self.colour = hex;
        }
        self.layout();
    }

    /// The keys in a popup, as in the other Fe2O3 programs. Esc, q or Enter
    /// closes it.
    fn show_help(&mut self) {
        let mut t = format!(" {}\n", style::bold(&format!("deck v{VERSION}")));
        for (k, what) in KEYS {
            t += &format!("{}{what}\n", style::fg(&format!("  {k:<16}"), GREEN));
        }
        let t = t.trim_end();
        let (cols, rows) = Crust::terminal_size();
        let w = t.lines().map(crust::display_width).max().unwrap_or(0) as u16 + 4;
        let h = t.lines().count() as u16;
        let mut pop = Popup::centered(w.min(cols.saturating_sub(4)), h.min(rows.saturating_sub(4)), 231, 236);
        pop.view(t);
        self.layout();
    }

    /// The prompt in $EDITOR (scribe if unset), as C-G does in Claude Code.
    /// The editor has the terminal until it quits; what it saved comes back.
    fn edit_input(&mut self) {
        let path = self.errors.replace(".err", ".md");
        if fs::write(&path, self.input.iter().collect::<String>()).is_err() {
            return;
        }
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "scribe".into());
        self.hand_over(Command::new("sh").args(["-c", &format!("{editor} \"$1\""), "sh", &path]));
        if let Ok(text) = fs::read_to_string(&path) {
            self.set_input(text.trim_end());
        }
        let _ = fs::remove_file(&path);
        self.layout();
    }

    /// The terminal to a program, such as the editor or prism, until it
    /// quits.
    fn hand_over(&self, cmd: &mut Command) -> Option<i32> {
        Crust::disable_focus_reports();
        Crust::disable_modifier_keys();
        Crust::disable_bracketed_paste();
        Crust::cleanup();
        let code = cmd.status().ok().and_then(|s| s.code());
        Crust::init();
        Crust::set_app_identity(&title(&self.s));
        Crust::enable_bracketed_paste();
        Crust::enable_ctrl_shift_keys();
        Crust::enable_focus_reports();
        code
    }

    /// Up and Down move between the prompt's rows, and past the first or
    /// last row to the prompts sent before, as in Claude Code.
    fn up_down(&mut self, up: bool) {
        let rows = input_rows(&self.input, self.line.w as usize);
        let (r, col) = cursor_at(&rows, &self.input, self.at);
        let to = if up { r.checked_sub(1) } else { Some(r + 1).filter(|&n| n < rows.len()) };
        if let Some(n) = to {
            self.at = at_column(rows[n], &self.input, col);
        } else if up && self.back > 0 {
            if self.back == self.history.len() {
                self.draft = self.input.iter().collect();
            }
            self.back -= 1;
            let h = self.history[self.back].clone();
            self.set_input(&h);
        } else if !up && self.back < self.history.len() {
            self.back += 1;
            let h = self.history.get(self.back).cloned().unwrap_or_else(|| std::mem::take(&mut self.draft));
            self.set_input(&h);
        }
    }

    fn set_input(&mut self, s: &str) {
        self.input = s.chars().collect();
        self.at = self.input.len();
    }

    fn enter(&mut self) {
        // A backslash before Enter asks for a new line, as in Claude Code.
        if self.at > 0 && self.input[self.at - 1] == '\\' {
            self.input[self.at - 1] = '\n';
            return;
        }
        let text: String = self.input.iter().collect::<String>().trim().to_string();
        if text.is_empty() || self.ended.is_some() {
            return;
        }
        if let Some(cmd) = text.strip_prefix('!') {
            if self.busy.is_some() {
                self.flash = Some("a ! command waits until Claude's turn ends".into());
                return;
            }
            self.remember(&text);
            return self.shell(cmd.trim());
        }
        self.remember(&text);
        if self.busy.is_some() {
            self.waiting.push_back(text);
            self.fill_answer(true);
        } else {
            self.send(text);
        }
    }

    /// A sent prompt goes to Up and Down, and to Claude Code's own history
    /// file, so both find it again.
    fn remember(&mut self, text: &str) {
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
        }
        self.back = self.history.len();
        self.draft.clear();
        self.set_input("");
        let line = json!({"display": text, "pastedContents": {}, "timestamp": now_ms(),
                          "project": self.s.cwd, "sessionId": self.s.id});
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(history_file()) {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }

    /// `!` runs a command on the terminal itself, so a password prompt
    /// works. `script` keeps a copy of what it prints, which deck shows and
    /// Claude gets with the next prompt, as in Claude Code.
    fn shell(&mut self, cmd: &str) {
        let log = self.errors.replace(".err", ".sh");
        let mut run = Command::new("script");
        run.args(["-qec", cmd, &log]).current_dir(&self.s.cwd);
        let code = self.hand_over(&mut run);
        let out = typescript(&fs::read_to_string(&log).unwrap_or_default());
        let _ = fs::remove_file(&log);
        let tail: String = out.chars().rev().take(30_000).collect::<Vec<_>>().into_iter().rev().collect();
        self.shell_out += &format!("<bash-input>{cmd}</bash-input>\n<bash-stdout>{tail}</bash-stdout>\n");
        let failed = code.is_some_and(|c| c != 0);
        let mut answer = format!("```\n{out}\n```");
        if let Some(c) = code.filter(|_| failed) {
            answer += &format!("\nexit {c}");
        }
        let prompt = format!("! {cmd}");
        self.turns.push(Turn { prompt: prompt.clone(), answer: Some(answer), failed, log_at: self.log.len(), drawn: None });
        self.log.push(Line::Prompt(prompt));
        self.layout();
    }

    // ------------------------------------------------------------------
    // Screen

    fn layout(&mut self) {
        let (cols, rows) = Crust::terminal_size();
        self.rows = rows;
        let n = self.input_height(cols);
        let h = rows.saturating_sub(2 + n).max(1);
        let sw = if cols >= 80 { cols * (10 - self.width) / 10 } else { 0 };
        let one = |p: &mut Pane| {
            p.wrap = false;
            p.scroll = false;
        };
        self.bar = Pane::new(1, 1, cols, 1, 255, 234);
        one(&mut self.bar);
        // One blank column keeps the answer's scroll mark off the log.
        self.answer = Pane::new(1, 2, cols - sw - u16::from(sw > 0), h, 252, 0);
        if let Some(hex) = &self.colour {
            self.answer.set_bg_hex(hex);
        }
        self.gap = (sw > 0).then(|| {
            let mut p = Pane::new(cols - sw, 2, 1, h, 252, 0);
            one(&mut p);
            if let Some(hex) = &self.colour {
                p.set_bg_hex(hex);
            }
            p
        });
        self.side = (sw > 0).then(|| {
            let mut p = Pane::new(cols - sw + 1, 2, sw, h, 246, 233);
            p.wrap = false;
            p
        });
        self.line = Pane::new(1, rows.saturating_sub(n).max(1), cols, n, 231, 0);
        one(&mut self.line);
        // Dark grey sets the status line apart from the answer pane above it.
        self.foot = Pane::new(1, rows, cols, 1, 252, 236);
        one(&mut self.foot);
        print!("{}", seq::HIDE);
        Crust::clear_screen();
        self.gap.iter_mut().for_each(Pane::refresh);
        self.fill_answer(false);
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
        let mut left = match &self.tab_line {
            Some(tabs) => format!("{tabs} {}  {}", style::fg(&self.name(), BLUE), state),
            None => format!(" {}  {}  {}", style::bold("deck"), style::fg(&self.name(), BLUE), state),
        };
        if let Some(f) = &self.flash {
            left += &style::dim(&format!(" · {f}"));
        }
        let tools = if self.tab_line.is_some() { "^T" } else { "^O" };
        let right = style::dim(&format!("PgUp/PgDn scroll · {tools} tools · ^Y copy · ^? help · ^D quit "));
        let room = (self.bar.w as usize).saturating_sub(crust::display_width(&left) + crust::display_width(&right));
        let text = if room > 0 { format!("{left}{}{right}", " ".repeat(room)) } else { left };
        self.bar.set_text(&text);
        self.bar.refresh();
    }

    /// The status line, and deck's version at the right, as in the other
    /// Fe2O3 programs.
    fn draw_foot(&mut self) {
        let old = rebuilt();
        let version = if old { format!("deck v{VERSION} is old: restart ") } else { format!("deck v{VERSION} ") };
        let room = (self.foot.w as usize).saturating_sub(version.len() + 1);
        let status = crust::truncate_ansi(&self.status, room);
        let pad = " ".repeat((self.foot.w as usize).saturating_sub(version.len() + crust::display_width(&status)));
        // Amber when old, as in kastrup and tock.
        let version = if old { style::bold(&style::fg(&version, 208)) } else { style::dim(&version) };
        self.foot.set_text(&format!("{status}{pad}{version}"));
        self.foot.refresh();
        self.place_cursor();
    }

    /// The rows the prompt takes: as many as it has, up to a third of the
    /// screen, and then it scrolls.
    fn input_height(&self, cols: u16) -> u16 {
        if !self.asks.is_empty() {
            return 1;
        }
        let most = (self.rows as usize / 3).clamp(1, 10);
        input_rows(&self.input, cols as usize).len().min(most) as u16
    }

    /// The prompt grew or shrank by a row: the panes above give or take it.
    /// They are drawn whole, since the prompt wrote over rows they had.
    fn reshape(&mut self, n: u16) {
        let h = self.rows.saturating_sub(2 + n).max(1);
        let at_end = !self.answer.moredown;
        self.answer.h = h;
        self.answer.invalidate();
        if at_end {
            self.answer.bottom();
        } else {
            self.answer.refresh();
        }
        if let Some(side) = self.side.as_mut() {
            side.h = h;
            side.invalidate();
            side.bottom();
        }
        if let Some(gap) = self.gap.as_mut() {
            gap.h = h;
            gap.full_refresh();
        }
        self.line = Pane::new(1, self.rows.saturating_sub(n).max(1), self.bar.w, n, 231, 0);
        self.line.wrap = false;
        self.line.scroll = false;
    }

    fn draw_line(&mut self) {
        let n = self.input_height(self.bar.w);
        if n != self.line.h {
            self.reshape(n);
        }
        let w = self.line.w as usize;
        let text = if self.asks.front().is_some_and(|a| a.1 == "AskUserQuestion") {
            style::fg("Claude asks", AMBER) + "  Enter picks · Esc answers nothing"
        } else if let Some((_, tool, input)) = self.asks.front() {
            style::fg(&format!("Allow {tool}: {}?", claude::what(tool, input)), AMBER) + "  y yes · n no"
        } else {
            let rows = input_rows(&self.input, w);
            let top = self.input_top(&rows);
            let shown: Vec<String> = rows[top..].iter().take(self.line.h as usize).enumerate().map(|(i, &(s, e))| {
                let lead = if top + i == 0 { style::fg("› ", BLUE) } else { "  ".into() };
                lead + &self.input[s..e].iter().collect::<String>()
            }).collect();
            shown.join("\n")
        };
        self.line.set_text(&text);
        self.line.refresh();
        self.place_cursor();
    }

    /// The first row of the prompt in view: the cursor's row stays shown.
    fn input_top(&self, rows: &[(usize, usize)]) -> usize {
        let (r, _) = cursor_at(rows, &self.input, self.at);
        (r + 1).saturating_sub(self.line.h as usize)
    }

    fn place_cursor(&self) {
        if !self.asks.is_empty() || self.ended.is_some() {
            print!("{}", seq::HIDE);
        } else {
            let rows = input_rows(&self.input, self.line.w as usize);
            let (r, col) = cursor_at(&rows, &self.input, self.at);
            let y = self.line.y + (r - self.input_top(&rows)) as u16;
            Cursor::set(col as u16 + 1, y);
            print!("{}", seq::SHOW);
        }
        let _ = std::io::stdout().flush();
    }

    /// The answers, oldest first. The pane follows the end, as a terminal
    /// does, unless it was scrolled up to read; `end` sends it there anyway.
    fn fill_answer(&mut self, end: bool) {
        let at_end = end || !self.answer.moredown;
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
            let mut latest = blocks.pop().unwrap_or_default();
            if !self.waiting.is_empty() {
                latest += &format!("\n\n{}", waiting_text(&self.waiting));
            }
            (blocks.join("\n\n"), latest)
        };
        let all = if before.is_empty() { latest } else { format!("{before}\n\n{latest}") };
        self.answer.set_text(&all);
        if at_end {
            self.answer.bottom();
        } else {
            self.answer.refresh();
        }
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

/// The prompt in rows of `w` columns, each a range of `input`: "› " or two
/// spaces lead each row, and a newline starts a new one (it shows in none).
/// One column stays free for the cursor at the end of a row.
fn input_rows(input: &[char], w: usize) -> Vec<(usize, usize)> {
    let room = w.saturating_sub(3).max(1);
    let mut rows = Vec::new();
    let (mut start, mut cols) = (0, 0);
    for (i, &c) in input.iter().enumerate() {
        if c == '\n' {
            rows.push((start, i));
            (start, cols) = (i + 1, 0);
            continue;
        }
        let cw = crust::cell_width(c);
        if cols + cw > room {
            rows.push((start, i));
            (start, cols) = (i, 0);
        }
        cols += cw;
    }
    rows.push((start, input.len()));
    rows
}

/// The cursor's row, and its column counted from the pane's left edge.
fn cursor_at(rows: &[(usize, usize)], input: &[char], at: usize) -> (usize, usize) {
    let r = rows.iter().rposition(|&(s, _)| s <= at).unwrap_or(0);
    let s = rows[r].0;
    (r, 2 + input[s..at].iter().map(|&c| crust::cell_width(c)).sum::<usize>())
}

/// The place in a row nearest a column. A row that wraps into the next one
/// stops a character short, or the cursor would show on the next row.
fn at_column((s, e): (usize, usize), input: &[char], col: usize) -> usize {
    let last = if e < input.len() && input[e] != '\n' { e - 1 } else { e };
    let (mut i, mut x) = (s, 2);
    while i < last && x + crust::cell_width(input[i]) <= col {
        x += crust::cell_width(input[i]);
        i += 1;
    }
    i
}

/// Text in lines of at most `w` columns, broken between words.
fn word_wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && crust::display_width(&line) + 1 + crust::display_width(word) > w.max(1) {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

/// An answer as drawn at `width`. It is drawn once per width, since the
/// HyperList drawing starts a program.
fn drawn(t: &mut Turn, width: usize) -> String {
    if t.drawn.as_ref().map(|d| d.0) != Some(width) {
        t.drawn = Some((width, render(t.answer.as_deref().unwrap_or(""), width)));
    }
    t.drawn.as_ref().map(|d| d.1.clone()).unwrap_or_default()
}

fn turn_text(t: &mut Turn, width: usize) -> String {
    let prompt: Vec<String> = t.prompt.lines().map(|l| style::fg(&format!("› {l}"), BLUE)).collect();
    let body = match &t.answer {
        None => style::dim("…"),
        Some(a) if a.trim().is_empty() && t.failed => style::fg("✗ stopped", RED),
        Some(_) => drawn(t, width),
    };
    format!("{}\n\n{}", prompt.join("\n"), body)
}

/// Prompts sent while Claude works, in their own colour until they go.
fn waiting_text(waiting: &VecDeque<String>) -> String {
    let lines: Vec<String> = waiting.iter().flat_map(|p| p.lines()).map(|l| style::fg(&format!("› {l}"), WAIT)).collect();
    lines.join("\n")
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

/// Where deck remembers each model's context window between runs.
fn cache_file() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::env::var("XDG_CACHE_HOME").unwrap_or(format!("{home}/.cache"));
    format!("{dir}/deck.json")
}

fn history_file() -> String {
    format!("{}/.claude/history.jsonl", std::env::var("HOME").unwrap_or_default())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// The prompts sent in `project`, oldest first, from the end of Claude
/// Code's history file: Up shows the same ones there and here.
fn load_history(project: &str) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(history_file()) else { return Vec::new() };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(1 << 20))).is_err() || f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    history_in(&String::from_utf8_lossy(&buf), project)
}

fn history_in(text: &str, project: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let Some(d) = v["display"].as_str().filter(|d| !d.trim().is_empty()) else { continue };
        if v["project"].as_str() == Some(project) && out.last().map(String::as_str) != Some(d) {
            out.push(d.to_string());
        }
    }
    let skip = out.len().saturating_sub(200);
    out.split_off(skip)
}

/// What a command printed, from the log `script` keeps: its first and
/// last lines are script's own, a carriage return starts the line over,
/// and colour codes go.
fn typescript(raw: &str) -> String {
    let mut lines: Vec<String> = raw
        .lines()
        .filter(|l| !l.starts_with("Script started on ") && !l.starts_with("Script done on "))
        .map(|l| crust::strip_ansi(l.trim_end_matches('\r').rsplit('\r').next().unwrap_or("")))
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn cache() -> Value {
    fs::read_to_string(cache_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or(json!({}))
}

fn save_cache(v: &Value) {
    let path = cache_file();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&path, v.to_string());
}

/// Claude Code reports a model's context window only at the end of a turn,
/// so the last one seen stands in until then.
fn known_window(model: &str) -> u64 {
    cache()["windows"][model].as_u64().unwrap_or(0)
}

fn remember_window(model: &str, window: u64) {
    let mut v = cache();
    v["windows"][model] = json!(window);
    save_cache(&v);
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

/// An answer as the terminal shows it: the HyperList drawing when the user
/// has it on, else tables drawn with lines; then light markdown.
fn render(md: &str, width: usize) -> String {
    draw(md, width, hyperlist)
}

/// The HyperList hook turns a table into Items itself, so it gets the
/// markdown as written. Lines drawn first came back wrapped as Items.
fn draw(md: &str, width: usize, hl: fn(&str, usize) -> Option<String>) -> String {
    let md = hl(md, width).unwrap_or_else(|| crust::text::format_markdown_tables(md, width));
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

/// Text through the HyperList display hook, when it is installed and on
/// (`/hl`). It wraps at `width` and marks the Items of a wrapped group with
/// "+". Claude Code leaves it to deck (HL_RAW), as twice over it garbles the
/// colour codes of the first pass.
fn hyperlist(md: &str, width: usize) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let hook = format!("{home}/.claude/hooks/hyperlist-display-asm");
    if !std::path::Path::new(&hook).exists() || std::path::Path::new(&format!("{home}/.claude/hyperlist-display.off")).exists() {
        return None;
    }
    let mut child = Command::new(hook).env("HL_WIDTH", width.to_string()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
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
    fn a_waiting_prompt_is_drawn() {
        let text = crust::strip_ansi(&waiting_text(&VecDeque::from(["one\ntwo".to_string(), "three".to_string()])));
        assert_eq!(text, "› one\n› two\n› three");
    }

    #[test]
    fn history_is_the_projects_own_prompts() {
        let text = [
            r#"{"display":"one","project":"/a"}"#,
            r#"{"display":"other","project":"/b"}"#,
            r#"{"display":"two","project":"/a"}"#,
            r#"{"display":"two","project":"/a"}"#,
            r#"{"display":" ","project":"/a"}"#,
            "cut mid-line",
        ].join("\n");
        assert_eq!(history_in(&text, "/a"), vec!["one", "two"]);
    }

    #[test]
    fn a_shell_log_keeps_only_what_the_command_printed() {
        let raw = "Script started on 2026-10-27 [COMMAND=\"x\"]\nhi\r\n50%\r100%\r\n\x1b[01;34mboot\x1b[0m\r\n\nScript done on 2026-10-27 [COMMAND_EXIT_CODE=\"3\"]\n";
        assert_eq!(typescript(raw), "hi\n100%\nboot");
    }

    #[test]
    fn the_hyperlist_hook_gets_the_table_as_written() {
        let md = "Two.\n\n| # | Question |\n|---|---|\n| 1 | Yes? |";
        let seen = |m: &str, _: usize| Some(if m.contains("|---|") { "raw" } else { "drawn" }.to_string());
        assert_eq!(draw(md, 80, seen), "raw");
        assert!(draw(md, 80, |_, _| None).contains('│'));
    }

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
    fn a_question_wraps_between_words() {
        assert_eq!(word_wrap("Should deck ring the bell?", 12), vec!["Should deck", "ring the", "bell?"]);
        assert_eq!(word_wrap("one\ntwo", 12), vec!["one", "two"]);
    }

    #[test]
    fn a_long_prompt_takes_more_rows() {
        let input: Vec<char> = "abcdefghij\nxy".chars().collect();
        // Eight columns: "› " and five characters, one column for the cursor.
        let rows = input_rows(&input, 8);
        assert_eq!(rows, vec![(0, 5), (5, 10), (11, 13)]);
        assert_eq!(cursor_at(&rows, &input, 13), (2, 4));
        assert_eq!(cursor_at(&rows, &input, 10), (1, 7));
        assert_eq!(cursor_at(&rows, &input, 5), (1, 2));
        // Up from the end of "xy" lands under it; from the right edge of a
        // wrapped row it stays on that row.
        assert_eq!(at_column(rows[1], &input, 4), 7);
        assert_eq!(at_column(rows[0], &input, 7), 4);
        assert_eq!(input_rows(&[], 8), vec![(0, 0)]);
    }
}
