# Codex CLI Integration - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Codex CLI session monitoring to recon with full parity - detection, names, CWD, status, create, resume, park/unpark, and history.

**Architecture:** New `codex.rs` module handles all Codex-specific logic (lsof PID lookup, SQLite metadata, pane status). Shared types (`AgentKind` enum, `Session` struct) live in `session.rs`. Existing Claude Code logic stays untouched except for adding `agent` fields and dispatching.

**Tech Stack:** Rust, rusqlite (bundled SQLite), ratatui, tmux, lsof

**Spec:** `docs/superpowers/specs/2026-05-26-codex-integration-design.md`

---

### Task 1: Add `rusqlite` dependency and `AgentKind` enum

**Files:**
- Modify: `Cargo.toml:6-14`
- Modify: `src/session.rs:72-111`

- [ ] **Step 1: Add rusqlite to Cargo.toml**

```toml
[dependencies]
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "process"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dirs = "6"
clap = { version = "4", features = ["derive"] }
chrono = "0.4"
rusqlite = { version = "0.34", features = ["bundled"] }
```

- [ ] **Step 2: Add `AgentKind` enum and `agent` field to `Session` in `src/session.rs`**

Add after `SessionStatus` impl block (after line 89):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum AgentKind {
    Claude,
    Codex,
}
```

Add `agent` field to `Session` struct (after `tags` field, line 110):

```rust
pub struct Session {
    pub session_id: String,
    pub project_name: String,
    pub branch: Option<String>,
    pub cwd: String,
    pub relative_dir: Option<String>,
    pub tmux_session: Option<String>,
    pub pane_target: Option<String>,
    pub model: Option<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub status: SessionStatus,
    pub pid: Option<i32>,
    pub effort: Option<String>,
    pub last_activity: Option<String>,
    pub started_at: u64,
    pub jsonl_path: PathBuf,
    pub last_file_size: u64,
    pub tags: HashMap<String, String>,
    pub agent: AgentKind,
}
```

- [ ] **Step 3: Fix all `Session { ... }` construction sites to include `agent: AgentKind::Claude`**

There are 4 places in `session.rs` that construct `Session` structs. Add `agent: AgentKind::Claude` to each:

1. Line 283 (main JSONL match path) - add after `tags,`:
```rust
                agent: AgentKind::Claude,
```

2. Line 378 (resumed session path) - add after `tags,`:
```rust
                agent: AgentKind::Claude,
```

3. Line 402 (new session placeholder) - add after `tags,`:
```rust
                agent: AgentKind::Claude,
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation (warnings OK)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/session.rs
git commit -m "Add rusqlite dependency and AgentKind enum"
```

---

### Task 2: Add Codex models to `model.rs`

**Files:**
- Modify: `src/model.rs:1-49`

- [ ] **Step 1: Add Codex model entries to all match arms**

Replace the entire `src/model.rs` with:

```rust
/// Map raw model IDs to human-friendly display names.
pub fn display_name(model_id: &str) -> &str {
    match model_id {
        "claude-opus-4-6" => "Opus 4.6",
        "claude-sonnet-4-6" => "Sonnet 4.6",
        "claude-sonnet-4-5-20250514" => "Sonnet 4.5",
        "claude-haiku-4-5-20251001" => "Haiku 4.5",
        "claude-opus-4-20250514" => "Opus 4",
        "claude-sonnet-4-20250514" => "Sonnet 4",
        "gpt-5.5" => "GPT-5.5",
        "gpt-5.4" => "GPT-5.4",
        "o4-mini" => "o4-mini",
        "o3" => "o3",
        _ => model_id,
    }
}

/// Context window size for a given model ID.
pub fn context_window(model_id: &str) -> u64 {
    match model_id {
        "claude-opus-4-6" | "gpt-5.5" => 1_000_000,
        _ => 200_000,
    }
}

/// Reverse lookup: display name (from /model output) -> model ID.
/// Returns None if the display name is not recognized.
pub fn id_from_display_name(display: &str) -> Option<&'static str> {
    match display {
        "Opus 4.6" | "Opus 4.6 (1M context)" => Some("claude-opus-4-6"),
        "Sonnet 4.6" => Some("claude-sonnet-4-6"),
        "Sonnet 4.5" => Some("claude-sonnet-4-5-20250514"),
        "Haiku 4.5" => Some("claude-haiku-4-5-20251001"),
        "Opus 4" => Some("claude-opus-4-20250514"),
        "Sonnet 4" => Some("claude-sonnet-4-20250514"),
        "GPT-5.5" => Some("gpt-5.5"),
        "GPT-5.4" => Some("gpt-5.4"),
        "o4-mini" => Some("o4-mini"),
        "o3" => Some("o3"),
        _ => None,
    }
}

