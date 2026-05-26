use crate::session::SessionStatus;

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

/// Check if a process (or its children) is a Codex CLI process.
/// Returns the (pid, session_id) if found.
pub fn find_codex_session(pid: i32) -> Option<(i32, String)> {
    if let Some(uuid) = session_id_from_lsof(pid) {
        return Some((pid, uuid));
    }
    let output = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(child_pid) = line.trim().parse::<i32>().ok() {
            if let Some(uuid) = session_id_from_lsof(child_pid) {
                return Some((child_pid, uuid));
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
    pub tokens_used: u64,
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
        "SELECT model, reasoning_effort, tokens_used, cwd, \
         git_branch, updated_at, created_at, rollout_path \
         FROM threads WHERE id = ?1"
    ).ok()?;

    stmt.query_row(rusqlite::params![session_id], |row| {
        Ok(CodexSessionMeta {
            model: row.get(0).ok(),
            effort: row.get(1).ok(),
            tokens_used: row.get::<_, i64>(2).unwrap_or(0) as u64,
            cwd: row.get(3).ok(),
            git_branch: row.get(4).ok(),
            updated_at: row.get::<_, i64>(5).unwrap_or(0) as u64,
            created_at: row.get::<_, i64>(6).unwrap_or(0) as u64,
            rollout_path: row.get(7).ok(),
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

    let mut lines_checked = 0;
    for line in content.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if input_patterns.iter().any(|p| trimmed.contains(p)) {
            return SessionStatus::Input;
        }

        // Prompt line: session is idle, waiting for next user input
        if trimmed.starts_with('\u{203A}') {
            return SessionStatus::Idle;
        }

        // "Worked for Xs" separator indicates a completed turn
        if trimmed.contains("Worked for") {
            return SessionStatus::Idle;
        }

        lines_checked += 1;
        if lines_checked >= 10 {
            break;
        }
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
