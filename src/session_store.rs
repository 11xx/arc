use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

const TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
    pub ts: Option<String>,
}

#[derive(Deserialize)]
struct TapesResponse {
    turns: Vec<TapesTurn>,
}

#[derive(Deserialize)]
struct TapesTurn {
    role: String,
    text: String,
    ts: Option<String>,
}

pub fn transcript_path(harness: &str, session: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    match harness {
        "claude" => std::fs::read_dir(home.join(".claude/projects"))
            .ok()?
            .flatten()
            .map(|entry| entry.path().join(format!("{session}.jsonl")))
            .find(|path| path.is_file()),
        "codex" => {
            let root = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            find_session_file(&root.join("sessions"), session, 0)
        }
        "pi" => {
            let root = std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os("PI_CODING_AGENT_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(".pi/agent"))
                        .join("sessions")
                });
            find_session_file(&root, session, 0)
        }
        _ => None,
    }
}

/// A session's turns as `tapes` reports them, or `None` when tapes cannot
/// answer — absent binary, non-zero exit, unparseable output. Every one of
/// those is a fall-back signal rather than an error: this is the preferred
/// path, not the only one.
pub fn tapes_turns(session: &str, limit: usize) -> Option<Vec<Turn>> {
    if limit == 0 {
        return Some(Vec::new());
    }
    let output = Command::new("tapes")
        .args(["show", session, "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let response = serde_json::from_slice::<TapesResponse>(&output.stdout).ok()?;
    // No `--tail`: tapes' own default window is the analogue of the byte
    // window the file reader takes, and the curation below is what decides how
    // many turns come back. Bounding tapes first would make `--tail` mean the
    // last N turns of any role here and the last N operator turns there.
    let turns = response
        .turns
        .into_iter()
        .filter(|turn| matches!(turn.role.as_str(), "user" | "assistant"))
        .map(|turn| Turn {
            role: turn.role,
            text: turn.text,
            ts: turn.ts,
        })
        .collect();
    Some(operator_view(turns, limit))
}

/// The operator's view of a transcript: what they asked, plus where the
/// assistant got to. Shared by both readers, so `--tail` counts the same thing
/// whichever one answered.
fn operator_view(turns: Vec<Turn>, limit: usize) -> Vec<Turn> {
    let mut users = Vec::new();
    let mut last_message = None;
    for turn in turns {
        if turn.role == "user" {
            users.push(turn.clone());
        }
        last_message = Some(turn);
    }
    if let Some(turn) = last_message.filter(|turn| turn.role == "assistant") {
        users.push(turn);
    }
    let keep_from = users.len().saturating_sub(limit);
    users.drain(keep_from..).collect()
}

pub fn opencode_databases() -> Option<[PathBuf; 2]> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let root = data_home.join("opencode");
    Some([root.join("opencode.db"), root.join("opencode-next.db")])
}

pub fn operator_turns(path: &Path, limit: usize) -> Result<Vec<Turn>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TRANSCRIPT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.take(TRANSCRIPT_BYTES).read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }

    let text = String::from_utf8_lossy(&bytes);
    let turns = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| parse_turn(&value))
        .collect();
    Ok(operator_view(turns, limit))
}

fn parse_turn(value: &serde_json::Value) -> Option<Turn> {
    let message = if value["message"]["role"].is_string() {
        &value["message"]
    } else if value["payload"]["type"] == "message" && value["payload"]["role"].is_string() {
        &value["payload"]
    } else {
        return None;
    };
    let role = message["role"].as_str()?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let text = content_text(&message["content"]);
    if text.is_empty() {
        return None;
    }
    let ts = value["timestamp"]
        .as_str()
        .or_else(|| message["timestamp"].as_str())
        .map(str::to_string);
    Some(Turn {
        role: role.to_string(),
        text,
        ts,
    })
}

fn content_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_session_file(dir: &Path, session: &str, depth: u8) -> Option<PathBuf> {
    if depth > 4 || !dir.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_session_file(&path, session, depth + 1) {
                return Some(found);
            }
        } else if entry.file_name().to_string_lossy().contains(session)
            && path.extension().is_some_and(|ext| ext == "jsonl")
        {
            return Some(path);
        }
    }
    None
}