/// Format model name with optional effort level.
pub fn format_with_effort(model_id: &str, effort: &str) -> String {
    let name = display_name(model_id);
    if effort.is_empty() || effort == "default" {
        name.to_string()
    } else {
        format!("{name} ({effort})")
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 3: Commit**

```bash
git add src/model.rs
git commit -m "Add Codex model entries (GPT-5.5, GPT-5.4, o4-mini, o3)"
```

---

### Task 3: Create `codex.rs` - lsof PID lookup and UUID extraction

**Files:**
- Create: `src/codex.rs`
- Modify: `src/main.rs:1` (add `mod codex;`)

- [ ] **Step 1: Create `src/codex.rs` with lsof session ID extraction**

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use crate::session::SessionStatus;

// Cache lsof results: pane PID -> (codex_pid, session_uuid).
// lsof is expensive (~200-500ms per call on macOS) and we call it recursively
// up to 3 levels deep. Caching avoids re-running it every 2s refresh cycle.
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
    let result = find_codex_session(pane_pid)?;
    {
        let mut cache = CODEX_PID_CACHE.lock().unwrap();
        if cache.is_none() { *cache = Some(HashMap::new()); }
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
    let stem = &filename_start[..jsonl_pos]; // "rollout-2026-05-26T09-05-13-{UUID}"
    // UUID is last 36 chars of the stem
    if stem.len() < 36 {
        return None;
    }
    let uuid = &stem[stem.len() - 36..];
    // Validate UUID format: 8-4-4-4-12 hex with dashes
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
/// The rollout JSONL is only held open by the native binary (grandchild).
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
```

- [ ] **Step 2: Add `mod codex;` to `src/main.rs`**

Add after `mod cli;` (line 2):

```rust
mod codex;
```

- [ ] **Step 3: Run tests**

Run: `cargo test codex 2>&1`
Expected: all 4 tests pass

- [ ] **Step 4: Commit**

```bash
git add src/codex.rs src/main.rs
git commit -m "Add codex.rs with lsof-based session ID extraction"
```

---

### Task 4: Add Codex SQLite metadata query to `codex.rs`

**Files:**
- Modify: `src/codex.rs`

- [ ] **Step 1: Add `CodexSessionMeta` struct and `query_session_meta` function**

Append to `src/codex.rs` (before the `#[cfg(test)]` block):

```rust
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
```

- [ ] **Step 2: Add `use` for rusqlite at the top of `codex.rs`**

No extra import needed - we use `rusqlite::` fully qualified in the function.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 4: Commit**

```bash
git add src/codex.rs
git commit -m "Add Codex SQLite metadata query to codex.rs"
```

---

### Task 5: Add Codex pane status detection to `codex.rs`

**Files:**
- Modify: `src/codex.rs`

- [ ] **Step 1: Add `codex_pane_status` function**

Append to `src/codex.rs` (before the `#[cfg(test)]` block):

**Key insight:** Unlike Claude Code where the prompt disappears during work, the Codex `›` prompt persists in the pane while output streams below it. A naive "first `›` = Idle" approach is wrong - the prompt's *position* relative to the bottom determines state. The Codex TUI layout (bottom-up) is: status bar, then either the prompt (idle) or active streaming content (working).

```rust
use crate::session::SessionStatus;

/// Determine session status by inspecting a Codex TUI pane.
///
/// The Codex TUI layout (bottom-up):
///   - Last line: status bar (model, cwd, context %)
///   - Above that: active content or prompt
///
/// When working, "esc to interrupt" appears near the bottom.
/// When idle, the `›` prompt is the bottommost interactive line
/// (just above the status bar) with no active content below it.
/// When waiting for approval, permission prompt text appears.
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 3: Commit**

```bash
git add src/codex.rs
git commit -m "Add Codex pane status detection to codex.rs"
```

---

### Task 6: Generalize pane discovery and `build_live_session_map` in `session.rs`

**Files:**
- Modify: `src/session.rs:443-490` (LiveSessionInfo, build_live_session_map)
- Modify: `src/session.rs:1159-1239` (discover_claude_tmux_panes, find_claude_child_pid)

This is the core integration task. We generalize tmux pane discovery to detect both Claude and Codex processes, and update `LiveSessionInfo` and `build_live_session_map` accordingly.

**Key insights discovered during implementation:**

1. **Process tree depth:** The Codex process chain is `fish/bash -> node (JS wrapper) -> codex (native binary)`. The rollout JSONL file is only held open by the native binary (grandchild of the shell). `find_codex_session()` must recurse 3 levels deep through the process tree, not just check direct children.

2. **lsof caching:** `lsof` takes 200-500ms per call on macOS. Cache positive results in a static `CODEX_PID_CACHE` keyed by pane PID. Only new PIDs trigger lsof; stale entries are evicted when pane PIDs disappear from tmux.

3. **Detection order matters for performance:** For `node`/`codex` panes, check Codex cache (instant) BEFORE `find_claude_child_pid` (pgrep). Otherwise every `node` pane triggers a ~20ms pgrep call per refresh, causing visible TUI lag with multiple Codex sessions.

4. **Don't add `fish` to `is_shell`:** The user may have 15+ fish shell panes. Each triggers a `pgrep` call for Claude child detection. Claude/Codex never appear as bare `fish` in `pane_current_command` - they show as `node`, version numbers, or `claude`/`codex`.

5. **Version-number panes need the Claude child fallback:** Claude Code shows as e.g. `2.1.150` in `pane_current_command`, but the pane PID is often the shell (no session file). These must still reach `find_claude_child_pid` - don't gate it behind `command == "node"` only.

- [ ] **Step 1: Add `agent` field to `LiveSessionInfo`**

In `src/session.rs`, change the `LiveSessionInfo` struct (line 443-449):

```rust
struct LiveSessionInfo {
    pid: i32,
    tmux_session: String,
    pane_target: String,
    pane_cwd: String,
    started_at: u64,
    agent: AgentKind,
}
```

- [ ] **Step 2: Rename `discover_claude_tmux_panes` to `discover_agent_tmux_panes` and add Codex detection**

Replace the function `discover_claude_tmux_panes` (lines 1159-1225) with:

```rust
struct DiscoveredPane {
    pid: i32,
    tmux_session: String,
    pane_target: String,
    pane_cwd: String,
    agent: AgentKind,
    codex_session_id: Option<String>,
}

/// Get tmux panes running Claude Code or Codex CLI.
fn discover_agent_tmux_panes() -> Vec<DiscoveredPane> {
    let output = match std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_pid}|||#{session_name}|||#{pane_current_command}|||#{pane_current_path}|||#{window_index}|||#{pane_index}",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let sessions_dir = dirs::home_dir()
        .map(|h| h.join(".claude").join("sessions"))
        .unwrap_or_default();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(6, "|||").collect();
        if parts.len() < 6 {
            continue;
        }
        let pid: i32 = match parts[0].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let session_name = parts[1];
        let command = parts[2];
        let pane_path = parts[3];
        let window_index = parts[4];
        let pane_index = parts[5];

        let is_candidate = command
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            || command == "claude"
            || command == "codex"
            || command == "node";

        let is_shell = command == "bash" || command == "sh" || command == "zsh";

        if is_candidate || is_shell {
            let pane_target = format!("{session_name}:{window_index}.{pane_index}");

            // Try Claude first: check PID session file
            if sessions_dir.join(format!("{pid}.json")).exists() {
                results.push(DiscoveredPane {
                    pid,
                    tmux_session: session_name.to_string(),
                    pane_target,
                    pane_cwd: pane_path.to_string(),
                    agent: AgentKind::Claude,
                    codex_session_id: None,
                });
                continue;
            }

            // For node/codex panes: try Codex cache first (instant) before pgrep
            if command == "node" || command == "codex" {
                if let Some((codex_pid, session_id)) = crate::codex::find_codex_session_cached(pid) {
                    results.push(DiscoveredPane {
                        pid: codex_pid,
                        tmux_session: session_name.to_string(),
                        pane_target,
                        pane_cwd: pane_path.to_string(),
                        agent: AgentKind::Codex,
                        codex_session_id: Some(session_id),
                    });
                    continue;
                }
            }

            // All other candidates (version numbers, "claude") and shells:
            // check for Claude child process via pgrep + session file
            if let Some(claude_pid) = find_claude_child_pid(pid) {
                results.push(DiscoveredPane {
                    pid: claude_pid,
                    tmux_session: session_name.to_string(),
                    pane_target,
                    pane_cwd: pane_path.to_string(),
                    agent: AgentKind::Claude,
                    codex_session_id: None,
                });
            }
        }
    }

    results
}
```

- [ ] **Step 3: Update `build_live_session_map` to use `discover_agent_tmux_panes`**

Replace `build_live_session_map` (lines 456-490) with:

```rust
fn build_live_session_map() -> HashMap<String, LiveSessionInfo> {
    let pid_session_map = read_pid_session_map();
    let panes = discover_agent_tmux_panes();

    let mut map = HashMap::new();
    for pane in panes {
        match pane.agent {
            AgentKind::Claude => {
                if let Some(info) = pid_session_map.get(&pane.pid) {
                    map.insert(
                        info.session_id.clone(),
                        LiveSessionInfo {
                            pid: pane.pid,
                            tmux_session: pane.tmux_session,
                            pane_target: pane.pane_target,
                            pane_cwd: pane.pane_cwd,
                            started_at: info.started_at,
                            agent: AgentKind::Claude,
                        },
                    );
                } else {
                    map.insert(
                        format!("tmux-{}", pane.pane_target),
                        LiveSessionInfo {
                            pid: pane.pid,
                            tmux_session: pane.tmux_session,
                            pane_target: pane.pane_target,
                            pane_cwd: pane.pane_cwd,
                            started_at: 0,
                            agent: AgentKind::Claude,
                        },
                    );
                }
            }
            AgentKind::Codex => {
                let session_id = pane.codex_session_id.clone()
                    .unwrap_or_else(|| format!("codex-tmux-{}", pane.pane_target));
                let started_at = pane.codex_session_id.as_ref()
                    .and_then(|id| crate::codex::query_session_meta(id))
                    .map(|m| m.created_at)
                    .unwrap_or(0);
                map.insert(
                    session_id,
                    LiveSessionInfo {
                        pid: pane.pid,
                        tmux_session: pane.tmux_session,
                        pane_target: pane.pane_target,
                        pane_cwd: pane.pane_cwd,
                        started_at,
                        agent: AgentKind::Codex,
                    },
                );
            }
        }
    }
    map
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "Generalize pane discovery for Claude and Codex"
```

---

### Task 7: Handle Codex sessions in `discover_sessions`

**Files:**
- Modify: `src/session.rs:161-434` (discover_sessions)

The main JSONL scanning loop stays for Claude. We add a parallel path for Codex sessions in the live map.

- [ ] **Step 1: Update `determine_status` to dispatch by agent kind**

Change the signature and body of `determine_status` (lines 1025-1041):

```rust
fn determine_status(
    _path: &Path,
    input_tokens: u64,
    output_tokens: u64,
    pane_target: Option<&str>,
    agent: &AgentKind,
) -> SessionStatus {
    if let Some(target) = pane_target {
        let pane = match agent {
            AgentKind::Claude => pane_status(target),
            AgentKind::Codex => crate::codex::codex_pane_status(target),
        };
        if input_tokens == 0 && output_tokens == 0 && pane == SessionStatus::Idle {
            return SessionStatus::New;
        }
        return pane;
    }

    if input_tokens == 0 && output_tokens == 0 {
        SessionStatus::New
    } else {
        SessionStatus::Idle
    }
}
```

- [ ] **Step 2: Update all `determine_status` call sites to pass `agent`**

There are 2 calls to `determine_status` in `discover_sessions`. Update both:

At line 273 (main JSONL match):
```rust
            let status = determine_status(
                &path,
                info.input_tokens,
                info.output_tokens,
                Some(&live.pane_target),
                &live.agent,
            );
```

At line 370 (resumed session path):
```rust
            let status = determine_status(
                &path,
                info.input_tokens,
                info.output_tokens,
                Some(&live.pane_target),
                &live.agent,
            );
```

- [ ] **Step 3: Add Codex session handling after the Claude JSONL scan loop**

After the existing "Handle live sessions with no direct JSONL name match" block (after line 423, before the sort at line 428), insert:

```rust
    // Handle Codex sessions from live_map.
    // Codex sessions don't have JSONL in ~/.claude/projects/, so they're
    // never matched by the Claude JSONL scan above. Query SQLite instead.
    for (session_id, live) in &live_map {
        if live.agent != AgentKind::Codex {
            continue;
        }
        if known_pids.contains(&live.pid) {
            continue;
        }

        let meta = crate::codex::query_session_meta(session_id);
        let cwd = meta.as_ref()
            .and_then(|m| m.cwd.clone())
            .unwrap_or_else(|| live.pane_cwd.clone());
        let (project_name, relative_dir, branch) = git_project_info(&cwd);

        let input_tokens = meta.as_ref().map(|m| m.tokens_used).unwrap_or(0);
        let status = determine_status(
            &PathBuf::new(),
            input_tokens,
            0,
            Some(&live.pane_target),
            &AgentKind::Codex,
        );

        let tags = read_tmux_tags(&live.tmux_session);
        sessions.push(Session {
            session_id: session_id.clone(),
            project_name,
            branch: meta.as_ref()
                .and_then(|m| m.git_branch.clone())
                .or(branch),
            cwd,
            relative_dir,
            tmux_session: Some(live.tmux_session.clone()),
            pane_target: Some(live.pane_target.clone()),
            model: meta.as_ref().and_then(|m| m.model.clone()),
            effort: meta.as_ref().and_then(|m| m.effort.clone()),
            total_input_tokens: input_tokens,
            total_output_tokens: 0,
            status,
            pid: Some(live.pid),
            last_activity: meta.as_ref()
                .map(|m| m.updated_at)
                .and_then(crate::codex::epoch_to_iso),
            started_at: live.started_at,
            jsonl_path: meta.as_ref()
                .and_then(|m| m.rollout_path.as_ref())
                .map(PathBuf::from)
                .unwrap_or_default(),
            last_file_size: 0,
            tags,
            agent: AgentKind::Codex,
        });
    }
```

**Important:** The existing "Handle live sessions with no direct JSONL name match" loop (for Claude resumed/new sessions) must skip Codex entries, otherwise Codex sessions get duplicated as Claude/New placeholders. Add `if live.agent == AgentKind::Codex { continue; }` at the top of that loop body.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 5: Commit**

```bash
git add src/session.rs
git commit -m "Handle Codex sessions in discover_sessions via SQLite"
```

---

### Task 8: Update UI for Codex color coding

**Files:**
- Modify: `src/ui.rs:63-66` (session name color)
- Modify: `src/ui.rs:157` (title bar)
- Modify: `src/view_ui.rs:567-576` (tamagotchi name color)
- Modify: `src/app.rs:423-444` (JSON output)

- [ ] **Step 1: Color Codex session names in table view**

In `src/ui.rs`, change the session name cell (line 122):

Replace:
```rust
                Cell::from(tmux_name.to_string()),
```

With:
```rust
                Cell::from(Span::styled(
                    tmux_name.to_string(),
                    if session.agent == crate::session::AgentKind::Codex {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    },
                )),
```

- [ ] **Step 2: Update title bar**

In `src/ui.rs`, change line 157:

Replace:
```rust
                .title(" recon — Claude Code Sessions "),
```

With:
```rust
                .title(" recon "),
```

- [ ] **Step 3: Color Codex session names in tamagotchi view**

In `src/view_ui.rs`, change the name style block (lines 568-572):

Replace:
```rust
    let name_style = if is_selected {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
```

With:
```rust
    let name_style = if is_selected {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if session.agent == crate::session::AgentKind::Codex {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };
```

- [ ] **Step 4: Add `agent` to JSON output**

In `src/app.rs`, add `"agent"` field to the `json!` macro in `to_json()` (after `"tags"` at line 443):

```rust
                    "agent": match s.agent {
                        crate::session::AgentKind::Claude => "claude",
                        crate::session::AgentKind::Codex => "codex",
                    },
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 6: Commit**

```bash
git add src/ui.rs src/view_ui.rs src/app.rs
git commit -m "Add Codex color coding in UI and agent field in JSON output"
```

---

### Task 9: Agent-aware session creation in `tmux.rs` and `new_session.rs`

**Files:**
- Modify: `src/tmux.rs:25-70` (create_session)
- Modify: `src/tmux.rs:157-161` (add which_codex)
- Modify: `src/new_session.rs:14-88` (add agent selector)

- [ ] **Step 1: Add `which_codex` and update `create_session` in `tmux.rs`**

Add after `which_claude` (line 161):

```rust
fn which_codex() -> Option<String> {
    let output = Command::new("which").arg("codex").output().ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}
```

Update `create_session` signature (line 25) to accept agent kind:

```rust
pub fn create_session(name: &str, cwd: &str, command: Option<&str>, tags: &[String], agent: &crate::session::AgentKind) -> Result<String, String> {
```

Update the `None` arm of the command match (lines 54-57):

```rust
        None => {
            let bin = match agent {
                crate::session::AgentKind::Claude => which_claude().unwrap_or_else(|| "claude".to_string()),
                crate::session::AgentKind::Codex => which_codex().unwrap_or_else(|| "codex".to_string()),
            };
            tmux_args.push(bin);
        }
```

- [ ] **Step 2: Update all `create_session` call sites**

There are 3 call sites. Update each to pass `&AgentKind::Claude` (the existing behavior):

In `src/new_session.rs` line 85:
```rust
                match tmux::create_session(self.name.trim(), &cwd, None, &[], &crate::session::AgentKind::Claude) {
```

In `src/app.rs` line 222:
```rust
                        if let Ok(name) = tmux::create_session(&default_name, &cwd, None, &[], &crate::session::AgentKind::Claude) {
```

In `src/main.rs` line 41:
```rust
            match tmux::create_session(session_name, session_cwd, command.as_deref(), &tag, &crate::session::AgentKind::Claude) {
```

- [ ] **Step 3: Add agent kind selector to `new_session.rs`**

Add `AgentKind` variant to `Field` enum (line 14):

```rust
enum Field {
    Name,
    Cwd,
    Agent,
}
```

Add `agent` field to `NewSessionForm` struct (after `active`, line 23):

```rust
pub struct NewSessionForm {
    name: String,
    cwd: String,
    cursor_pos: usize,
    active: Field,
    agent: crate::session::AgentKind,
    pub result: Option<String>,
}
```

Initialize in `new()` (line 28):

```rust
        NewSessionForm {
            name,
            cwd,
            cursor_pos,
            active: Field::Name,
            agent: crate::session::AgentKind::Claude,
            result: None,
        }
```

Update `active_text`/`active_text_mut` to handle `Agent` field (return empty/no-op since it's not a text field):

```rust
    fn active_text(&self) -> &str {
        match self.active {
            Field::Name => &self.name,
            Field::Cwd => &self.cwd,
            Field::Agent => "",
        }
    }

    fn active_text_mut(&mut self) -> &mut String {
        match self.active {
            Field::Name => &mut self.name,
            Field::Cwd => &mut self.cwd,
            Field::Agent => &mut self.name, // unused, agent toggles differently
        }
    }
```

In `handle_key`, update the `Enter` on `Field::Cwd` to use selected agent (line 85):

```rust
                match tmux::create_session(self.name.trim(), &cwd, None, &[], &self.agent) {
```

Add agent toggle handling in `handle_key` - when on the Agent field, Space or Enter toggles:

In the Tab/Down handler (line 90-101), add the Agent field to the cycle:

```rust
            KeyCode::Tab | KeyCode::Down => {
                match self.active {
                    Field::Name => {
                        self.active = Field::Cwd;
                        self.cursor_pos = self.cwd.len();
                    }
                    Field::Cwd => {
                        self.active = Field::Agent;
                        self.cursor_pos = 0;
                    }
                    Field::Agent => {
                        self.active = Field::Name;
                        self.cursor_pos = self.name.len();
                    }
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                match self.active {
                    Field::Name => {
                        self.active = Field::Agent;
                        self.cursor_pos = 0;
                    }
                    Field::Cwd => {
                        self.active = Field::Name;
                        self.cursor_pos = self.name.len();
                    }
                    Field::Agent => {
                        self.active = Field::Cwd;
                        self.cursor_pos = self.cwd.len();
                    }
                }
            }
```

Add Space key handler for toggling agent (in the main `match event.code` block, before the `KeyCode::Char(c)` arm):

```rust
            KeyCode::Char(' ') if matches!(self.active, Field::Agent) => {
                self.agent = match self.agent {
                    crate::session::AgentKind::Claude => crate::session::AgentKind::Codex,
                    crate::session::AgentKind::Codex => crate::session::AgentKind::Claude,
                };
            }
```

Also make Enter on the Agent field trigger the create (same as Cwd's Enter logic):

Add before the existing `KeyCode::Enter` handler:

```rust
            KeyCode::Enter if matches!(self.active, Field::Agent) => {
                if self.name.trim().is_empty() {
                    return;
                }
                let cwd = if self.cwd.trim().is_empty() {
                    ".".to_string()
                } else {
                    let c = self.cwd.trim().to_string();
                    if let Some(rest) = c.strip_prefix('~') {
                        if let Some(home) = dirs::home_dir() {
                            format!("{}{rest}", home.display())
                        } else {
                            c
                        }
                    } else {
                        c
                    }
                };
                match tmux::create_session(self.name.trim(), &cwd, None, &[], &self.agent) {
                    Ok(name) => self.result = Some(name),
                    Err(_) => self.result = Some(String::new()),
                }
            }
```

Update `render()` to show the agent selector (line 164). Add a third row for the Agent field:

```rust
        let rows = Layout::vertical([
            Constraint::Length(3), // Name box
            Constraint::Length(3), // Dir box
            Constraint::Length(3), // Agent box
            Constraint::Length(1), // Hints
            Constraint::Min(0),
        ])
        .split(area);
```

After the CWD block rendering, add:

```rust
        // Agent selector
        let agent_active = matches!(self.active, Field::Agent);
        let agent_border = if agent_active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let agent_block = Block::default()
            .borders(Borders::ALL)
            .title(" Agent ")
            .border_style(agent_border);
        let agent_inner = agent_block.inner(rows[2]);
        frame.render_widget(agent_block, rows[2]);

        let agent_label = match self.agent {
            crate::session::AgentKind::Claude => "Claude Code",
            crate::session::AgentKind::Codex => "Codex CLI",
        };
        let agent_color = match self.agent {
            crate::session::AgentKind::Claude => Color::White,
            crate::session::AgentKind::Codex => Color::Cyan,
        };
        frame.render_widget(
            Paragraph::new(agent_label).style(Style::default().fg(agent_color)),
            agent_inner,
        );
```

Update hint rows to use `rows[3]` and `rows[4]`:

```rust
        let hint = match self.active {
            Field::Name => Line::from(vec![
                Span::styled(" Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" next  "),
                Span::styled("Tab", Style::default().fg(Color::Cyan)),
                Span::raw(" switch  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ]),
            Field::Cwd => Line::from(vec![
                Span::styled(" Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" next  "),
                Span::styled("Tab", Style::default().fg(Color::Cyan)),
                Span::raw(" switch  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ]),
            Field::Agent => Line::from(vec![
                Span::styled(" Space", Style::default().fg(Color::Cyan)),
                Span::raw(" toggle  "),
                Span::styled("Enter", Style::default().fg(Color::Cyan)),
                Span::raw(" create  "),
                Span::styled("Tab", Style::default().fg(Color::Cyan)),
                Span::raw(" switch  "),
                Span::styled("Esc", Style::default().fg(Color::Cyan)),
                Span::raw(" cancel"),
            ]),
        };
        frame.render_widget(Paragraph::new(hint), rows[3]);
```

Update cursor positioning to handle Agent field:

```rust
        let (cx, cy) = match self.active {
            Field::Name => (name_inner.x + self.cursor_pos as u16, name_inner.y),
            Field::Cwd => (cwd_inner.x + self.cursor_pos as u16, cwd_inner.y),
            Field::Agent => (agent_inner.x, agent_inner.y), // no text cursor for toggle
        };
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 5: Commit**

```bash
git add src/tmux.rs src/new_session.rs src/app.rs src/main.rs
git commit -m "Agent-aware session creation with Claude/Codex selector"
```

---

### Task 10: Agent-aware resume in `tmux.rs` and `history.rs`

**Files:**
- Modify: `src/tmux.rs:72-119` (resume_session)
- Modify: `src/history.rs` (add Codex sessions to picker)

- [ ] **Step 1: Update `resume_session` to handle both agents**

Change `resume_session` signature to accept agent kind:

```rust
pub fn resume_session(session_id: &str, name: Option<&str>, agent: &crate::session::AgentKind) -> Result<String, String> {
```

Replace the command construction (lines 93-109) with agent-dispatched logic:

```rust
    let (bin, args): (String, Vec<String>) = match agent {
        crate::session::AgentKind::Claude => {
            let path = which_claude().unwrap_or_else(|| "claude".to_string());
            (path, vec!["--resume".to_string(), session_id.to_string()])
        }
        crate::session::AgentKind::Codex => {
            let path = which_codex().unwrap_or_else(|| "codex".to_string());
            (path, vec!["resume".to_string(), session_id.to_string()])
        }
    };

    let env_var = format!("RECON_RESUMED_FROM={session_id}");
    let mut tmux_cmd_args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        session_name.clone(),
        "-c".to_string(),
        cwd,
        "-e".to_string(),
        env_var,
        bin,
    ];
    tmux_cmd_args.extend(args);

    let status = Command::new("tmux")
        .args(&tmux_cmd_args)
        .status()
        .map_err(|e| format!("Failed to create tmux session: {e}"))?;

    if !status.success() {
        return Err("tmux new-session failed".to_string());
    }

    Ok(session_name)
```

Also update `find_session_cwd` in `session.rs` for Codex. For now, the `resume_session` function already has a fallback to current dir, and Codex sessions store CWD in SQLite. We can query it:

Add to `src/codex.rs`:

```rust
/// Find the CWD for a Codex session from SQLite.
pub fn find_codex_session_cwd(session_id: &str) -> Option<String> {
    query_session_meta(session_id).and_then(|m| m.cwd)
}
```

Update `resume_session` CWD lookup to try Codex:

```rust
    let cwd = match agent {
        crate::session::AgentKind::Claude => {
            session::find_session_cwd(session_id)
                .filter(|c| session::validate_cwd(c))
        }
        crate::session::AgentKind::Codex => {
            crate::codex::find_codex_session_cwd(session_id)
                .filter(|c| session::validate_cwd(c))
        }
    }
    .or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).ok())
    .unwrap_or_else(|| ".".to_string());
```

- [ ] **Step 2: Update `resume_session` call sites to pass agent**

In `src/park.rs` line 138:
```rust
        match tmux::resume_session(&s.session_id, Some(&s.tmux_session), &crate::session::AgentKind::Claude) {
```

In `src/main.rs` line 56:
```rust
                match tmux::resume_session(&session_id, name.as_deref(), &crate::session::AgentKind::Claude) {
```

In `src/main.rs` line 71:
```rust
                    match tmux::resume_session(&session_id, Some(&sess_name), &crate::session::AgentKind::Claude) {
```

(These default to Claude for now - the history picker in Task 10 step 3 will pass the correct agent.)

- [ ] **Step 3: Add Codex sessions to history picker**

In `src/history.rs`, add `agent` field to `ResumeEntry`:

```rust
pub struct ResumeEntry {
    pub session_id: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub tokens: u64,
    pub last_active: String,
    pub agent: crate::session::AgentKind,
}
```

Add a function to find resumable Codex sessions:

```rust
fn find_resumable_codex_sessions(live_ids: &HashSet<String>) -> Vec<ResumeEntry> {
    let db_path = match dirs::home_dir() {
        Some(h) => h.join(".codex").join("state_5.sqlite"),
        None => return vec![],
    };
    if !db_path.exists() {
        return vec![];
    }

    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match rusqlite::Connection::open_with_flags(&db_path, flags) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let _ = conn.pragma_update(None, "journal_mode", "wal");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(7 * 24 * 3600);

    let mut stmt = match conn.prepare(
        "SELECT id, model, reasoning_effort, tokens_used, cwd, git_branch, updated_at \
         FROM threads WHERE updated_at > ?1 AND archived = 0 \
         ORDER BY updated_at DESC LIMIT 20"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = stmt.query_map(rusqlite::params![cutoff as i64], |row| {
        let id: String = row.get(0)?;
        let model: Option<String> = row.get(1).ok();
        let _effort: Option<String> = row.get(2).ok();
        let tokens: i64 = row.get(3).unwrap_or(0);
        let cwd: Option<String> = row.get(4).ok();
        let branch: Option<String> = row.get(5).ok();
        let updated_at: i64 = row.get(6).unwrap_or(0);
        Ok((id, model, tokens, cwd, branch, updated_at))
    });

    let mut entries = Vec::new();
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (id, model, tokens, cwd, branch, updated_at) = row;
            if live_ids.contains(&id) || tokens == 0 {
                continue;
            }
            let last_active = crate::codex::epoch_to_iso(updated_at as u64)
                .unwrap_or_default();
            entries.push(ResumeEntry {
                session_id: id,
                cwd: cwd.unwrap_or_default(),
                branch,
                model,
                tokens: tokens as u64,
                last_active,
                agent: crate::session::AgentKind::Codex,
            });
        }
    }
    entries
}
```

Update `find_resumable_sessions` to include Claude `agent` field and merge with Codex:

Add `agent: crate::session::AgentKind::Claude,` to the `entries.push(ResumeEntry { ... })` block (line 92):

```rust
            entries.push(ResumeEntry {
                session_id,
                cwd: cwd.clone(),
                branch: summary.branch,
                model: summary.model,
                tokens: summary.tokens,
                last_active: format_epoch_ms(mtime_ms),
                agent: crate::session::AgentKind::Claude,
            });
```

At the end of `find_resumable_sessions`, before the sort:

```rust
    // Add Codex sessions
    entries.extend(find_resumable_codex_sessions(&live_ids));
```

Update the resume picker to pass agent kind. In `run_resume_picker`, change the result type and return:

At line 252-253, change:
```rust
                            result = Some((entry.session_id.clone(), name, entry.agent.clone()));
```

Change the function signature:
```rust
pub fn run_resume_picker() -> io::Result<Option<(String, String, crate::session::AgentKind)>> {
```

And the `None` result at line 234 and 249:
```rust
                        result = None;
```

Update `src/main.rs` to handle the new return type (lines 69-80):

```rust
                let result = history::run_resume_picker()?;
                if let Some((session_id, sess_name, agent)) = result {
                    match tmux::resume_session(&session_id, Some(&sess_name), &agent) {
```

- [ ] **Step 4: Add Codex color indicator in the resume picker table**

In the resume picker's row rendering, color the session ID cell for Codex sessions:

```rust
                        let id_style = if e.agent == crate::session::AgentKind::Codex {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default()
                        };
                        // ... in the Row::new:
                        Cell::from(Span::styled(short_id.to_string(), id_style)),
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 6: Commit**

```bash
git add src/tmux.rs src/history.rs src/codex.rs src/main.rs
git commit -m "Agent-aware resume with Codex sessions in history picker"
```

---

### Task 11: Agent-aware park/unpark in `park.rs`

**Files:**
- Modify: `src/park.rs:6-17` (ParkedSession struct)
- Modify: `src/park.rs:23-98` (park function)
- Modify: `src/park.rs:100-153` (unpark function)

- [ ] **Step 1: Add `agent` field to `ParkedSession`**

```rust
#[derive(Serialize, Deserialize)]
struct ParkedSession {
    session_id: String,
    tmux_session: String,
    cwd: String,
    agent: String, // "claude" or "codex"
}
```

Using a `String` for serde simplicity rather than deriving Serialize/Deserialize on `AgentKind`.

- [ ] **Step 2: Update `park()` to store agent kind**

In the `parked` iterator (lines 30-46), add agent field:

```rust
            Some(ParkedSession {
                session_id: resume_id,
                tmux_session: s.tmux_session.as_ref()?.clone(),
                cwd: s.cwd.clone(),
                agent: match s.agent {
                    crate::session::AgentKind::Claude => "claude".to_string(),
                    crate::session::AgentKind::Codex => "codex".to_string(),
                },
            })
```

- [ ] **Step 3: Update `unpark()` to use stored agent kind**

In the unpark loop (lines 137-149):

```rust
    for s in &park_file.sessions {
        let agent = match s.agent.as_str() {
            "codex" => crate::session::AgentKind::Codex,
            _ => crate::session::AgentKind::Claude,
        };
        match tmux::resume_session(&s.session_id, Some(&s.tmux_session), &agent) {
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 5: Commit**

```bash
git add src/park.rs
git commit -m "Agent-aware park/unpark with stored agent kind"
```

---

### Task 12: Final integration test and version bump

**Files:**
- Modify: `Cargo.toml:3` (version)

- [ ] **Step 1: Run full build**

Run: `cargo build 2>&1`
Expected: successful compilation with no errors

- [ ] **Step 2: Run all tests**

Run: `cargo test 2>&1`
Expected: all tests pass

- [ ] **Step 3: Install and smoke test**

Run: `cargo install --path .`
Then run `recon --json` and verify output includes `"agent"` field.

- [ ] **Step 4: Bump version**

In `Cargo.toml`, change version from `"0.6.0"` to `"0.7.0"`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml
git commit -m "Bump version to 0.7.0"
```
