//! The Claude Code process behind deck. It runs headless (`claude -p`)
//! and speaks stream-json: one JSON object a line, each way.
//!
//! A thread reads its output and hands each line to the screen as events.
//! The screen waits on the keyboard alone, so the thread wakes it with a
//! SIGWINCH to itself: crust's key reader returns "RESIZE" for it. One
//! signal covers every event sent before the screen drains them, and
//! nothing is checked on a timer while Claude is quiet.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

/// Set by the reader when it signals, cleared by the screen before it
/// drains: an event that lands after the clear signals again.
pub static WAKE: AtomicBool = AtomicBool::new(false);

/// Output of a tool call is kept up to this many bytes, for the detail view.
const KEEP: usize = 20_000;

#[derive(Debug, PartialEq)]
pub enum Event {
    Init { session: String, model: String, version: String },
    /// A tool call, with a few words on what it does.
    Tool { id: String, name: String, what: String, input: Value },
    /// The tool call `id` ended.
    Done { id: String, failed: bool, output: String },
    /// Text Claude wrote on the way, between tool calls.
    Note(String),
    /// A stop hook sent the answer back; its first line of complaint.
    Rewrite(String),
    Stopped,
    /// Claude Code took in this prompt (it echoes each one when it reads it).
    Taken(String),
    /// What other sessions sent, handed over by the fleet bus hook.
    Mail(String),
    /// Context in use after the last call to the model, in tokens.
    Context(u64),
    /// Claude Code asks whether a tool may run.
    Ask { id: String, tool: String, input: Value },
    /// The turn is over. `text` is its final answer: after a stop hook's
    /// rewrite, the rewrite alone.
    Answer { text: String, failed: bool, window: u64 },
    /// Use of the plan's limits, in percent, and when the week's resets.
    Limits { hours: f64, week: f64, week_reset: u64 },
    /// A new status line, drawn by ~/.claude/statusline.sh.
    Status(String),
    /// The process ended.
    Gone,
}

/// The events in one line of Claude Code's output.
pub fn read(v: &Value) -> Vec<Event> {
    let mut out = Vec::new();
    // Work inside a subagent belongs to its Task call, not to the turn.
    if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
        return out;
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match v["type"].as_str().unwrap_or("") {
        "system" if v["subtype"] == "init" => {
            out.push(Event::Init {
                session: s(v, "session_id"),
                model: s(v, "model"),
                version: s(v, "claude_code_version"),
            });
        }
        "system" if v["subtype"] == "hook_response" && v["hook_event"] == "UserPromptSubmit" => {
            let said: Value = serde_json::from_str(v["stdout"].as_str().unwrap_or("")).unwrap_or_default();
            if let Some(m) = said["hookSpecificOutput"]["additionalContext"].as_str().and_then(mail) {
                out.push(Event::Mail(m));
            }
        }
        "assistant" => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("tool_use") => out.push(Event::Tool {
                        id: s(b, "id"),
                        name: s(b, "name"),
                        what: what(&s(b, "name"), &b["input"]),
                        input: b["input"].clone(),
                    }),
                    Some("text") => {
                        let t = s(b, "text");
                        if !t.trim().is_empty() {
                            out.push(Event::Note(t.trim().to_string()));
                        }
                    }
                    _ => {}
                }
            }
            let u = &v["message"]["usage"];
            let used = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                .iter()
                .map(|k| u[k].as_u64().unwrap_or(0))
                .sum::<u64>();
            if used > 0 {
                out.push(Event::Context(used));
            }
        }
        "user" if v["isReplay"] == true => {
            if let Some(t) = v["message"]["content"].as_str() {
                out.push(Event::Taken(t.to_string()));
            }
        }
        "user" => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("tool_result") => out.push(Event::Done {
                        id: s(b, "tool_use_id"),
                        failed: b["is_error"].as_bool().unwrap_or(false),
                        output: result_text(&b["content"]),
                    }),
                    Some("text") => {
                        let t = s(b, "text");
                        if let Some(m) = mail(&t) {
                            out.push(Event::Mail(m));
                        } else if let Some(rest) = t.strip_prefix("Stop hook feedback:") {
                            let said = rest.lines().map(str::trim).filter(|l| !l.is_empty());
                            // The first line names the check; the second says what it found.
                            let found = said.clone().nth(1).or(said.clone().next()).unwrap_or("");
                            out.push(Event::Rewrite(found.to_string()));
                        } else if t.starts_with("[Request interrupted") {
                            out.push(Event::Stopped);
                        }
                    }
                    _ => {}
                }
            }
        }
        "control_request" if v["request"]["subtype"] == "can_use_tool" => {
            out.push(Event::Ask {
                id: s(v, "request_id"),
                tool: s(&v["request"], "tool_name"),
                input: v["request"]["input"].clone(),
            });
        }
        "rate_limit_event" => {
            let w = &v["rate_limit_info"]["unifiedWindows"];
            let pct = |k: &str| w[k]["utilization"].as_f64().unwrap_or(0.0) * 100.0;
            if w.is_object() {
                out.push(Event::Limits {
                    hours: pct("five_hour"),
                    week: pct("seven_day"),
                    week_reset: w["seven_day"]["resetsAt"].as_u64().unwrap_or(0),
                });
            }
        }
        "result" => {
            let window = v["modelUsage"]
                .as_object()
                .and_then(|m| m.values().filter_map(|u| u["contextWindow"].as_u64()).max())
                .unwrap_or(0);
            out.push(Event::Answer {
                text: s(v, "result"),
                failed: v["is_error"].as_bool().unwrap_or(false) || v["subtype"] != "success",
                window,
            });
        }
        _ => {}
    }
    out
}

