//! Which session deck opens: a new one, or one named by a bookmark tag or
//! a session id. Bookmarks are the ones `cc-sessions` keeps.

use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct Session {
    pub id: String,
    pub cwd: String,
    pub tags: Vec<String>,
    /// False for a new session: Claude Code starts it under `id`.
    pub resume: bool,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn bookmarks() -> Value {
    fs::read_to_string(home().join(".cc-sessions/bookmarks.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn tags_of(b: &Value, id: &str) -> Vec<String> {
    b["sessions"][id]["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn transcript(id: &str) -> Option<PathBuf> {
    fs::read_dir(home().join(".claude/projects"))
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

/// The directory a session started in: the first `cwd` in its transcript.
fn first_cwd(id: &str) -> Option<String> {
    let mut text = Vec::new();
    fs::File::open(transcript(id)?).ok()?.take(200_000).read_to_end(&mut text).ok()?;
    let head = String::from_utf8_lossy(&text).into_owned();
    head.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v["cwd"].as_str().map(str::to_string))
}

fn looks_like_id(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// A random version 4 UUID, for a new session.
fn new_id() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The session `arg` names, or a new one in the current directory.
pub fn find(arg: Option<&str>) -> Result<Session, String> {
    let here = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let Some(arg) = arg else {
        return Ok(Session { id: new_id(), cwd: here, tags: Vec::new(), resume: false });
    };
    let b = bookmarks();
    let tag = arg.trim_start_matches('#');
    let id = if looks_like_id(arg) {
        arg.to_string()
    } else {
        b["sessions"]
            .as_object()
            .and_then(|m| m.iter().find(|(_, e)| {
                e["tags"].as_array().is_some_and(|a| a.iter().any(|t| t.as_str() == Some(tag)))
            }))
            .map(|(id, _)| id.clone())
            .ok_or(format!("No session has the tag {tag}."))?
    };
    if transcript(&id).is_none() {
        return Err(format!("No transcript for session {id}."));
    }
    let path = b["sessions"][&id]["path"].as_str().unwrap_or("").to_string();
    let cwd = if !path.is_empty() && PathBuf::from(&path).is_dir() {
        path
    } else {
        first_cwd(&id).filter(|d| PathBuf::from(d).is_dir()).unwrap_or(here)
    };
    Ok(Session { tags: tags_of(&b, &id), id, cwd, resume: true })
}

/// The background fleet gives the session, from its `session <tag> <ws>
/// <bg>` lines in ~/.fleetrc.
pub fn colour(tags: &[String]) -> Option<String> {
    let rc = fs::read_to_string(home().join(".fleetrc")).ok()?;
    colour_in(&rc, tags)
}

fn colour_in(rc: &str, tags: &[String]) -> Option<String> {
    rc.lines().find_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
        ["session", tag, _, bg, ..] if tags.iter().any(|t| t == tag) => Some(bg.to_string()),
        _ => None,
    })
}

/// Keep `hex` as the session's background in ~/.fleetrc, where fleet's b
/// puts it too. False when the session has no tag to keep it under.
pub fn set_colour(tags: &[String], hex: &str) -> bool {
    if tags.is_empty() {
        return false;
    }
    let path = home().join(".fleetrc");
    let rc = fs::read_to_string(&path).unwrap_or_default();
    let tmp = path.with_extension("deck");
    fs::write(&tmp, with_colour(&rc, tags, hex)).is_ok() && fs::rename(&tmp, &path).is_ok()
}

/// The rc with the colour on the first `session` line of the session's
/// tags, the line `colour_in` reads, or on a new one.
fn with_colour(rc: &str, tags: &[String], hex: &str) -> String {
    let mut lines: Vec<String> = rc.lines().map(str::to_string).collect();
    let mine = |l: &String| match l.split_whitespace().collect::<Vec<_>>()[..] {
        ["session", tag, ..] => tags.iter().any(|t| t == tag),
        _ => false,
    };
    match lines.iter().position(mine) {
        Some(i) => {
            let f: Vec<&str> = lines[i].split_whitespace().collect();
            lines[i] = format!("session {} {} {hex}", f[1], f.get(2).unwrap_or(&"-"));
        }
        None => lines.push(format!("session {} - {hex}", tags[0])),
    }
    lines.join("\n") + "\n"
}

/// The pid of a Claude Code process already running `id`. Sessions
/// started with `c <tag>` carry CC_SESSION_ID in their environment.
pub fn open_elsewhere(id: &str) -> Option<u32> {
    let want = format!("CC_SESSION_ID={id}");
    for e in fs::read_dir("/proc").ok()?.filter_map(|e| e.ok()) {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        if fs::read_to_string(e.path().join("comm")).map(|c| c.trim() != "claude").unwrap_or(true) {
            continue;
        }
        let Ok(env) = fs::read(e.path().join("environ")) else { continue };
        if env.split(|&c| c == 0).any(|v| v == want.as_bytes()) {
            return Some(pid);
        }
    }
    None
}

/// What the end of a transcript tells: the last prompts, each with its
/// answer, the model, and the context in use.
#[derive(Default, Debug, PartialEq)]
pub struct Last {
    pub turns: Vec<(String, String)>,
    pub model: String,
    pub context: u64,
}

/// How many past turns a reopened session shows.
const TURNS: usize = 10;
/// Ten turns lie 1 to 30 MB from the end of the transcripts measured.
const MOST: u64 = 32 << 20;

/// Read only the end of the transcript: they reach hundreds of megabytes.
pub fn last(id: &str) -> Last {
    let Some(path) = transcript(id) else { return Last::default() };
    let Ok(mut f) = fs::File::open(path) else { return Last::default() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut span = 2 << 20;
    loop {
        let from = len.saturating_sub(span);
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(from)).is_err() || f.read_to_end(&mut buf).is_err() {
            return Last::default();
        }
        let text = String::from_utf8_lossy(&buf);
        // The first line is cut in two unless the read began at the start.
        let lines = text.lines().skip(usize::from(from > 0));
        let found = last_in(lines);
        if found.turns.len() >= TURNS || from == 0 || (span >= MOST && !found.turns.is_empty()) {
            return found;
        }
        span *= 4;
    }
}

pub fn last_in<'a>(lines: impl Iterator<Item = &'a str>) -> Last {
    let mut l = Last::default();
    for line in lines {
        // Most of a transcript is tool output, attachments and request
        // records, never a prompt or an answer: skip it before the parse.
        let has = |k: &str| line.contains(k);
        let prompt = has(r#""type":"user""#) && !has(r#""type":"tool_result""#);
        let answer = has(r#""type":"assistant""#) && has(r#""type":"text""#);
        let queued = has(r#""type":"queued_command""#);
        if !(prompt || answer || queued) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        // A compaction writes its summary as a user line; it is no prompt.
        if ["isMeta", "isSidechain", "isCompactSummary"].iter().any(|k| v[k].as_bool() == Some(true)) {
            continue;
        }
        let m = &v["message"];
        match v["type"].as_str() {
            // A prompt Claude read in the middle of a turn joins that turn.
            Some("attachment") => {
                let p = v["attachment"]["prompt"].as_str().filter(|_| v["attachment"]["type"] == "queued_command");
                if let (Some(p), Some(turn)) = (p, l.turns.last_mut()) {
                    turn.0 = format!("{}\n{}", turn.0, p.trim());
                }
            }
            Some("user") => {
                let text = match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) if !a.iter().any(|b| b["type"] == "tool_result") => a
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => continue,
                };
                let t = text.trim();
                if t.is_empty() || t.starts_with('<') || t.starts_with('[') || t.starts_with("Stop hook feedback") {
                    continue;
                }
                l.turns.push((t.to_string(), String::new()));
            }
            Some("assistant") => {
                for b in m["content"].as_array().into_iter().flatten() {
                    // Claude Code writes that line for a turn cut short.
                    let said = b["text"].as_str().filter(|t| !t.trim().is_empty() && t.trim() != "No response requested.");
                    if let (Some(t), Some(turn)) = (said, l.turns.last_mut()) {
                        turn.1 = t.trim().to_string();
                    }
                }
                if let Some(model) = m["model"].as_str().filter(|s| s.starts_with("claude")) {
                    l.model = model.to_string();
                }
                let u = &m["usage"];
                let used: u64 = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                    .iter()
                    .map(|k| u[k].as_u64().unwrap_or(0))
                    .sum();
                if used > 0 {
                    l.context = used;
                }
            }
            _ => {}
        }
    }
    let cut = l.turns.len().saturating_sub(TURNS);
    l.turns.drain(..cut);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_colour_comes_from_the_sessions_tag() {
        let rc = "# fleet\nsession asm 2 00101a\nsession pf 5 280028\nsession sOS -\n";
        assert_eq!(colour_in(rc, &["pf".into()]).as_deref(), Some("280028"));
        assert_eq!(colour_in(rc, &["sOS".into()]), None);
        assert_eq!(colour_in(rc, &[]), None);
    }

    #[test]
    fn a_picked_colour_goes_on_the_line_that_is_read() {
        let rc = "session asm 2 00101a\nsession pf 5 280028\nsession sOS -\n";
        let set = |tags: &[&str], hex| with_colour(rc, &tags.iter().map(|t| t.to_string()).collect::<Vec<_>>(), hex);
        assert_eq!(set(&["pf"], "112233"), "session asm 2 00101a\nsession pf 5 112233\nsession sOS -\n");
        assert_eq!(set(&["sOS"], "112233"), "session asm 2 00101a\nsession pf 5 280028\nsession sOS - 112233\n");
        assert_eq!(set(&["new"], "112233"), format!("{rc}session new - 112233\n"));
        let after = set(&["x", "pf"], "445566");
        assert_eq!(colour_in(&after, &["x".into(), "pf".into()]).as_deref(), Some("445566"));
    }

    #[test]
    fn a_new_id_is_a_version_4_uuid() {
        let id = new_id();
        assert!(looks_like_id(&id));
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_id());
    }

    #[test]
    fn a_reopened_session_keeps_the_last_ten_turns() {
        let lines: Vec<String> = (0..12)
            .flat_map(|i| {
                [
                    format!(r#"{{"type":"user","message":{{"content":"p{i}"}}}}"#),
                    format!(r#"{{"type":"attachment","message":{{"content":"a{i}"}}}}"#),
                    format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"a{i}"}}]}}}}"#),
                ]
            })
            .collect();
        let l = last_in(lines.iter().map(String::as_str));
        assert_eq!(l.turns.len(), 10);
        assert_eq!(l.turns[0], ("p2".into(), "a2".into()));
        assert_eq!(l.turns[9], ("p11".into(), "a11".into()));
    }

    #[test]
    fn the_last_answer_follows_the_last_real_prompt() {
        let lines = [
            r#"{"type":"user","message":{"content":"first"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"one"}],"usage":{"input_tokens":5,"cache_read_input_tokens":95}}}"#,
            r#"{"type":"user","message":{"content":"second"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"attachment","attachment":{"type":"queued_command","prompt":"and this"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"Stop hook feedback: too long"}]}}"#,
            r#"{"type":"system","subtype":"compact_boundary"}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
            r#"{"type":"user","message":{"content":"cut short"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"No response requested."}]}}"#,
        ];
        let l = last_in(lines.into_iter());
        let turns = vec![("first".into(), "one".into()), ("second\nand this".into(), "two".into()), ("cut short".into(), String::new())];
        assert_eq!(l, Last { turns, model: "claude-opus-5-5".into(), context: 100 });
    }
}
