use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use crate::session::SessionStatus;

// Cache lsof results: pane PID -> (codex_pid, session_uuid).
// lsof is expensive (~200-500ms per call on macOS) and we call it recursively
// up to 3 levels deep. Caching avoids re-running it every 2s refresh cycle.
// Entries are evicted when their pane PID disappears from tmux.
static CODEX_PID_CACHE: Mutex<Option<HashMap<i32, (i32, String)>>> = Mutex::new(None);

/// Look up a cached Codex session for a pane PID, or run lsof to discover it.
pub fn find_codex_session_cached(pane_pid: i32) -> Option<(i32, String)> {
    {
        let cache = CODEX_PID_CACHE.lock().unwrap();
        if let Some(map) = cache.as_ref() {
            if let Some(entry) = map.get(&pane_pid) {
                return Some(entry.clone());
            }
        }
    }
    // Cache miss - run the expensive lsof lookup
    let result = find_codex_session(pane_pid)?;
    {
        let mut cache = CODEX_PID_CACHE.lock().unwrap();
        if cache.is_none() {
            *cache = Some(HashMap::new());
        }
        cache.as_mut().unwrap().insert(pane_pid, result.clone());
    }
    Some(result)
}

/// Remove cached entries for pane PIDs that are no longer in tmux.
pub fn evict_stale_codex_cache(live_pane_pids: &[i32]) {
    let mut cache = CODEX_PID_CACHE.lock().unwrap();
    if let Some(map) = cache.as_mut() {
        map.retain(|pid, _| live_pane_pids.contains(pid));
    }
}

/// Extract the Codex session UUID from a running process by checking its open files.
/// Looks for an open rollout JSONL file in ~/.codex/sessions/.
pub fn session_id_from_lsof(pid: i32) -> Option<String> {
    let output = std::process::Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(uuid) = extract_rollout_uuid(line) {
            return Some(uuid);
        }
    }
    None
}

/// Extract session UUID from a path containing a rollout JSONL filename.
/// Filename format: rollout-{ISO_TIMESTAMP}-{UUID}.jsonl
/// UUID is the last 36 chars before .jsonl (8-4-4-4-12 format).
fn extract_rollout_uuid(line: &str) -> Option<String> {
    let codex_sessions = ".codex/sessions/";
    let pos = line.find(codex_sessions)?;
    let after = &line[pos..];
    let rollout_pos = after.find("rollout-")?;
    let filename_start = &after[rollout_pos..];
    let jsonl_pos = filename_start.find(".jsonl")?;
    let stem = &filename_start[..jsonl_pos];
    if stem.len() < 36 {
        return None;
    }
    let uuid = &stem[stem.len() - 36..];
    if is_uuid(uuid) {
        Some(uuid.to_string())
    } else {
        None
    }
}

fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8, 4, 4, 4, 12];
    parts.iter().zip(expected_lens.iter()).all(|(part, &len)| {
        part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Check if a process (or its descendants) is a Codex CLI process.
/// Returns the (pid, session_id) if found.
/// Searches up to 3 levels deep: shell -> node wrapper -> native codex binary.
pub fn find_codex_session(pid: i32) -> Option<(i32, String)> {
    find_codex_session_recursive(pid, 3)
}

fn find_codex_session_recursive(pid: i32, depth: u8) -> Option<(i32, String)> {
    if depth == 0 {
        return None;
    }
    if let Some(uuid) = session_id_from_lsof(pid) {
        return Some((pid, uuid));
    }
    let output = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(child_pid) = line.trim().parse::<i32>().ok() {
            if let Some(result) = find_codex_session_recursive(child_pid, depth - 1) {
                return Some(result);
            }
        }
    }
    None
}

/// Metadata for a Codex session, read from state_5.sqlite.
#[derive(Debug)]
pub struct CodexSessionMeta {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub updated_at: u64,
    pub created_at: u64,
    pub rollout_path: Option<String>,
}

/// Query Codex session metadata from ~/.codex/state_5.sqlite.
/// Returns None if the database is missing, locked, or the session is not found.
pub fn query_session_meta(session_id: &str) -> Option<CodexSessionMeta> {
    let db_path = dirs::home_dir()?.join(".codex").join("state_5.sqlite");
    if !db_path.exists() {
        return None;
    }

    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(&db_path, flags).ok()?;
    conn.pragma_update(None, "journal_mode", "wal").ok();

    let mut stmt = conn.prepare(
        "SELECT model, reasoning_effort, cwd, \
         git_branch, updated_at, created_at, rollout_path \
         FROM threads WHERE id = ?1"
    ).ok()?;

    stmt.query_row(rusqlite::params![session_id], |row| {
        Ok(CodexSessionMeta {
            model: row.get(0).ok(),
            effort: row.get(1).ok(),
            cwd: row.get(2).ok(),
            git_branch: row.get(3).ok(),
            updated_at: row.get::<_, i64>(4).unwrap_or(0) as u64,
            created_at: row.get::<_, i64>(5).unwrap_or(0) as u64,
            rollout_path: row.get(6).ok(),
        })
    }).ok()
}

/// Convert a Unix epoch (seconds) to an ISO 8601 string.
pub fn epoch_to_iso(epoch: u64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .map(|dt| dt.to_rfc3339())
}

/// Find the CWD for a Codex session from SQLite.
pub fn find_codex_session_cwd(session_id: &str) -> Option<String> {
    query_session_meta(session_id).and_then(|m| m.cwd)
}

/// Per-turn token info extracted from the rollout JSONL's last token_count event.
#[derive(Debug)]
pub struct CodexTokenInfo {
    pub last_input_tokens: u64,
    pub context_window: u64,
}

/// Read the tail of a Codex rollout JSONL to find the last token_count event.
/// Returns the current turn's input tokens and the model's context window.
pub fn read_rollout_tokens(rollout_path: &Path) -> Option<CodexTokenInfo> {
    let file = std::fs::File::open(rollout_path).ok()?;
    let file_size = file.metadata().ok()?.len();
    if file_size == 0 {
        return None;
    }

    // Read last 100KB - enough to contain multiple token_count events
    let offset = file_size.saturating_sub(100_000);
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset)).ok()?;

    // Skip partial first line if we seeked into the middle
    if offset > 0 {
        let mut discard = String::new();
        reader.read_line(&mut discard).ok()?;
    }

    let mut last_input = None;
    let mut last_window = None;

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        if !line.contains("token_count") {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
            let payload = &obj["payload"];
            if payload.get("type").and_then(|t| t.as_str()) == Some("token_count") {
                let info = &payload["info"];
                if let Some(v) = info["last_token_usage"]["input_tokens"].as_u64() {
                    last_input = Some(v);
                }
                if let Some(v) = info["model_context_window"].as_u64() {
                    last_window = Some(v);
                }
            }
        }
    }

    Some(CodexTokenInfo {
        last_input_tokens: last_input.unwrap_or(0),
        context_window: last_window.unwrap_or(0),
    })
}

/// Determine session status by inspecting a Codex TUI pane.
pub fn codex_pane_status(pane_target: &str) -> SessionStatus {
    let output = match std::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_target, "-p"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return SessionStatus::Idle,
    };

    let content = String::from_utf8_lossy(&output.stdout);

    let input_patterns = [
        "Allow Codex to run",
        "Codex wants to edit",
        "Action Required",
        "E X E C",
        "P E R M I S S I O N S",
        "D I F F",
        "P A T C H",
        "E L I C I T A T I O N",
    ];

    // Collect last 15 non-empty lines for analysis.
    // The Codex TUI layout (bottom-up): status bar, then either the prompt
    // line (idle) or active streaming content (working). The user's `›` input
    // line can sit above streaming output, so we can't return Idle on first
    // `›` sight - we need to check what's below it.
    let tail: Vec<&str> = content
        .lines()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(15)
        .collect();

    // Working: "esc to interrupt" appears while Codex is actively processing
    if tail.iter().any(|l| l.contains("esc to interrupt")) {
        return SessionStatus::Working;
    }

    // Input: approval/permission prompts
    if tail.iter().any(|l| input_patterns.iter().any(|p| l.contains(p))) {
        return SessionStatus::Input;
    }

    // Idle: prompt `›` is near the bottom (within first 3 non-empty lines,
    // i.e. just above the status bar). If it's deeper, active content has
    // pushed it up, meaning Codex is working.
    for (i, line) in tail.iter().enumerate() {
        if line.starts_with('\u{203A}') {
            return if i <= 2 { SessionStatus::Idle } else { SessionStatus::Working };
        }
    }

    // "Worked for" separator = turn just completed, idle
    if tail.iter().any(|l| l.contains("Worked for")) {
        return SessionStatus::Idle;
    }

    SessionStatus::Working
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_uuid_from_rollout_path() {
        let line = "codex  1234  user  txt  REG  /Users/test/.codex/sessions/2026/05/26/rollout-2026-05-26T09-05-13-019e631a-104b-7f73-b26b-d6ea1a6efcd1.jsonl";
        let uuid = extract_rollout_uuid(line);
        assert_eq!(uuid, Some("019e631a-104b-7f73-b26b-d6ea1a6efcd1".to_string()));
    }

    #[test]
    fn extract_uuid_no_match() {
        let line = "codex  1234  user  txt  REG  /Users/test/.claude/sessions/abc.json";
        assert_eq!(extract_rollout_uuid(line), None);
    }

    #[test]
    fn is_uuid_valid() {
        assert!(is_uuid("019e631a-104b-7f73-b26b-d6ea1a6efcd1"));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("019e631a-104b-7f73-b26b-d6ea1a6efcd")); // too short
    }
}
