use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::model;

/// Maximum bytes per JSONL line before discarding.
/// Prevents OOM from malicious files with unbounded lines.
const MAX_LINE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Read a line with a cap on allocation. Uses fill_buf/consume to avoid
/// allocating beyond the cap. Returns Ok(0) at EOF. Overlong lines are
/// consumed and discarded (buf left empty, positive byte count returned
/// so callers can distinguish from EOF).
pub(crate) fn read_line_capped<R: Read>(
    reader: &mut BufReader<R>,
    buf: &mut String,
) -> std::io::Result<usize> {
    let mut raw = Vec::new();
    let mut overflowed = false;
    let mut total_consumed = 0usize;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }

        let newline_pos = available.iter().position(|&b| b == b'\n');
        let chunk_end = newline_pos.map(|p| p + 1).unwrap_or(available.len());

        if !overflowed {
            if raw.len() + chunk_end <= MAX_LINE_BYTES {
                raw.extend_from_slice(&available[..chunk_end]);
            } else {
                overflowed = true;
                raw = Vec::new();
                buf.clear(); // ensure buf is empty on overflow even if caller didn't pre-clear
            }
        }

        total_consumed += chunk_end;
        reader.consume(chunk_end);

        if newline_pos.is_some() {
            break;
        }
    }

    if total_consumed == 0 {
        return Ok(0); // EOF
    }

    if !overflowed {
        *buf = String::from_utf8(raw).unwrap_or_default();
    }

    Ok(total_consumed)
}

/// Validate that a CWD path is safe to pass to external commands.
/// Must be absolute and resolve to an existing directory.
pub(crate) fn validate_cwd(cwd: &str) -> bool {
    let path = Path::new(cwd);
    path.is_absolute() && path.is_dir()
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    New,
    Working,
    Idle,
    Input,
    BackgroundTasks(u32),
    BackgroundAgents(u32),
}

