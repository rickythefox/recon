use std::process::Command;

use chrono::{DateTime, Utc};

use crate::session;

const CONTINUE_OPTION: &str = "@recon_continue_at";

/// Switch to a tmux pane (inside tmux) or attach to its session (outside tmux).
/// `target` is a pane target like "mywork:0.0" (session:window.pane).
pub fn switch_to_pane(target: &str) {
    let inside_tmux = std::env::var("TMUX").is_ok();
    if inside_tmux {
        let _ = Command::new("tmux")
            .args(["switch-client", "-t", target])
            .status();
    } else {
        let _ = Command::new("tmux")
            .args(["attach-session", "-t", target])
            .status();
    }
}

/// Zoom the window containing `target` if it isn't already zoomed.
/// `resize-pane -Z` toggles zoom, so we first query the window's zoom flag to
/// avoid un-zooming an already-zoomed window.
pub fn zoom_pane(target: &str) {
    // Query whether the target's window is already zoomed (1) or not (0).
    let zoomed = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, "#{window_zoomed_flag}"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false);

    // Only toggle when not already zoomed.
    if !zoomed {
        let _ = Command::new("tmux")
            .args(["resize-pane", "-Z", "-t", target])
            .status();
    }
}

/// Launch a command in a new tmux session with the given name and working directory.
/// If `command` is None, runs claude. Otherwise splits the command on whitespace
/// and passes the parts as the binary + args to tmux (no shell wrapper, so aliases
/// won't resolve — use full paths).
/// Returns the session name on success.
pub fn create_session(name: &str, cwd: &str, command: Option<&str>, tags: &[String], agent: &crate::session::AgentKind) -> Result<String, String> {
    if !session::validate_cwd(cwd) {
        return Err(format!("Invalid working directory: {cwd}"));
    }

    let base_name = sanitize_session_name(name);
    let session_name = unique_session_name(&base_name);

    let mut tmux_args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        session_name.clone(),
        "-c".to_string(),
        cwd.to_string(),
    ];

    if !tags.is_empty() {
        let tags_val = tags.join(",");
        tmux_args.push("-e".to_string());
        tmux_args.push(format!("RECON_TAGS={tags_val}"));
    }

    match command {
        Some(cmd) => {
            for part in cmd.split_whitespace() {
                tmux_args.push(part.to_string());
            }
        }
        None => {
            let bin = match agent {
                crate::session::AgentKind::Claude => which_claude().unwrap_or_else(|| "claude".to_string()),
                crate::session::AgentKind::Codex => which_codex().unwrap_or_else(|| "codex".to_string()),
            };
            tmux_args.push(bin);
        }
    }

    let status = Command::new("tmux")
        .args(&tmux_args)
        .status()
        .map_err(|e| format!("Failed to create tmux session: {e}"))?;

    if !status.success() {
        return Err("tmux new-session failed".to_string());
    }

    Ok(session_name)
}

/// Resume a session in a new tmux session. Dispatches to Claude or Codex.
/// No-op if the session is already running — returns the existing tmux name.
pub fn resume_session(session_id: &str, name: Option<&str>, agent: &crate::session::AgentKind) -> Result<String, String> {
    if let Some(existing) = session::find_live_tmux_for_session(session_id) {
        return Ok(existing);
    }

    let tmux_name = name
        .map(|n| n.to_string())
        .unwrap_or_else(|| session_id[..6.min(session_id.len())].to_string());

    // Use the original session's cwd so we start in the right project directory.
    let cwd = match agent {
        crate::session::AgentKind::Claude => {
            session::find_session_cwd(session_id).filter(|c| session::validate_cwd(c))
        }
        crate::session::AgentKind::Codex => {
            crate::codex::find_codex_session_cwd(session_id).filter(|c| session::validate_cwd(c))
        }
    }
    .or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).ok())
    .unwrap_or_else(|| ".".to_string());

    let base_name = sanitize_session_name(&tmux_name);
    let session_name = unique_session_name(&base_name);

    // Build agent-specific command
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

    // Store the original session-id in the tmux session environment so recon can
    // find the right JSONL without parsing process command lines.
    let env_var = format!("RECON_RESUMED_FROM={session_id}");
    let mut tmux_cmd_args = vec![
        "new-session".to_string(), "-d".to_string(),
        "-s".to_string(), session_name.clone(),
        "-c".to_string(), cwd,
        "-e".to_string(), env_var, bin,
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
}

/// Get default session name and cwd for a new session.
pub fn default_new_session_info() -> (String, String) {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let name = std::path::Path::new(&cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "claude".to_string());

    (name, cwd)
}