/// The first words of the fleet bus hook's text.
pub const MAIL: &str = "Messages delivered via the fleet bus";

/// What other sessions sent, from the fleet bus hook's text without its
/// header. The hook hands it over with a prompt, or at the end of a turn.
pub fn mail(text: &str) -> Option<String> {
    let text = text.strip_prefix("Stop hook feedback:").unwrap_or(text).trim_start();
    let (head, body) = text.split_once('\n')?;
    let body = body.trim_end();
    let body = body.strip_suffix("Deal with these before you stop.").unwrap_or(body).trim();
    (head.starts_with(MAIL) && !body.is_empty()).then(|| body.to_string())
}

/// A tool result's text: a string, or a list of blocks with text in them.
fn result_text(c: &Value) -> String {
    let mut t = match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if t.len() > KEEP {
        let mut cut = KEEP;
        while !t.is_char_boundary(cut) {
            cut -= 1;
        }
        t.truncate(cut);
        t.push_str("\n…");
    }
    t
}

/// A few words on what a tool call does: its description, or the thing it
/// works on.
pub fn what(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    let short = |p: String| match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => p,
    };
    let w = match name {
        "Bash" => {
            let d = s("description");
            if d.is_empty() { s("command") } else { d }
        }
        "Read" | "Edit" | "Write" | "NotebookEdit" => short(s("file_path")),
        "Grep" | "Glob" => s("pattern"),
        "WebFetch" => s("url"),
        "WebSearch" | "ToolSearch" => s("query"),
        "Task" | "Agent" => s("description"),
        "Skill" => s("skill"),
        _ => input
            .as_object()
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default(),
    };
    w.lines().next().unwrap_or("").to_string()
}

pub struct Claude {
    child: Child,
    stdin: Option<ChildStdin>,
    asked: u64,
    /// Held with SIGSTOP while idle; the next thing sent lets it go on.
    paused: bool,
}