impl SessionStatus {
    pub fn label(&self) -> String {
        match self {
            SessionStatus::New => "New".to_string(),
            SessionStatus::Working => "Working".to_string(),
            SessionStatus::Idle => "Idle".to_string(),
            SessionStatus::Input => "Input".to_string(),
            SessionStatus::BackgroundTasks(1) => "1 task".to_string(),
            SessionStatus::BackgroundTasks(count) => format!("{count} tasks"),
            SessionStatus::BackgroundAgents(1) => "1 agent".to_string(),
            SessionStatus::BackgroundAgents(count) => format!("{count} agents"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentKind {
    Claude,
    Codex,
}

#[derive(Debug, Clone)]
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
    pub session_name: Option<String>,
    pub agent: AgentKind,
    // Overrides model-derived context window (used by Codex where the rollout
    // reports the actual window size, which differs from naive model lookups).
    pub context_window: Option<u64>,
}

impl Session {
    pub fn room_id(&self) -> String {
        match &self.relative_dir {
            Some(dir) => format!("{} \u{203A} {}", self.project_name, dir),
            None => self.project_name.clone(),
        }
    }

    pub fn token_display(&self) -> String {
        let used = self.total_input_tokens + self.total_output_tokens;
        let window = self.context_window.unwrap_or_else(|| {
            self.model.as_deref().map(model::context_window).unwrap_or(200_000)
        });
        format!("{}k / {}", used / 1000, format_window(window))
    }

    pub fn token_ratio(&self) -> f64 {
        let used = self.total_input_tokens + self.total_output_tokens;
        let window = self.context_window.unwrap_or_else(|| {
            self.model.as_deref().map(model::context_window).unwrap_or(200_000)
        });
        if window == 0 {
            return 0.0;
        }
        used as f64 / window as f64
    }

    pub fn model_display(&self) -> String {
        match &self.model {
            Some(m) => model::format_with_effort(m, self.effort.as_deref().unwrap_or("")),
            None => "—".to_string(),
        }
    }
}

pub fn format_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M", tokens / 1_000_000)
    } else {
        format!("{}k", tokens / 1000)
    }
}

/// Discover sessions by scanning JSONL files, then matching to live tmux panes.
pub fn discover_sessions(prev_sessions: &HashMap<String, Session>) -> Vec<Session> {

    let claude_dir = match dirs::home_dir() {
        Some(h) => h.join(".claude").join("projects"),
        None => return vec![],
    };

    if !claude_dir.exists() {
        return vec![];
    }

    // Build the live session map: session_id → (pid, tmux_name, started_at)
    // by joining ~/.claude/sessions/{PID}.json with tmux pane info.
    let live_map = build_live_session_map();

    let mut sessions: Vec<Session> = Vec::new();
    let mut matched_session_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // Scan all JSONL files across project directories.
    // No mtime cutoff needed — the live_map check (below) already filters out
    // dead sessions, and skipping the stat() call is faster than doing it.
    let entries = match fs::read_dir(&claude_dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        let jsonl_files = match fs::read_dir(&project_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for jentry in jsonl_files.flatten() {
            let path = jentry.path();
            if path.is_dir() {
                continue;
            }
            if !path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                continue;
            }

            let session_id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            // Look up in live map — skip if no live process
            let live = match live_map.get(&session_id) {
                Some(l) => l,
                None => continue,
            };

            // Same session_id can appear in multiple project dirs (e.g. session
            // started in one CWD then moved to a worktree). Prefer the larger file.
            if matched_session_ids.contains(&session_id) {
                if let Some(existing) = sessions.iter_mut().find(|s| s.session_id == session_id) {
                    let existing_size = existing.jsonl_path.metadata().ok().map(|m| m.len()).unwrap_or(0);
                    let new_size = path.metadata().ok().map(|m| m.len()).unwrap_or(0);
                    if new_size > existing_size {
                        let prev = prev_sessions.get(&session_id);
                        let info = parse_jsonl(
                            &path,
                            prev.map(|s| s.last_file_size).unwrap_or(0),
                            prev.map(|s| s.total_input_tokens).unwrap_or(0),
                            prev.map(|s| s.total_output_tokens).unwrap_or(0),
                            prev.and_then(|s| s.model.clone()),
                            prev.and_then(|s| s.effort.clone()),
                            prev.and_then(|s| s.last_activity.clone()),
                            prev.and_then(|s| s.session_name.clone()),
                        );
                        let cwd = info.cwd
                            .or_else(|| prev.map(|s| s.cwd.clone()))
                            .unwrap_or_else(|| decode_project_path(&project_dir));
                        let (project_name, relative_dir, branch) = git_project_info(&cwd);
                        existing.project_name = project_name;
                        existing.relative_dir = relative_dir;
                        existing.branch = branch;
                        existing.cwd = cwd;
                        existing.model = info.model;
                        existing.effort = info.effort;
                        existing.total_input_tokens = info.input_tokens;
                        existing.total_output_tokens = info.output_tokens;
                        existing.last_activity = info.last_activity;
                        existing.session_name = info.session_name;
                        existing.jsonl_path = path;
                        existing.last_file_size = info.file_size;
                    }
                }
                continue;
            }

            // Incremental JSONL parsing
            let prev = prev_sessions.get(&session_id);
            let info = parse_jsonl(
                &path,
                prev.map(|s| s.last_file_size).unwrap_or(0),
                prev.map(|s| s.total_input_tokens).unwrap_or(0),
                prev.map(|s| s.total_output_tokens).unwrap_or(0),
                prev.and_then(|s| s.model.clone()),
                prev.and_then(|s| s.effort.clone()),
                prev.and_then(|s| s.last_activity.clone()),
                prev.and_then(|s| s.session_name.clone()),
            );

            let cwd = info
                .cwd
                .or_else(|| prev.map(|s| s.cwd.clone()))
                .unwrap_or_else(|| decode_project_path(&project_dir));
            let (project_name, relative_dir, branch) = git_project_info(&cwd);

            let status = determine_status(
                &path,
                info.input_tokens,
                info.output_tokens,
                Some(&live.pane_target),
                &live.agent,
            );

            matched_session_ids.insert(session_id.clone());

            let tags = read_tmux_tags(&live.tmux_session);
            sessions.push(Session {
                session_id,
                project_name,
                branch,
                cwd,
                relative_dir,
                tmux_session: Some(live.tmux_session.clone()),
                pane_target: Some(live.pane_target.clone()),
                model: info.model,
                effort: info.effort,
                total_input_tokens: info.input_tokens,
                total_output_tokens: info.output_tokens,
                status,
                pid: Some(live.pid),
                last_activity: info.last_activity,
                started_at: live.started_at,
                jsonl_path: path,
                last_file_size: info.file_size,
                tags,
                session_name: info.session_name,
                agent: AgentKind::Claude,
                context_window: None,
            });
        }
    }
    // Handle live sessions with no direct JSONL name match.
    // This covers two cases:
    //   1. Brand-new sessions (no JSONL yet) → show as New placeholder
    //   2. Resumed sessions (claude --resume creates a new session-id in the session file
    //      but continues appending to the original JSONL) → find via lsof, show real data
    //
    // Dedup by PID, not tmux session name. Multiple Claude instances can share
    // a tmux session (e.g. two panes). Deduping by session name would silently
    // hide the second instance. PID is the unique identifier per Claude process,
    // so each instance gets its own stable entry in the table — even if the TUI
    // shows duplicate session names.
    let known_pids: std::collections::HashSet<i32> = sessions
        .iter()
        .filter_map(|s| s.pid)
        .collect();

    for (session_id_key, live) in &live_map {
        if known_pids.contains(&live.pid) {
            continue;
        }

        // Codex sessions are handled in their own loop below; skip them here
        // to prevent duplicate Claude/New entries being created for Codex PIDs.
        if live.agent == AgentKind::Codex {
            continue;
        }

        // For sessions that have a real session-id (not the "tmux-{name}" placeholder),
        // try to find the JSONL via resume detection. This handles resumed sessions
        // where the session file's session-id doesn't match the original JSONL filename.
        //
        // However, if the session was /reset after being resumed, the ps args still
        // show the old --resume ID while a new JSONL exists. In that case, the resume
        // JSONL is stale. We detect this: if the resumed JSONL's session-id matches
        // the session_id_key (from {PID}.json), the resume is current; otherwise
        // /reset happened and we skip the stale resume path.
        let jsonl_path = if !session_id_key.starts_with("tmux-") {
            let cached = prev_sessions
                .get(session_id_key.as_str())
                .filter(|s| !s.jsonl_path.as_os_str().is_empty())
                .map(|s| s.jsonl_path.clone());
            cached.or_else(|| find_jsonl_for_resumed_session(&live.tmux_session, live.pid))
        } else {
            None
        };

        let resolved_path = jsonl_path;

        // Mark as claimed so other sessions in the same dir don't grab the same JSONL
        if let Some(ref path) = resolved_path {
            if let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) {
                matched_session_ids.insert(stem);
            }
        }

        if let Some(path) = resolved_path {
            let prev = prev_sessions.get(session_id_key.as_str());
            let info = parse_jsonl(
                &path,
                prev.map(|s| s.last_file_size).unwrap_or(0),
                prev.map(|s| s.total_input_tokens).unwrap_or(0),
                prev.map(|s| s.total_output_tokens).unwrap_or(0),
                prev.and_then(|s| s.model.clone()),
                prev.and_then(|s| s.effort.clone()),
                prev.and_then(|s| s.last_activity.clone()),
                prev.and_then(|s| s.session_name.clone()),
            );

            let cwd = info.cwd.clone().unwrap_or_else(|| live.pane_cwd.clone());
            let (project_name, relative_dir, branch) = git_project_info(&cwd);

            let status = determine_status(
                &path,
                info.input_tokens,
                info.output_tokens,
                Some(&live.pane_target),
                &live.agent,
            );

            let tags = read_tmux_tags(&live.tmux_session);
            sessions.push(Session {
                session_id: session_id_key.clone(),
                project_name,
                relative_dir,
                branch,
                cwd,
                tmux_session: Some(live.tmux_session.clone()),
                pane_target: Some(live.pane_target.clone()),
                model: info.model,
                effort: info.effort,
                total_input_tokens: info.input_tokens,
                total_output_tokens: info.output_tokens,
                status,
                pid: Some(live.pid),
                last_activity: info.last_activity,
                started_at: live.started_at,
                jsonl_path: path,
                last_file_size: info.file_size,
                tags,
                session_name: info.session_name,
                agent: AgentKind::Claude,
                context_window: None,
            });
        } else {
            // No JSONL found — brand-new session, show as New placeholder
            let (project_name, relative_dir, branch) = git_project_info(&live.pane_cwd);
            let tags = read_tmux_tags(&live.tmux_session);
            sessions.push(Session {
                session_id: session_id_key.clone(),
                project_name,
                relative_dir,
                branch,
                cwd: live.pane_cwd.clone(),
                tmux_session: Some(live.tmux_session.clone()),
                pane_target: Some(live.pane_target.clone()),
                model: None,
                effort: None,
                total_input_tokens: 0,
                total_output_tokens: 0,
                status: SessionStatus::New,
                pid: Some(live.pid),
                last_activity: None,
                started_at: live.started_at,
                jsonl_path: PathBuf::new(),
                last_file_size: 0,
                tags,
                session_name: None,
                agent: AgentKind::Claude,
                context_window: None,
            });
        }
    }

