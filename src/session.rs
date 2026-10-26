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
    let text = fs::read(transcript(id)?).ok()?;
    let head = String::from_utf8_lossy(&text[..text.len().min(200_000)]).into_owned();
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

/// What the end of a transcript tells: the last prompt and its answer,
/// the model, and the context in use.
#[derive(Default, Debug, PartialEq)]
pub struct Last {
    pub prompt: String,
    pub answer: String,
    pub model: String,
    pub context: u64,
}

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
        if !found.prompt.is_empty() || from == 0 {
            return found;
        }
        span *= 4;
    }
}

pub fn last_in<'a>(lines: impl Iterator<Item = &'a str>) -> Last {
    let mut l = Last::default();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["isMeta"].as_bool() == Some(true) || v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let m = &v["message"];
        match v["type"].as_str() {
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
                l.prompt = t.to_string();
                l.answer.clear();
            }
            Some("assistant") => {
                for b in m["content"].as_array().into_iter().flatten() {
                    if let Some(t) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                        l.answer = t.trim().to_string();
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
    fn a_new_id_is_a_version_4_uuid() {
        let id = new_id();
        assert!(looks_like_id(&id));
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_id());
    }

    #[test]
    fn the_last_answer_follows_the_last_real_prompt() {
        let lines = [
            r#"{"type":"user","message":{"content":"first"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"one"}],"usage":{"input_tokens":5,"cache_read_input_tokens":95}}}"#,
            r#"{"type":"user","message":{"content":"second"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"Stop hook feedback: too long"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
        ];
        let l = last_in(lines.into_iter());
        assert_eq!(l, Last { prompt: "second".into(), answer: "two".into(), model: "claude-opus-5-5".into(), context: 100 });
    }
}