fn unique_session_name(base_name: &str) -> String {
    if !session_exists(base_name) {
        return base_name.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base_name}-{n}");
        if !session_exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn which_claude() -> Option<String> {
    let output = Command::new("which").arg("claude").output().ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn which_codex() -> Option<String> {
    let output = Command::new("which").arg("codex").output().ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Schedule one literal `continue` and Enter in a tmux-owned background job.
pub fn schedule_continue(pane_id: &str, reset_at: DateTime<Utc>) -> Result<(), String> {
    // Refuse any target that could alter the generated shell command.
    if !validate_pane_id(pane_id) {
        return Err(format!("Invalid tmux pane ID: {pane_id}"));
    }

    // Treat the same deadline as an idempotent request and reject conflicts.
    let timestamp = reset_at.timestamp().to_string();
    let existing = Command::new("tmux")
        .args(["show-options", "-p", "-v", "-t", pane_id, CONTINUE_OPTION])
        .output()
        .map_err(|error| format!("Failed to inspect scheduled continuation: {error}"))?;
    if existing.status.success() {
        let existing = String::from_utf8_lossy(&existing.stdout).trim().to_string();
        if existing == timestamp {
            return Ok(());
        }
        if !existing.is_empty() {
            return Err(format!(
                "Pane already has a different scheduled continuation: {existing}"
            ));
        }
    }

    // Persist the deadline on the pane before launching the background job.
    let set_option = Command::new("tmux")
        .args([
            "set-option",
            "-p",
            "-t",
            pane_id,
            CONTINUE_OPTION,
            &timestamp,
        ])
        .output()
        .map_err(|error| format!("Failed to mark scheduled continuation: {error}"))?;
    if !set_option.status.success() {
        return Err("tmux set-option failed".to_string());
    }

    // Delegate the wait to tmux so it survives recon exiting.
    let delay = (reset_at - Utc::now()).num_seconds().max(0) as u64;
    let command = scheduled_continue_command(pane_id, delay);
    let run_shell = match Command::new("tmux")
        .args(["run-shell", "-b", "-t", pane_id, &command])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            clear_scheduled_continue(pane_id);
            return Err(format!("Failed to schedule continuation: {error}"));
        }
    };
    if !run_shell.status.success() {
        clear_scheduled_continue(pane_id);
        return Err("tmux run-shell failed".to_string());
    }

    Ok(())
}

fn validate_pane_id(pane_id: &str) -> bool {
    // Tmux pane IDs are an immutable percent sign followed by decimal digits.
    pane_id.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.chars().all(|char| char.is_ascii_digit())
    })
}

fn scheduled_continue_command(pane_id: &str, delay: u64) -> String {
    // Verify the pane still exists, send literal text, send Enter, then clear the marker.
    format!(
        "sleep {delay}; if tmux display-message -p -t {pane_id} '#{{pane_id}}' >/dev/null 2>&1; then tmux send-keys -t {pane_id} -l continue; tmux send-keys -t {pane_id} Enter; fi; tmux set-option -p -u -t {pane_id} {CONTINUE_OPTION} 2>/dev/null || true"
    )
}

fn clear_scheduled_continue(pane_id: &str) {
    // Best-effort cleanup restores Limit status after a scheduling failure.
    let _ = Command::new("tmux")
        .args(["set-option", "-p", "-u", "-t", pane_id, CONTINUE_OPTION])
        .output();
}

/// Kill a tmux session by name.
pub fn kill_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Sanitize a string for use as a tmux session name.
/// Uses an allowlist (alphanumeric, `-`, `_`) to prevent injection via
/// crafted directory names. Leading dashes are stripped to avoid flag injection.
fn sanitize_session_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = sanitized.trim_start_matches('-');

    if trimmed.is_empty() {
        "claude".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_immutable_pane_ids() {
        // Only tmux's percent-prefixed numeric pane IDs are safe to interpolate.
        assert!(validate_pane_id("%42"));
        assert!(!validate_pane_id("W:1.0"));
        assert!(!validate_pane_id("%42; touch /tmp/pwned"));
        assert!(!validate_pane_id("%"));
    }

    #[test]
    fn scheduled_continue_command_sends_literal_text_then_enter() {
        // Literal mode prevents tmux from interpreting continue as a key name.
        assert_eq!(
            scheduled_continue_command("%42", 90),
            "sleep 90; if tmux display-message -p -t %42 '#{pane_id}' >/dev/null 2>&1; then tmux send-keys -t %42 -l continue; tmux send-keys -t %42 Enter; fi; tmux set-option -p -u -t %42 @recon_continue_at 2>/dev/null || true"
        );
    }

    #[test]
    fn sanitize_normal_name() {
        assert_eq!(sanitize_session_name("my-project"), "my-project");
        assert_eq!(sanitize_session_name("foo_bar"), "foo_bar");
    }

    #[test]
    fn sanitize_dots_and_colons() {
        assert_eq!(sanitize_session_name("my.project:1"), "my-project-1");
    }

    #[test]
    fn sanitize_shell_metacharacters() {
        assert_eq!(sanitize_session_name("$HOME;rm -rf /"), "HOME-rm--rf--");
    }

    #[test]
    fn sanitize_control_chars() {
        assert_eq!(sanitize_session_name("hello\x00\x1bworld"), "hello--world");
    }

    #[test]
    fn sanitize_leading_dashes_stripped() {
        assert_eq!(sanitize_session_name("--flag"), "flag");
        assert_eq!(sanitize_session_name("...name"), "name");
    }

    #[test]
    fn sanitize_all_special_becomes_claude() {
        assert_eq!(sanitize_session_name("..."), "claude");
        assert_eq!(sanitize_session_name(""), "claude");
    }

    #[test]
    fn sanitize_unicode_preserved() {
        assert_eq!(sanitize_session_name("café"), "café");
    }
}