    // Handle Codex sessions from live_map.
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

        // Read per-turn token info from rollout JSONL (current context, not accumulated total)
        let rollout_path = meta.as_ref()
            .and_then(|m| m.rollout_path.as_ref())
            .map(PathBuf::from)
            .unwrap_or_default();
        let token_info = crate::codex::read_rollout_tokens(&rollout_path);
        let input_tokens = token_info.as_ref().map(|t| t.last_input_tokens).unwrap_or(0);
        let ctx_window = token_info.as_ref().map(|t| t.context_window).filter(|&w| w > 0);

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
            jsonl_path: rollout_path,
            last_file_size: 0,
            tags,
            session_name: meta.as_ref().and_then(|m| m.title.clone()),
            agent: AgentKind::Codex,
            context_window: ctx_window,
        });
    }

    // Sort by last activity at minute resolution (most recent first),
    // then by started_at as tiebreaker. Truncating to the minute prevents
    // the table from reordering on every poll cycle.
    sessions.sort_by(|a, b| {
        truncate_to_minute(&b.last_activity)
            .cmp(&truncate_to_minute(&a.last_activity))
            .then(b.started_at.cmp(&a.started_at))
    });
    sessions
}

/// Truncate an ISO timestamp to minute resolution for stable sorting.
/// "2026-03-19T21:25:34.098Z" → Some("2026-03-19T21:25")
fn truncate_to_minute(ts: &Option<String>) -> Option<String> {
    ts.as_ref().map(|s| s.get(..16).unwrap_or(s).to_string())
}

/// Info about a live agent session, built from tmux + session files.
struct LiveSessionInfo {
    pid: i32,
    tmux_session: String,
    pane_target: String,
    pane_cwd: String,
    started_at: u64,
    agent: AgentKind,
}

/// Build a map from session_id -> live session info.
///
/// Joins multiple sources:
///   - tmux list-panes: discover agent panes (Claude and Codex)
///   - ~/.claude/sessions/{PID}.json: PID -> (session_id, started_at) for Claude
///   - Codex SQLite DB: session metadata for Codex
fn build_live_session_map() -> HashMap<String, LiveSessionInfo> {
    let pid_session_map = read_pid_session_map();
    let tmux_panes = discover_agent_tmux_panes();

    let mut map = HashMap::new();
    for pane in tmux_panes {
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
                    // Tmux pane running claude but no session file yet (just started).
                    // Use pane_target as placeholder key so two panes don't collide.
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
                // For Codex, use the codex_session_id as the map key.
                // Query SQLite for started_at.
                if let Some(session_id) = pane.codex_session_id {
                    let started_at = crate::codex::query_session_meta(&session_id)
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
    }
    map
}

#[derive(Debug)]
struct ParsedInfo {
    input_tokens: u64,
    output_tokens: u64,
    model: Option<String>,
    effort: Option<String>,
    cwd: Option<String>,
    last_activity: Option<String>,
    session_name: Option<String>,
    file_size: u64,
}

use std::sync::Mutex;
use std::time::Instant;

struct GitInfo {
    repo_name: String,
    relative_dir: Option<String>,
    branch: Option<String>,
    fetched_at: Instant,
}

static GIT_CACHE: Mutex<Option<HashMap<String, GitInfo>>> = Mutex::new(None);

const GIT_CACHE_TTL: Duration = Duration::from_secs(30);

/// Get the git project name, relative_dir, and branch for a directory (cached for 30s).
fn git_project_info(cwd: &str) -> (String, Option<String>, Option<String>) {
    if !validate_cwd(cwd) {
        let fallback = Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd.to_string());
        return (fallback, None, None);
    }

    {
        let cache = GIT_CACHE.lock().unwrap();
        if let Some(info) = cache.as_ref().and_then(|c| c.get(cwd)) {
            if info.fetched_at.elapsed() < GIT_CACHE_TTL {
                return (info.repo_name.clone(), info.relative_dir.clone(), info.branch.clone());
            }
        }
    }

    let repo_name = fetch_git_repo_name(cwd);
    let relative_dir = fetch_relative_dir(cwd);
    let branch = fetch_git_branch(cwd);

    let mut cache = GIT_CACHE.lock().unwrap();
    if cache.is_none() {
        *cache = Some(HashMap::new());
    }
    cache.as_mut().unwrap().insert(
        cwd.to_string(),
        GitInfo {
            repo_name: repo_name.clone(),
            relative_dir: relative_dir.clone(),
            branch: branch.clone(),
            fetched_at: Instant::now(),
        },
    );
    (repo_name, relative_dir, branch)
}

fn fetch_git_repo_name(cwd: &str) -> String {
    // Use --git-common-dir to get a stable name across worktrees
    fetch_canonical_repo_name(cwd).unwrap_or_else(|| {
        Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| cwd.to_string())
    })
}

