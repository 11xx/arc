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
    #[serde(default)]
    truncation: Option<TapesTruncation>,
}

#[derive(Deserialize)]
struct TapesTurn {
    role: String,
    text: String,
    ts: Option<String>,
}

#[derive(Deserialize)]
struct TapesTruncation {
    #[serde(default)]
    source: Vec<TapesBound>,
}

/// One bound `tapes` reports its read rested on. `Unknown` keeps a bound kind
/// a newer tapes adds from discarding an otherwise readable answer.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TapesBound {
    /// Only the final `bytes` of the recording file were read.
    FileTail { bytes: u64 },
    /// The store read stopped at the newest `records` records of one kind.
    RecordPage { records: usize, of: String },
    /// These `turns` carry text the source stored cut at `chars` characters.
    TurnText { turns: usize, chars: usize },
    /// A bound kind this reader does not know.
    #[serde(other)]
    Unknown,
}

impl TapesBound {
    /// Whether the bound withheld turns rather than shortening text inside a
    /// returned turn. A withheld turn is text outside the read; a text cut is
    /// text inside it.
    pub fn withholds_turns(&self) -> bool {
        !matches!(self, TapesBound::TurnText { .. })
    }
}

/// How the `tapes` reader answered for one session.
pub enum TapesTurns {
    /// The reader returned what its window reached, with the bound its own
    /// source reported, if any.
    Answered {
        turns: Vec<Turn>,
        bound: Option<TapesBound>,
    },
    /// The `tapes` binary is not installed.
    Absent,
    /// `tapes` ran and refused, or wrote output this reader cannot parse.
    Declined,
}

pub fn transcript_path(harness: &str, session: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    match harness {
        "claude" => claude_transcript_path(&home, session),
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

/// The directory Claude Code keeps one subdirectory per project under,
/// honouring the override its own tooling documents: `CLAUDE_CONFIG_DIR`
/// relocates the whole configuration directory, session history included.
fn claude_projects(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
        .join("projects")
}

/// A session id can be recorded under more than one project directory, so the
/// answer follows a stated rule rather than directory enumeration order: the
/// most recently modified recording wins, and equal timestamps fall back to
/// path order. A recording whose timestamp cannot be read never wins.
fn claude_transcript_path(home: &Path, session: &str) -> Option<PathBuf> {
    let mut recordings: Vec<PathBuf> = std::fs::read_dir(claude_projects(home))
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(format!("{session}.jsonl")))
        .filter(|path| path.is_file())
        .collect();
    recordings.sort_by(|left, right| {
        modified(right)
            .cmp(&modified(left))
            .then_with(|| left.cmp(right))
    });
    recordings.into_iter().next()
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// A session's exchange turns as the `tapes` CLI reports them. Both transcript
/// readers rest on the same bound — the newest `TRANSCRIPT_BYTES` of the
/// recording — so an answer does not depend on which reader is installed.
/// `--tail` is pinned to the widest window so tapes' own turn window cannot
/// clip inside that byte window, and `--read-bytes` pins the byte window to
/// the one the native reader uses. The role filter and `--tail` projection
/// then apply once, locally, for both readers: tapes' own kind classification
/// varies across its releases, while the roles it reports do not.
pub fn tapes_turns(session: &str) -> TapesTurns {
    let output = match Command::new("tapes")
        .args([
            "show",
            session,
            "--tail",
            &usize::MAX.to_string(),
            "--read-bytes",
            &TRANSCRIPT_BYTES.to_string(),
            "--json",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return TapesTurns::Absent,
        Err(_) => return TapesTurns::Declined,
    };
    if !output.status.success() {
        return TapesTurns::Declined;
    }
    let Ok(response) = serde_json::from_slice::<TapesResponse>(&output.stdout) else {
        return TapesTurns::Declined;
    };
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
    let bound = response
        .truncation
        .and_then(|truncation| truncation.source.into_iter().next());
    TapesTurns::Answered { turns, bound }
}

/// Whether arc has its own reader for a harness's recording files.
pub fn native_supported(harness: &str) -> bool {
    matches!(harness, "claude" | "codex" | "pi")
}

/// The operator's view of a transcript: what they asked, plus where the
/// assistant got to. Shared by both readers, so `--tail` counts the same thing
/// whichever one answered.
pub fn operator_view(turns: Vec<Turn>, limit: usize) -> Vec<Turn> {
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

/// The operator turns one recording file holds, with the window they were
/// read from.
pub struct OperatorTurns {
    pub turns: Vec<Turn>,
    /// Bytes the read inspected from the end of the recording.
    pub window_bytes: u64,
    /// Bytes of the recording before the window; zero when it fit whole.
    pub skipped_bytes: u64,
}

pub fn operator_turns(path: &Path) -> Result<OperatorTurns> {
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
    Ok(OperatorTurns {
        turns,
        window_bytes: len - start,
        skipped_bytes: start,
    })
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