impl Claude {
    /// Start Claude Code in `cwd` on session `id`: an old one when
    /// `resume`, else a new one under that id. Its errors go to `errors`;
    /// its events to `tx`.
    pub fn start(
        id: &str,
        resume: bool,
        cwd: &str,
        env: &[(&str, String)],
        errors: File,
        tx: Sender<Event>,
    ) -> io::Result<Claude> {
        // A stand-in for the tests.
        let program = std::env::var("DECK_CLAUDE").unwrap_or_else(|_| "claude".into());
        let mut cmd = Command::new(program);
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"]);
        // An echo of each prompt when Claude reads it, for prompts sent mid-turn.
        cmd.arg("--replay-user-messages");
        // Each hook's output, for the messages the fleet bus hook hands over.
        cmd.arg("--include-hook-events");
        // Permission questions come to deck instead of being refused.
        cmd.args(["--permission-prompts", "host"]);
        cmd.args([if resume { "--resume" } else { "--session-id" }, id]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Its own process group, so the hangup of a closing window does not
        // reach it; the kernel ends it instead, with SIGTERM, when the
        // thread that starts it ends: deck's main thread, so when deck
        // ends, by C-D or by the window closing. Claude Code then stops its
        // commands too, a background job as well.
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = cmd
            .process_group(0)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(errors))
            .spawn()?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                // Every hook run reports twice; only fleet mail is read.
                if line.contains(r#""subtype":"hook_"#) && !line.contains(MAIL) {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let events = read(&v);
                if events.is_empty() {
                    continue;
                }
                for e in events {
                    if tx.send(e).is_err() {
                        return;
                    }
                }
                wake();
            }
            let _ = tx.send(Event::Gone);
            wake();
        });
        Ok(Claude { child, stdin, asked: 0, paused: false })
    }

    /// Hold it while idle: an idle Claude Code still wakes about 25 times
    /// a second, 30 to 70 mW. A paused one costs nothing and stays in
    /// fleet's list. Not while it runs a process of its own (a background
    /// job, a monitor), whose end should wake it. True when paused.
    pub fn pause(&mut self) -> bool {
        if !self.paused && !self.has_children() {
            self.paused = unsafe { libc::kill(self.child.id() as i32, libc::SIGSTOP) } == 0;
        }
        self.paused
    }

    fn go_on(&mut self) {
        if self.paused {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGCONT);
            }
            self.paused = false;
        }
    }

    /// True while it runs a command, in a turn or in the background.
    pub fn has_children(&self) -> bool {
        !self.children().is_empty()
    }

    /// The process ids of the commands it runs.
    fn children(&self) -> Vec<i32> {
        let tasks = std::fs::read_dir(format!("/proc/{}/task", self.child.id()));
        let lists = tasks.into_iter().flatten().flatten();
        let lists = lists.filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok());
        lists.flat_map(|c| c.split_whitespace().filter_map(|p| p.parse().ok()).collect::<Vec<_>>()).collect()
    }

    fn send(&mut self, v: Value) {
        self.go_on();
        if let Some(i) = self.stdin.as_mut() {
            let _ = writeln!(i, "{v}").and_then(|_| i.flush());
        }
    }

    pub fn say(&mut self, text: &str) {
        self.send(json!({"type": "user", "message": {"role": "user", "content": text}}));
    }

    pub fn interrupt(&mut self) {
        self.asked += 1;
        let id = format!("deck-{}", self.asked);
        self.send(json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}}));
    }

    /// Answer a permission question. A refusal carries `why` back to Claude.
    pub fn answer(&mut self, id: &str, input: &Value, allow: bool, why: &str) {
        let response = if allow {
            json!({"behavior": "allow", "updatedInput": input})
        } else {
            json!({"behavior": "deny", "message": why})
        };
        self.send(json!({"type": "control_response",
            "response": {"subtype": "success", "request_id": id, "response": response}}));
    }

    /// Close its input, so it ends on its own; that takes it most of a
    /// second, which deck does not wait for. A turn still running is cut,
    /// and so are its commands, which it would otherwise wait for with
    /// no window to answer in.
    pub fn stop(&mut self, busy: bool) {
        self.go_on();
        self.stdin = None;
        let jobs = self.children();
        if busy || !jobs.is_empty() {
            // Claude Code starts each command in a group of its own.
            for p in jobs {
                unsafe {
                    if libc::kill(-p, libc::SIGTERM) != 0 {
                        libc::kill(p, libc::SIGTERM);
                    }
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Wake the screen: it drains the events waiting for it.
pub fn wake() {
    if !WAKE.swap(true, Ordering::SeqCst) {
        unsafe {
            libc::kill(libc::getpid(), libc::SIGWINCH);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Vec<Event> {
        read(&serde_json::from_str(s).unwrap())
    }

    #[test]
    fn a_tool_call_and_its_result() {
        let call = ev(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash",
            "input":{"command":"echo hi","description":"Say hi"}}],"usage":{"input_tokens":2,"cache_read_input_tokens":98}}}"#);
        assert_eq!(call[0], Event::Tool { id: "t1".into(), name: "Bash".into(), what: "Say hi".into(),
            input: json!({"command": "echo hi", "description": "Say hi"}) });
        assert_eq!(call[1], Event::Context(100));
        let done = ev(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"hi","is_error":false}]}}"#);
        assert_eq!(done, vec![Event::Done { id: "t1".into(), failed: false, output: "hi".into() }]);
    }

    #[test]
    fn a_message_from_another_session_comes_with_a_prompt_or_at_the_end() {
        let head = "Messages delivered via the fleet bus (from other local sessions or the fleet TUI):";
        let said = json!({"hookSpecificOutput": {"additionalContext": format!("{head}\n- deck is yours\n-- fe2o3")}});
        let hook = json!({"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit", "stdout": said.to_string()});
        assert_eq!(read(&hook), vec![Event::Mail("- deck is yours\n-- fe2o3".into())]);
        let end = format!("Stop hook feedback:\n{head}\n- deck is yours\n-- fe2o3\n\nDeal with these before you stop.");
        let stop = json!({"type": "user", "message": {"content": [{"type": "text", "text": end}]}});
        assert_eq!(read(&stop), vec![Event::Mail("- deck is yours\n-- fe2o3".into())]);
        let other = json!({"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit", "stdout": "{}"});
        assert!(read(&other).is_empty());
    }

    #[test]
    fn a_stop_hook_rewrite_shows_as_one_line_and_the_answer_is_the_rewrite() {
        let back = ev(r#"{"type":"user","message":{"content":[{"type":"text","text":"Stop hook feedback:\nstylecheck failed on this turn:\nanswer:1: 31 words (max 22): The small...\nRewrite ONLY the flagged parts"}]}}"#);
        assert_eq!(back, vec![Event::Rewrite("answer:1: 31 words (max 22): The small...".into())]);
        let end = ev(r#"{"type":"result","subtype":"success","is_error":false,"result":"Short now.",
            "modelUsage":{"claude-opus-5-5":{"contextWindow":1000000}}}"#);
        assert_eq!(end, vec![Event::Answer { text: "Short now.".into(), failed: false, window: 1_000_000 }]);
    }

    #[test]
    fn an_interrupt_ends_the_turn_as_failed() {
        assert_eq!(ev(r#"{"type":"user","isReplay":true,"message":{"role":"user","content":"Also this."}}"#),
            vec![Event::Taken("Also this.".into())]);
        assert_eq!(ev(r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#),
            vec![Event::Stopped]);
        let end = ev(r#"{"type":"result","subtype":"error_during_execution","is_error":false}"#);
        assert_eq!(end, vec![Event::Answer { text: String::new(), failed: true, window: 0 }]);
    }

    #[test]
    fn a_subagents_own_steps_stay_out_of_the_turn() {
        assert!(ev(r#"{"type":"assistant","parent_tool_use_id":"t9","message":{"content":[{"type":"text","text":"inside"}]}}"#).is_empty());
    }

    #[test]
    fn a_permission_question() {
        let ask = ev(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm x"}}}"#);
        assert_eq!(ask, vec![Event::Ask { id: "r1".into(), tool: "Bash".into(), input: json!({"command": "rm x"}) }]);
    }

    #[test]
    fn an_idle_claude_is_paused_unless_it_runs_a_job_of_its_own() {
        // A signal lands a moment after it is sent.
        let state = |c: &Claude| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let st = std::fs::read_to_string(format!("/proc/{}/stat", c.child.id())).unwrap();
            st.rsplit(") ").next().unwrap().chars().next().unwrap()
        };
        let dir = std::env::temp_dir().join(format!("deck-pause-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let start = |script: &str| {
            let path = dir.join("claude");
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            std::env::set_var("DECK_CLAUDE", &path);
            let errors = File::create(dir.join("err")).unwrap();
            let c = Claude::start("x", false, "/", &[], errors, std::sync::mpsc::channel().0).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            c
        };
        let mut idle = start("exec sleep 30");
        assert!(idle.pause());
        assert_eq!(state(&idle), 'T');
        idle.say("hi");
        assert_ne!(state(&idle), 'T');
        idle.stop(true);
        // One command in a group of its own, as Claude Code starts them,
        // and one in its group.
        let mut working = start("setsid sleep 30 & sleep 30 & exec sleep 30");
        assert!(!working.pause());
        assert_ne!(state(&working), 'T');
        let jobs = working.children();
        assert_eq!(jobs.len(), 2);
        working.stop(false);
        std::thread::sleep(std::time::Duration::from_millis(100));
        for p in jobs {
            let st = std::fs::read_to_string(format!("/proc/{p}/stat")).unwrap_or_default();
            assert!(st.is_empty() || st.contains(") Z"), "{p} still runs: {st}");
        }
        // Ended with the thread that started it, as when deck's window closes.
        let mut orphan = std::thread::scope(|s| s.spawn(|| start("exec sleep 30")).join().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(state(&orphan), 'Z');
        orphan.stop(true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tool_is_named_by_what_it_works_on() {
        std::env::set_var("HOME", "/home/u");
        assert_eq!(what("Read", &json!({"file_path": "/home/u/a.rs"})), "~/a.rs");
        assert_eq!(what("Bash", &json!({"command": "ls\nwc"})), "ls");
        assert_eq!(what("Grep", &json!({"pattern": "fn main"})), "fn main");
    }
}