/// Get the canonical repo name from --git-common-dir (stable across worktrees).
fn fetch_canonical_repo_name(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let common_path = if Path::new(&common).is_absolute() {
        PathBuf::from(&common)
    } else {
        PathBuf::from(cwd).join(&common)
    };
    let resolved = common_path.canonicalize().unwrap_or(common_path);
    let repo_root = if resolved.file_name().map(|n| n == ".git").unwrap_or(false) {
        resolved.parent()?
    } else {
        &resolved
    };
    repo_root.file_name().map(|n| n.to_string_lossy().to_string())
}

fn fetch_git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

/// Compute the relative path from the git worktree root to the CWD.
///
/// Returns None if CWD is the worktree root (or not a git repo).
///   /repos/line5              → None
///   /repos/line5/tools/solo   → Some("tools/solo")
fn fetch_relative_dir(cwd: &str) -> Option<String> {
    let toplevel = match std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return None,
    };

    // Canonicalize both paths to resolve symlinks (e.g. /tmp → /private/tmp on macOS)
    let cwd_resolved = Path::new(cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd));
    let top_resolved = Path::new(&toplevel)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&toplevel));
    let relative = cwd_resolved
        .strip_prefix(&top_resolved)
        .unwrap_or(Path::new(""));

    if relative.as_os_str().is_empty() || relative == Path::new(".") {
        None
    } else {
        Some(relative.display().to_string())
    }
}

/// Decode an encoded project directory name back to a path.
/// `-Users-gavra-repos-yaba` -> `/Users/gavra/repos/yaba`
/// This is a best-effort reverse of the encoding (ambiguous for `.` and `_`).
fn decode_project_path(project_dir: &Path) -> String {
    let name = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // The encoded name replaces / with -, so the first char is always -
    // Convert back: leading - becomes /, internal - becomes /
    // This is lossy (can't distinguish original - from / or . or _) but good enough
    if name.starts_with('-') {
        name.replacen('-', "/", 1)
            .replace('-', "/")
    } else {
        name
    }
}

/// Minimal serde structs for JSONL parsing.
#[derive(Deserialize)]
struct JsonlEntry {
    #[serde(default)]
    message: Option<MessageEntry>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct MessageEntry {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<UsageEntry>,
}

#[derive(Deserialize)]
struct UsageEntry {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// Parse JSONL file, incrementally if possible.
fn parse_jsonl(
    path: &Path,
    prev_file_size: u64,
    prev_input: u64,
    prev_output: u64,
    prev_model: Option<String>,
    prev_effort: Option<String>,
    prev_activity: Option<String>,
    prev_session_name: Option<String>,
) -> ParsedInfo {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return ParsedInfo {
                input_tokens: prev_input,
                output_tokens: prev_output,
                model: prev_model,
                effort: prev_effort,
                cwd: None,
                last_activity: prev_activity,
                session_name: prev_session_name,
                file_size: 0,
            }
        }
    };

    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);

    if file_size == prev_file_size && prev_file_size > 0 {
        return ParsedInfo {
            input_tokens: prev_input,
            output_tokens: prev_output,
            model: prev_model,
            effort: prev_effort,
            cwd: None,
            last_activity: prev_activity,
            session_name: prev_session_name,
            file_size,
        };
    }

    let mut reader = BufReader::new(file);
    let mut total_input = prev_input;
    let mut total_output = prev_output;
    let mut model = prev_model;
    let mut effort = prev_effort;
    let mut last_activity = prev_activity;
    let mut session_name = prev_session_name;
    let mut cwd = None;

    if prev_file_size > 0 {
        let _ = reader.seek(SeekFrom::Start(prev_file_size));
    } else {
        total_input = 0;
        total_output = 0;
        model = None;
        effort = None;
        last_activity = None;
    }

    let mut line = String::new();
    loop {
        line.clear();
        match read_line_capped(&mut reader, &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains("\"type\"") {
            continue;
        }

        if trimmed.contains("\"type\":\"assistant\"") {
            // Skip synthetic entries — they have 0 tokens and overwrite real data
            if trimmed.contains("\"<synthetic>\"") {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<JsonlEntry>(trimmed) {
                if let Some(ts) = entry.timestamp {
                    last_activity = Some(ts);
                }
                if entry.cwd.is_some() {
                    cwd = entry.cwd;
                }
                if let Some(msg) = entry.message {
                    if let Some(m) = msg.model {
                        model = Some(m);
                    }
                    if let Some(usage) = msg.usage {
                        total_input = usage.input_tokens
                            + usage.cache_creation_input_tokens
                            + usage.cache_read_input_tokens;
                        total_output = usage.output_tokens;
                    }
                }
            }
        } else if trimmed.contains("\"type\":\"user\"") || trimmed.contains("\"type\":\"system\"") {
            if let Ok(entry) = serde_json::from_str::<JsonlEntry>(trimmed) {
                if let Some(ts) = entry.timestamp {
                    last_activity = Some(ts);
                }
                if entry.cwd.is_some() {
                    cwd = entry.cwd;
                }
            }
            // Extract model + effort from /model command stdout recorded in JSONL:
            //   "Set model to Opus 4.6 (1M context) (default) with max effort"
            //   "Set model to Sonnet 4.6"
            if trimmed.contains("<local-command-stdout>Set model to")
                && !trimmed.contains("toolUseResult")
                && !trimmed.contains("tool_result")
            {
                let stdout_pos = trimmed.find("<local-command-stdout>Set model to").unwrap();
                let tag_end = stdout_pos + "<local-command-stdout>Set model to".len();
                let raw_remainder = &trimmed[tag_end..];
                // Truncate at closing tag
                let raw_remainder = raw_remainder
                    .find("</local-command-stdout>")
                    .map_or(raw_remainder, |end| &raw_remainder[..end]);
                let remainder = strip_ansi(raw_remainder);
                let remainder = remainder.trim();

                // Extract effort if present ("with <effort> effort")
                let (model_part, new_effort) = if let Some(wp) = remainder.find("with ") {
                    let after_with = &remainder[wp + 5..];
                    let eff = after_with.find(" effort")
                        .map(|end| after_with[..end].trim().to_string())
                        .filter(|s| !s.is_empty());
                    (&remainder[..wp], eff)
                } else {
                    (&remainder[..], None)
                };
                if let Some(e) = new_effort {
                    effort = Some(e);
                }

                // Extract model: strip suffixes like "(1M context)" and "(default)"
                let model_name = model_part
                    .trim()
                    .trim_end_matches("(default)")
                    .trim()
                    .trim_end_matches("(1M context)")
                    .trim()
                    .trim_end_matches("(200k context)")
                    .trim();
                if let Some(id) = model::id_from_display_name(model_name) {
                    model = Some(id.to_string());
                }
            }
        } else if trimmed.contains("\"type\":\"custom-title\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(title) = v.get("customTitle").and_then(|t| t.as_str()) {
                    if title.is_empty() {
                        session_name = None;
                    } else {
                        session_name = Some(title.to_string());
                    }
                }
            }
        }
    }

    ParsedInfo {
        input_tokens: total_input,
        output_tokens: total_output,
        model,
        effort,
        cwd,
        last_activity,
        session_name,
        file_size,
    }
}

/// For a resumed session, find the original JSONL by locating the session-id
/// that `claude --resume` was called with.
///
/// `claude --resume <orig-id>` writes a new session-id to its session file but
/// continues appending to the original JSONL (named after the old session-id).
///
/// Strategy (in order):
///  1. Read `RECON_RESUMED_FROM` from the tmux session environment — set by
///     `recon --resume` at session creation time. Reliable and zero-overhead.
///  2. Fall back to parsing `ps` args for sessions started outside of recon
///     (e.g. the user ran `claude --resume <id>` in a tmux session manually).
fn find_jsonl_for_resumed_session(tmux_session: &str, pid: i32) -> Option<PathBuf> {
    // Try tmux environment variable first (set by recon --resume)
    let original_id = read_tmux_env(tmux_session, "RECON_RESUMED_FROM")
        // Fall back to parsing ps args
        .or_else(|| parse_resume_id_from_ps(pid))?;

    find_jsonl_by_session_id(&original_id)
}

/// Read a variable from a tmux session's environment table.
fn read_tmux_env(session_name: &str, var: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["show-environment", "-t", session_name, var])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    // Output format: "VAR=value\n"
    let line = String::from_utf8_lossy(&output.stdout);
    line.trim().split_once('=').map(|(_, v)| v.to_string())
}

/// Read RECON_TAGS from a tmux session's environment and parse into key:value pairs.
fn read_tmux_tags(session_name: &str) -> HashMap<String, String> {
    read_tmux_env(session_name, "RECON_TAGS")
        .map(|val| {
            val.split(',')
                .filter_map(|tag| tag.split_once(':').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `--resume <session-id>` from the process command line via ps.
/// Fallback for sessions not created by `recon --resume`.
fn parse_resume_id_from_ps(pid: i32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;

    let args = String::from_utf8_lossy(&output.stdout);
    args.trim()
        .split_whitespace()
        .skip_while(|&a| a != "--resume")
        .nth(1)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Strip ANSI escape sequences from a string.
/// Handles both raw ESC byte (\x1b[...m) and JSON-encoded form (\\u001b[...m).
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Raw ESC byte: skip until 'm'
            for next in chars.by_ref() {
                if next == 'm' { break; }
            }
        } else if c == '\\' && chars.peek() == Some(&'u') {
            // Check for JSON-escaped \\u001b
            let rest: String = chars.clone().take(5).collect();
            if rest.starts_with("u001b") || rest.starts_with("u001B") {
                // Consume "u001b" (5 chars)
                for _ in 0..5 { chars.next(); }
                // Skip the ANSI parameter sequence until 'm'
                for next in chars.by_ref() {
                    if next == 'm' { break; }
                }
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Find the JSONL file for a given session-id by scanning all project directories.
fn find_jsonl_by_session_id(session_id: &str) -> Option<PathBuf> {
    let projects_dir = dirs::home_dir()?.join(".claude").join("projects");
    let mut best: Option<(PathBuf, u64)> = None;
    for entry in fs::read_dir(&projects_dir).ok()?.flatten() {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            let size = candidate.metadata().ok().map(|m| m.len()).unwrap_or(0);
            if best.as_ref().map_or(true, |(_, s)| size > *s) {
                best = Some((candidate, size));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Find the cwd used by an existing session (by scanning its JSONL for a cwd entry).
/// Used by the resume command to start the tmux session in the right directory.
/// Return session-id → tmux info for all currently live claude sessions.
/// Used by the resume picker to filter out still-running sessions.
pub fn build_live_session_map_public() -> HashMap<String, String> {
    build_live_session_map()
        .into_iter()
        .map(|(id, info)| (id, info.tmux_session))
        .collect()
}

/// Check if a session ID (JSONL-based) is already running in tmux.
/// Returns the pane target (session:window.pane) if found.
pub fn find_live_tmux_for_session(session_id: &str) -> Option<String> {
    let live_map = build_live_session_map();

    // Direct match: PID file's session_id == the one we're looking for.
    if let Some(info) = live_map.get(session_id) {
        return Some(info.pane_target.clone());
    }

    // Resumed session: RECON_RESUMED_FROM env var matches.
    for (_, info) in &live_map {
        if let Some(orig_id) = read_tmux_env(&info.tmux_session, "RECON_RESUMED_FROM") {
            if orig_id == session_id {
                return Some(info.pane_target.clone());
            }
        }
    }

    None
}

pub fn find_session_cwd(session_id: &str) -> Option<String> {
    let projects_dir = dirs::home_dir()?.join(".claude").join("projects");
    for entry in fs::read_dir(&projects_dir).ok()?.flatten() {
        let jsonl_path = entry.path().join(format!("{session_id}.jsonl"));
        if !jsonl_path.exists() {
            continue;
        }
        let file = fs::File::open(&jsonl_path).ok()?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        for _ in 0..20 {
            line.clear();
            match read_line_capped(&mut reader, &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

/// Determine session status from file recency and token counts.
/// - New: no tokens yet (never interacted)
/// - Working: JSONL modified in last 5s
/// - Input: last activity within 10 minutes (active conversation, waiting for user)
/// - Idle: last activity older than 10 minutes
fn determine_status(
    _path: &Path,
    input_tokens: u64,
    output_tokens: u64,
    pane_target: Option<&str>,
    agent: &AgentKind,
) -> SessionStatus {
    // tmux pane content is the source of truth for active sessions
    if let Some(target) = pane_target {
        let pane = match agent {
            AgentKind::Claude => pane_status(target),
            AgentKind::Codex => crate::codex::codex_pane_status(target),
        };
        // Only show New if pane also looks idle (no active streaming)
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

/// Determine status by inspecting the Claude Code TUI pane content.
///
/// Scans the last few non-empty lines bottom-up looking for:
///   - Working: a line starting with a Unicode spinner (✽✢✳✶⏺) that indicates
///     active thinking/tool execution
///   - Input: "Esc to cancel" on the last line, or a selection menu ("❯ N.")
///   - Idle: anything else
fn pane_status(pane_target: &str) -> SessionStatus {
    let output = match std::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_target, "-p"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return SessionStatus::Idle,
    };

    let content = String::from_utf8_lossy(&output.stdout);

    pane_status_from_content(&content)
}

fn pane_status_from_content(content: &str) -> SessionStatus {
    let mut lines_checked = 0;
    let mut background_tasks = None;
    let mut background_agents = None;
    // Tracks whether the line physically below the current one was a wrapped
    // continuation carrying the "…" ellipsis. In a narrow pane Claude's active
    // spinner line ("✽ Task 1: … @CorrelationId…") wraps, landing the spinner on
    // one captured line and the ellipsis on the next. Iterating bottom-up, that
    // continuation is seen one step before the spinner line, so remember it.
    let mut continuation_has_ellipsis = false;

    for line in content.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // A blank line breaks adjacency: the next spinner line above it has
            // no wrapped continuation directly below.
            continuation_has_ellipsis = false;
            continue;
        }

        // Input: "Esc to cancel" is Claude's live prompt hint. It usually sits
        // on the last line, but plugins (e.g. agent-deck) render a task-list
        // panel below the footer, so scan the whole pane. Claude collapses this
        // hint once the prompt is answered, so a stale match won't persist.
        if trimmed.contains("Esc to cancel") {
            return SessionStatus::Input;
        }

        // Background shell tasks are surfaced in Claude's status footer.
        if let Some(count) = background_shell_count(trimmed) {
            background_tasks = Some(count);
        }

        // Claude shows "Waiting for N background agent(s) to finish" (spinner
        // line, no ellipsis) while the main loop is blocked on a spawned
        // subagent. This isn't Idle - the session is actively waiting on work.
        if let Some(count) = background_agent_count(trimmed) {
            background_agents = Some(count);
        }

        // Working: Claude uses spinner-prefixed lines for active progress.
        // Scan the full visible pane, not just the footer: a long todo
        // checklist or output block renders below the spinner line and can
        // push it many lines above the footer.
        if is_claude_working_line(trimmed, continuation_has_ellipsis) {
            return SessionStatus::Working;
        }

        // Input: selection-style permission prompts ("❯ N."). These only
        // appear at the bottom, so only trust them near the footer to avoid
        // matching numbered lines in scrolled-back output. Require the digit
        // to be followed by a dot ("❯ 1. Yes") so a number typed into the
        // input box ("❯ 1") isn't misread as a menu selection.
        if lines_checked < 10 {
            if let Some(pos) = trimmed.find('\u{276F}') { // ❯
                let after = trimmed[pos + '\u{276F}'.len_utf8()..].trim_start();
                let rest = after.trim_start_matches(|c: char| c.is_ascii_digit());
                if rest.len() < after.len() && rest.starts_with('.') {
                    return SessionStatus::Input;
                }
            }
        }

        lines_checked += 1;

        // Record whether this (non-spinner) line is a wrapped continuation
        // carrying the ellipsis, for the spinner line that sits above it.
        continuation_has_ellipsis = trimmed.contains('\u{2026}') && !is_spinner_line(trimmed);
    }

    // Waiting on a subagent is a more specific signal than background shells.
    if let Some(count) = background_agents {
        return SessionStatus::BackgroundAgents(count);
    }

    if let Some(count) = background_tasks {
        return SessionStatus::BackgroundTasks(count);
    }

    SessionStatus::Idle
}

fn background_shell_count(line: &str) -> Option<u32> {
    let is_footer = line.contains("bypass permissions") || line.starts_with('\u{23F5}');
    if !(line.contains("still running") || line.contains("for agents") || is_footer) {
        return None;
    }

    let words: Vec<&str> = line.split_whitespace().collect();
    words.windows(2).find_map(|window| {
        let count = window[0].parse::<u32>().ok()?;
        let label = window[1].trim_matches(|c: char| !c.is_alphanumeric());
        matches!(label, "shell" | "shells").then_some(count)
    })
}

/// Parse the count from Claude's "Waiting for N background agent(s) to finish"
/// line, tolerating a leading spinner char and the singular/plural wording.
fn background_agent_count(line: &str) -> Option<u32> {
    let rest = line.split("Waiting for ").nth(1)?;
    if !rest.contains("background agent") {
        return None;
    }
    rest.split_whitespace().next()?.parse().ok()
}

fn is_claude_working_line(line: &str, continuation_has_ellipsis: bool) -> bool {
    if !is_spinner_line(line) {
        return false;
    }

    // The "…" that marks an in-progress action may sit on this line, or - when a
    // narrow pane wraps it - on the continuation line directly below.
    line.contains('\u{2026}')
        || continuation_has_ellipsis
        || line.contains("Running ") && line.contains(" shell command")
}

/// True if the line begins with a Claude activity spinner character.
fn is_spinner_line(line: &str) -> bool {
    line.chars().next().is_some_and(is_spinner)
}

/// Check if a character is a Claude Code activity indicator.
/// Covers dingbat spinners (✽✢✳✶✻ etc.), record symbol (⏺),
/// and middle dot (·) used for progress lines.
fn is_spinner(c: char) -> bool {
    matches!(c,
        '\u{2720}'..='\u{2767}' | // Dingbats: ✽✢✳✶✻✺✴✵ etc.
        '\u{23FA}'              | // ⏺ (record)
        '\u{00B7}'                // · (middle dot, used for progress)
    )
}

// --- Live session discovery ---

struct SessionFileInfo {
    session_id: String,
    started_at: u64,
}

/// Read ~/.claude/sessions/{PID}.json files to build a PID → session info map.
fn read_pid_session_map() -> HashMap<i32, SessionFileInfo> {
    let sessions_dir = match dirs::home_dir() {
        Some(h) => h.join(".claude").join("sessions"),
        None => return HashMap::new(),
    };

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return HashMap::new(),
    };

    let mut map = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let (Some(pid), Some(sid)) = (
                        v.get("pid").and_then(|p| p.as_i64()),
                        v.get("sessionId").and_then(|s| s.as_str()),
                    ) {
                        let started_at = v
                            .get("startedAt")
                            .and_then(|s| s.as_u64())
                            .unwrap_or(0);
                        map.insert(
                            pid as i32,
                            SessionFileInfo {
                                session_id: sid.to_string(),
                                started_at,
                            },
                        );
                    }
                }
            }
        }
    }
    map
}

/// A discovered tmux pane running a code agent (Claude or Codex).
struct DiscoveredPane {
    pid: i32,
    tmux_session: String,
    pane_target: String,
    pane_cwd: String,
    agent: AgentKind,
    codex_session_id: Option<String>,
}

/// Get tmux panes running code agents (Claude or Codex).
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

    // Evict stale Codex cache entries for PIDs no longer in tmux
    let all_pane_pids: Vec<i32> = stdout
        .lines()
        .filter_map(|l| l.splitn(2, "|||").next()?.parse::<i32>().ok())
        .collect();
    crate::codex::evict_stale_codex_cache(&all_pane_pids);

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

        // Candidate commands cover Claude, Codex, Node wrappers, and Claude's macOS
        // npm binary process name "claude.exe".
        let is_candidate = command
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            || command == "claude"
            || command == "claude.exe"
            || command == "codex"
            || command == "node";

        let is_shell = command == "bash" || command == "sh" || command == "zsh";

        if !is_candidate && !is_shell {
            continue;
        }

        let pane_target = format!("{session_name}:{window_index}.{pane_index}");

        // Try Claude first: direct PID has a session file
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

        // For "node"/"codex" panes, try Codex cache first (instant) before
        // the expensive pgrep-based Claude child search.
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

    results
}

/// Check if a shell process has a claude child by looking for a child PID
/// that has a corresponding ~/.claude/sessions/{PID}.json file.
fn find_claude_child_pid(parent_pid: i32) -> Option<i32> {
    let sessions_dir = dirs::home_dir()?.join(".claude").join("sessions");
    let output = std::process::Command::new("pgrep")
        .args(["-P", &parent_pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .find(|pid| sessions_dir.join(format!("{pid}.json")).exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn read_line_capped_normal() {
        let data = b"hello\nworld\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();

        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0);
        assert_eq!(buf, "hello\n");

        buf.clear();
        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0);
        assert_eq!(buf, "world\n");

        buf.clear();
        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert_eq!(n, 0); // EOF
    }

    #[test]
    fn read_line_capped_no_trailing_newline() {
        let data = b"no newline";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();

        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0);
        assert_eq!(buf, "no newline");
    }

    #[test]
    fn read_line_capped_empty() {
        let data = b"";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();

        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn read_line_capped_overlong_discarded() {
        // Create a line that exceeds MAX_LINE_BYTES, followed by a normal line
        let mut data = vec![b'x'; MAX_LINE_BYTES + 100];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");

        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();

        // First line is overlong — should be discarded
        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0); // consumed bytes, not EOF
        assert!(buf.is_empty()); // but buf is empty

        // Second line should read normally
        buf.clear();
        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0);
        assert_eq!(buf, "ok\n");
    }

    #[test]
    fn read_line_capped_overflow_clears_stale_buf() {
        let mut data = vec![b'x'; MAX_LINE_BYTES + 100];
        data.push(b'\n');

        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::from("stale data");

        let n = read_line_capped(&mut reader, &mut buf).unwrap();
        assert!(n > 0);
        assert!(buf.is_empty()); // stale data cleared
    }

    #[test]
    fn validate_cwd_rejects_relative() {
        assert!(!validate_cwd("relative/path"));
    }

    #[test]
    fn validate_cwd_rejects_nonexistent() {
        assert!(!validate_cwd("/nonexistent/path/that/does/not/exist"));
    }

    #[test]
    fn validate_cwd_accepts_real_dir() {
        assert!(validate_cwd("/tmp"));
    }

    #[test]
    fn claude_pane_status_reports_background_shell_count_from_footer() {
        let content = "\
────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 30.0% | .../work
  ⏵⏵ bypass permissions on · 1 shell · ← for agents
";

        assert_eq!(
            pane_status_from_content(content),
            SessionStatus::BackgroundTasks(1)
        );
    }

    #[test]
    fn claude_pane_status_keeps_working_when_background_shells_also_run() {
        let content = "\
✽ Proving parity and measuring in TEST… (30s · ↓ 1.6k tokens)

────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 31.0% | .../work
  ⏵⏵ bypass permissions on · 2 shells
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn claude_pane_status_reports_running_shell_command_as_working() {
        let content = "\
⏺ Running 1 shell command

────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 31.0% | .../work
  ⏵⏵ bypass permissions on
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn claude_pane_status_reports_background_shell_count_from_cropped_idle_footer() {
        let content = "\
────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 31.0% | .../work
  ⏵⏵ bypass permissions on · 2 shells
";

        assert_eq!(
            pane_status_from_content(content),
            SessionStatus::BackgroundTasks(2)
        );
    }

    #[test]
    fn background_task_status_label_omits_bg_prefix() {
        assert_eq!(SessionStatus::BackgroundTasks(1).label(), "1 task");
        assert_eq!(SessionStatus::BackgroundTasks(4).label(), "4 tasks");
    }

    #[test]
    fn claude_pane_status_reports_plural_background_shell_count() {
        let content = "\
✻ Sautéed for 33s · 4 shells still running

────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 30.0% | .../work
  ⏵⏵ bypass permissions on · 4 shells · ← for agents
";

        assert_eq!(
            pane_status_from_content(content),
            SessionStatus::BackgroundTasks(4)
        );
    }

    #[test]
    fn claude_pane_status_detects_working_above_long_todo_list() {
        // Regression: the spinner line renders above the todo checklist,
        // input box, statusline, and footer, pushing it past the old
        // 10-line bottom-up scan window.
        let content = "\
✶ Contemplating… (7m 29s · ↓ 31.9k tokens)
  └ ✔ Build lever_b/dataverse.py Web API helper
    ✔ Write 01_measure.sql + 02_snapshot_select.sql
    ✔ Write F-008 export script (02_export_predelete.py)
    □ Write cleanup/key/rollback scripts (03/04/05)
    □ Write F-006 runbook README

────────────────────────────────────────────────────────────────
›
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 19.0% | Block: 2hr 16m | (+45,-13) | .../work /rc
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn claude_pane_status_detects_working_when_narrow_pane_wraps_spinner_line() {
        // Regression: in a narrow (35-col) split pane, the active spinner line
        // "✽ Task 1: … @CorrelationId…" wraps, landing the spinner on one
        // captured line and the trailing "…" on the next. Neither half alone
        // satisfied the spinner+ellipsis check, so a working session read Idle.
        let content = "\
     Running…
     … +15 tool uses
     (ctrl+b ctrl+b (twice) to run
     in background)

✽ Task 1: V024 MergeLog fix + drop
  applock + @CorrelationId…

  ⎿  ◼ Task 1: V024 MergeL…
     ◻ Task 2: Error-path …

───────────────────────────────────
❯
───────────────────────────────────
  Fable 5 | Ctx Used: 42...
  ⏵⏵ bypass permissions on  · ← f…

  ⏺ main
  ◯ artifact-writer
  ◯ general-purpose 3m 7s · ↓ 76.1k
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn claude_pane_status_reports_waiting_background_agent() {
        // Claude blocks the main loop on a spawned subagent with a spinner line
        // "✻ Waiting for 1 background agent to finish" - no ellipsis, so it isn't
        // a working line, but the session is not Idle either.
        let content = "\
⏺ Resumed successfully with foreground-only instructions.

✻ Waiting for 1 background agent to finish

──────────────────────────────────────── Validate WDP bug report points ──
❯ did it finish?
──────────────────────────────────────────────────────────────────────────
  Fable 5 | Ctx Used: 54.0% | Block: 1hr 59m | (+0,-0) | .../fabric   /rc
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents
";

        assert_eq!(
            pane_status_from_content(content),
            SessionStatus::BackgroundAgents(1)
        );
    }

    #[test]
    fn claude_pane_status_reports_plural_background_agents() {
        let content = "\
✻ Waiting for 3 background agents to finish

────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 20.0% | .../work
  ⏵⏵ bypass permissions on · ← for agents
";

        assert_eq!(
            pane_status_from_content(content),
            SessionStatus::BackgroundAgents(3)
        );
    }

    #[test]
    fn claude_pane_status_ignores_completed_shell_command_summary() {
        let content = "\
  Ran 1 shell command

────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 30.0% | .../work
  ⏵⏵ bypass permissions on · ← for agents
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn claude_pane_status_keeps_working_when_number_typed_in_input_box() {
        // Regression: a number typed into the input box renders as "❯ 1",
        // which used to be misread as a selection menu ("❯ N.") and reported
        // Input even though the agent is actively working above it.
        let content = "\
✳ Metamorphosing… (6m 33s · ↓ 20.4k tokens)
  └ Tip: Use Plan Mode to prepare for a complex request before making changes.

────────────────────────────────────────────────────────────────
❯ 1
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 14.0% | Block: 1hr 47m | (+0,-0) | .../work /rc
  ⏵⏵ bypass permissions on (shift+tab to cycle)
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn claude_pane_status_idle_when_number_typed_in_input_box_after_done() {
        // Regression: leftover number in the input box ("❯ 1") must not keep
        // a finished session pinned to Input.
        let content = "\
✳ Sautéed for 7m 43s

────────────────────────────────────────────────────────────────
❯ 1
────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 15.0% | Block: 1hr 48m | (+0,-0) | .../work /rc
  ⏵⏵ bypass permissions on (shift+tab to cycle)
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn claude_pane_status_detects_selection_menu_with_dot() {
        // A genuine selection menu renders the choice as "❯ 1. Yes" - the
        // digit is followed by a dot. This must still report Input.
        let content = "\
Do you want to proceed?
❯ 1. Yes
  2. No

────────────────────────────────────────────────────────────────
  Opus 4.8 | Ctx Used: 30.0% | .../work
  ⏵⏵ bypass permissions on
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Input);
    }

    #[test]
    fn claude_pane_status_detects_input_under_trailing_task_panel() {
        // Regression: the agent-deck plugin renders a task-list panel below
        // Claude's prompt footer, pushing "Esc to cancel" several lines above
        // the bottom. This must still report Input, not Idle.
        let content = "\
❯ 1. You advise on rn_pdl
     You tell me the intended fix.
  2. I fix both minimally
  3. Pause after Task 1
  4. Type something.
────────────────────────────────────────────────────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel

  7 tasks (0 done, 1 in progress, 6 open)
  ◼ Task 1: Capture table, view, purge, harness wiring (V014)
  ◻ Task 2: Capture in sp_AddSkillByJobTitle (skill)
  ◻ Task 3: Capture in sp_AddSkillByLinkedinSkill (workoskill)
  ◻ Task 4: Capture in sp_Run_JobRole_Assignment (jobrole+scoring)
  ◻ Task 5: Capture in sp_SetEmploymentJobRoleByTitle (ce grain)
   … +2 pending
";

        assert_eq!(pane_status_from_content(content), SessionStatus::Input);
    }
}
