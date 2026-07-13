# Session Limit Scheduled Continue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect Claude Code session-limit panes and let `c` schedule one `continue` plus Enter at the displayed reset deadline.

**Architecture:** A focused `session_limit` module parses Claude's reset marker into a timezone-aware absolute deadline. Session discovery combines that status with immutable tmux pane IDs and pane-local scheduling metadata, while `tmux.rs` owns a validated tmux background job that survives recon exiting.

**Tech Stack:** Rust 2021, chrono, chrono-tz, tmux, ratatui, crossterm, built-in Rust unit tests.

## Global Constraints

- Recognize only Claude Code's complete `You've hit your session limit · resets <time> (<timezone>)` marker.
- Display `Limit H:MM` before scheduling and `Queued H:MM` after scheduling.
- Use `c` in table view and on the selected agent in a zoomed Tamagotchi room.
- Send literal `continue`, then Enter, exactly once.
- The timer must survive recon exiting.
- Validate immutable tmux pane IDs before interpolating them into a shell command.
- Do not include limited panes in `i` or Tab input navigation.
- Add comments above every new logical code block.
- Write each behavior test first and observe the expected failure before implementation.

---

### Task 1: Parse timezone-aware Claude reset deadlines

**Files:**
- Create: `src/session_limit.rs`
- Modify: `src/main.rs:1-12`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**
- Produces: `SessionLimit`, `parse_session_limit(line, reference)`, `SessionLimit::label_time()`, `SessionLimit::seconds_until(now)`.
- Consumes: `chrono::DateTime<Utc>` and `chrono_tz::Tz`.

- [ ] **Step 1: Add the dependency and failing parser tests**

Run:

```bash
cargo add chrono-tz
```

Add `mod session_limit;` beside the other module declarations in `src/main.rs`. Create `src/session_limit.rs` with tests for the exact live marker, PM time, midnight rollover, elapsed deadlines, and malformed input:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn parses_live_session_limit_marker() {
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
        let limit = parse_session_limit(
            "⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();

        assert_eq!(limit.label_time(), "1:10");
        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 22, 10, 0).unwrap()
        );
    }

    #[test]
    fn parses_pm_reset_time() {
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 8, 0, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10pm (America/New_York)",
            reference,
        )
        .unwrap();

        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 17, 10, 0).unwrap()
        );
    }

    #[test]
    fn resolves_after_midnight_against_event_time() {
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 30, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();

        assert_eq!(
            limit.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 13, 22, 10, 0).unwrap()
        );
    }

    #[test]
    fn elapsed_deadline_has_zero_wait() {
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
        let limit = parse_session_limit(
            "You've hit your session limit · resets 1:10am (Asia/Nicosia)",
            reference,
        )
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 13, 22, 11, 0).unwrap();

        assert_eq!(limit.seconds_until(now), 0);
    }

    #[test]
    fn rejects_unrelated_or_malformed_limit_text() {
        let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();

        assert!(parse_session_limit("You have 4 usage limit resets available", reference).is_none());
        assert!(parse_session_limit(
            "You've hit your session limit · resets soon (Asia/Nicosia)",
            reference
        )
        .is_none());
        assert!(parse_session_limit(
            "You've hit your session limit · resets 1:10am (Not/AZone)",
            reference
        )
        .is_none());
    }
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test session_limit::tests -- --nocapture
```

Expected: compilation fails because `parse_session_limit` and `SessionLimit` do not exist.

- [ ] **Step 3: Implement the parser and deadline model**

Add the following production code above the tests in `src/session_limit.rs`:

```rust
use chrono::{DateTime, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

const SESSION_LIMIT_MARKER: &str = "You've hit your session limit · resets ";

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLimit {
    pub reset_at: DateTime<Utc>,
    pub reset_time: NaiveTime,
    pub timezone: Tz,
}

impl SessionLimit {
    pub fn label_time(&self) -> String {
        let hour = match self.reset_time.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        format!("{hour}:{:02}", self.reset_time.minute())
    }

    pub fn seconds_until(&self, now: DateTime<Utc>) -> u64 {
        (self.reset_at - now).num_seconds().max(0) as u64
    }
}

pub fn parse_session_limit(line: &str, reference: DateTime<Utc>) -> Option<SessionLimit> {
    let suffix = line.split_once(SESSION_LIMIT_MARKER)?.1;
    let (clock, timezone) = suffix.split_once(" (")?;
    let timezone = timezone.strip_suffix(')')?.parse::<Tz>().ok()?;
    let reset_time = parse_clock(clock)?;
    let reset_at = resolve_reset_at(reset_time, timezone, reference)?;

    Some(SessionLimit {
        reset_at,
        reset_time,
        timezone,
    })
}

fn parse_clock(clock: &str) -> Option<NaiveTime> {
    let clock = clock.to_ascii_lowercase();
    let (clock, is_pm) = if let Some(value) = clock.strip_suffix("am") {
        (value, false)
    } else {
        (clock.strip_suffix("pm")?, true)
    };
    let (hour, minute) = clock.split_once(':')?;
    let hour = hour.parse::<u32>().ok()?;
    let minute = minute.parse::<u32>().ok()?;
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }
    let hour = (hour % 12) + if is_pm { 12 } else { 0 };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn resolve_reset_at(
    reset_time: NaiveTime,
    timezone: Tz,
    reference: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let reference = reference.with_timezone(&timezone);
    let mut date = reference.date_naive();
    let mut candidate = timezone.from_local_datetime(&date.and_time(reset_time)).earliest()?;

    if candidate <= reference {
        date = date.succ_opt()?;
        candidate = timezone.from_local_datetime(&date.and_time(reset_time)).earliest()?;
    }

    Some(candidate.with_timezone(&Utc))
}
```

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test session_limit::tests -- --nocapture
```

Expected: all five parser tests pass.

- [ ] **Step 5: Commit the parser**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/session_limit.rs
git commit -m "feat: parse Claude session reset deadlines"
```

---

### Task 2: Add limited and queued pane statuses

**Files:**
- Modify: `src/session.rs:72-125`
- Modify: `src/session.rs:290-518`
- Modify: `src/session.rs:538-613`
- Modify: `src/session.rs:1173-1358`
- Modify: `src/session.rs:1408-1525`
- Test: `src/session.rs` test module

**Interfaces:**
- Consumes: `session_limit::parse_session_limit` and `SessionLimit` from Task 1.
- Produces: `SessionStatus::Limited(SessionLimit)`, `SessionStatus::ContinueScheduled(SessionLimit)`, `Session::pane_id`, and queued-state discovery from `@recon_continue_at`.

- [ ] **Step 1: Write failing status-priority and queued-state tests**

Add tests using a fixed event timestamp:

```rust
#[test]
fn claude_pane_status_reports_session_limit() {
    let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
    let content = "\
⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)
   /upgrade to increase your usage limit.
";

    assert_eq!(
        pane_status_from_content_at(content, reference, None).label(),
        "Limit 1:10"
    );
}

#[test]
fn claude_pane_status_reports_matching_scheduled_continue() {
    let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
    let reset_at = Utc.with_ymd_and_hms(2026, 7, 13, 22, 10, 0).unwrap();
    let content = "⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)";

    assert_eq!(
        pane_status_from_content_at(content, reference, Some(reset_at.timestamp())).label(),
        "Queued 1:10"
    );
}

#[test]
fn active_working_signal_overrides_stale_session_limit() {
    let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
    let content = "\
✽ Continuing implementation…
⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)
";

    assert_eq!(
        pane_status_from_content_at(content, reference, None),
        SessionStatus::Working
    );
}

#[test]
fn active_input_signal_overrides_stale_session_limit() {
    let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();
    let content = "\
Esc to cancel
⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)
";

    assert_eq!(
        pane_status_from_content_at(content, reference, None),
        SessionStatus::Input
    );
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test session::tests -- --nocapture
```

Expected: compilation fails because the new helper and status variants do not exist.

- [ ] **Step 3: Add status variants and labels**

Import `chrono::{DateTime, Utc}` and `crate::session_limit::{parse_session_limit, SessionLimit}`. Extend `SessionStatus` and its label implementation:

```rust
pub enum SessionStatus {
    New,
    Working,
    Idle,
    Input,
    BackgroundTasks(u32),
    BackgroundAgents(u32),
    Limited(SessionLimit),
    ContinueScheduled(SessionLimit),
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
            SessionStatus::Limited(limit) => format!("Limit {}", limit.label_time()),
            SessionStatus::ContinueScheduled(limit) => {
                format!("Queued {}", limit.label_time())
            }
        }
    }
}
```

- [ ] **Step 4: Refactor Claude pane parsing to enforce signal priority**

Change `pane_status` to accept the event reference and scheduled timestamp. Keep the existing `pane_status_from_content` wrapper for current tests, and add the deterministic helper:

```rust
fn pane_status(
    pane_target: &str,
    reference: DateTime<Utc>,
    scheduled_continue_at: Option<i64>,
) -> SessionStatus {
    let output = match std::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_target, "-p"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return SessionStatus::Idle,
    };

    let content = String::from_utf8_lossy(&output.stdout);
    pane_status_from_content_at(&content, reference, scheduled_continue_at)
}

fn pane_status_from_content(content: &str) -> SessionStatus {
    pane_status_from_content_at(content, Utc::now(), None)
}
```

In `pane_status_from_content_at`, retain the existing background and wrapped-line parsing, but accumulate `needs_input`, `is_working`, and `session_limit` instead of returning early. Resolve in this exact order after the loop:

```rust
if needs_input {
    return SessionStatus::Input;
}

if is_working {
    return SessionStatus::Working;
}

if let Some(limit) = session_limit {
    if scheduled_continue_at == Some(limit.reset_at.timestamp()) {
        return SessionStatus::ContinueScheduled(limit);
    }
    return SessionStatus::Limited(limit);
}

if let Some(count) = background_agents {
    return SessionStatus::BackgroundAgents(count);
}

if let Some(count) = background_tasks {
    return SessionStatus::BackgroundTasks(count);
}

SessionStatus::Idle
```

Set `session_limit` inside the loop with `parse_session_limit(trimmed, reference)`. Set the two booleans where the current function returns Input or Working.

- [ ] **Step 5: Carry pane IDs and scheduling timestamps through discovery**

Add these fields to `DiscoveredPane` and `LiveSessionInfo`:

```rust
pane_id: String,
scheduled_continue_at: Option<i64>,
```

Add this field to `Session`:

```rust
pub pane_id: Option<String>,
```

Extend the tmux format and parser:

```rust
"#{pane_pid}|||#{session_name}|||#{pane_current_command}|||#{pane_current_path}|||#{window_index}|||#{pane_index}|||#{pane_id}|||#{@recon_continue_at}"
```

Use `line.splitn(8, "|||")`, require eight parts, assign `parts[6]` to `pane_id`, and parse `parts[7]` as `Option<i64>`. Propagate both values from `DiscoveredPane` to `LiveSessionInfo` in all three agent branches. Set `pane_id: Some(live.pane_id.clone())` in all four live `Session` constructors and `pane_id: None` in test-only constructors.

Pass `live.scheduled_continue_at` to every `determine_status` call. Update `determine_status` to derive the event reference from the JSONL modification time and pass it to Claude parsing:

```rust
let reference = path
    .metadata()
    .and_then(|metadata| metadata.modified())
    .map(DateTime::<Utc>::from)
    .unwrap_or_else(|_| Utc::now());

let pane = match agent {
    AgentKind::Claude => pane_status(target, reference, scheduled_continue_at),
    AgentKind::Codex => crate::codex::codex_pane_status(target),
};
```

Codex continues to ignore the scheduling timestamp.

- [ ] **Step 6: Run focused and complete session tests**

Run:

```bash
cargo test session::tests -- --nocapture
```

Expected: all existing and new session tests pass.

- [ ] **Step 7: Commit status discovery**

```bash
git add src/session.rs src/app.rs src/view_ui.rs
git commit -m "feat: show Claude session limits in pane status"
```

---

### Task 3: Schedule one tmux-owned continuation

**Files:**
- Modify: `src/tmux.rs`
- Test: `src/tmux.rs` test module

**Interfaces:**
- Consumes: immutable pane ID and `SessionLimit::reset_at`.
- Produces: `tmux::schedule_continue(pane_id, reset_at) -> Result<(), String>`.

- [ ] **Step 1: Write failing validation and command tests**

```rust
#[test]
fn validates_immutable_pane_ids() {
    assert!(validate_pane_id("%42"));
    assert!(!validate_pane_id("W:1.0"));
    assert!(!validate_pane_id("%42; touch /tmp/pwned"));
    assert!(!validate_pane_id("%"));
}

#[test]
fn scheduled_continue_command_sends_literal_text_then_enter() {
    assert_eq!(
        scheduled_continue_command("%42", 90),
        "sleep 90; if tmux display-message -p -t %42 '#{pane_id}' >/dev/null 2>&1; then tmux send-keys -t %42 -l continue; tmux send-keys -t %42 Enter; fi; tmux set-option -p -u -t %42 @recon_continue_at 2>/dev/null || true"
    );
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test tmux::tests -- --nocapture
```

Expected: compilation fails because both helpers are missing.

- [ ] **Step 3: Implement validated background scheduling**

Add imports for `chrono::{DateTime, Utc}`. Implement:

```rust
const CONTINUE_OPTION: &str = "@recon_continue_at";

pub fn schedule_continue(pane_id: &str, reset_at: DateTime<Utc>) -> Result<(), String> {
    if !validate_pane_id(pane_id) {
        return Err(format!("Invalid tmux pane ID: {pane_id}"));
    }

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
            return Err(format!("Pane already has a different scheduled continuation: {existing}"));
        }
    }

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
    pane_id
        .strip_prefix('%')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|char| char.is_ascii_digit()))
}

fn scheduled_continue_command(pane_id: &str, delay: u64) -> String {
    format!(
        "sleep {delay}; if tmux display-message -p -t {pane_id} '#{{pane_id}}' >/dev/null 2>&1; then tmux send-keys -t {pane_id} -l continue; tmux send-keys -t {pane_id} Enter; fi; tmux set-option -p -u -t {pane_id} {CONTINUE_OPTION} 2>/dev/null || true"
    )
}

fn clear_scheduled_continue(pane_id: &str) {
    let _ = Command::new("tmux")
        .args(["set-option", "-p", "-u", "-t", pane_id, CONTINUE_OPTION])
        .output();
}
```

- [ ] **Step 4: Run tmux unit tests and verify GREEN**

Run:

```bash
cargo test tmux::tests -- --nocapture
```

Expected: all tmux tests pass.

- [ ] **Step 5: Commit scheduling**

```bash
git add src/tmux.rs
git commit -m "feat: schedule tmux continuation at session reset"
```

---

### Task 4: Bind `c` and render attention states

**Files:**
- Modify: `src/app.rs:209-385`
- Modify: `src/ui.rs:75-100`
- Modify: `src/ui.rs:362-400`
- Modify: `src/view_ui.rs:190-200`
- Modify: `src/view_ui.rs:305-331`
- Modify: `src/view_ui.rs:630-667`
- Modify: `README.md:15-140`
- Test: `src/app.rs` test module

**Interfaces:**
- Consumes: `tmux::schedule_continue`, `Session::pane_id`, and both session-limit status variants.
- Produces: table and zoom-view `c` behavior plus red limited/queued rendering.

- [ ] **Step 1: Write failing app scheduling tests**

Import `chrono::TimeZone` and `crate::session::SessionStatus` in the test module. Extend the test session helper with `pane_id: Some("%42".to_string())`. Add a helper that constructs a deterministic limit and these tests:

```rust
fn make_limit() -> crate::session_limit::SessionLimit {
    crate::session_limit::parse_session_limit(
        "You've hit your session limit · resets 1:10am (Asia/Nicosia)",
        chrono::Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap(),
    )
    .unwrap()
}

#[test]
fn scheduling_selected_limited_session_marks_it_queued() {
    let mut app = App::new();
    let limit = make_limit();
    let deadline = limit.reset_at;
    let mut session = make_session("limited");
    session.status = SessionStatus::Limited(limit);
    app.sessions = vec![session];
    let called = std::cell::Cell::new(false);

    app.schedule_continue_for_with(0, |pane_id, reset_at| {
        called.set(true);
        assert_eq!(pane_id, "%42");
        assert_eq!(reset_at, deadline);
        Ok(())
    });

    assert!(called.get());
    assert!(matches!(
        app.sessions[0].status,
        SessionStatus::ContinueScheduled(_)
    ));
}

#[test]
fn scheduling_queued_session_does_not_duplicate_timer() {
    let mut app = App::new();
    let mut session = make_session("queued");
    session.status = SessionStatus::ContinueScheduled(make_limit());
    app.sessions = vec![session];
    let called = std::cell::Cell::new(false);

    app.schedule_continue_for_with(0, |_, _| {
        called.set(true);
        Ok(())
    });

    assert!(!called.get());
}
```

- [ ] **Step 2: Run focused app tests and verify RED**

Run:

```bash
cargo test app::tests -- --nocapture
```

Expected: compilation fails because `schedule_continue_for_with` is missing.

- [ ] **Step 3: Implement the scheduling action and keybindings**

Add `SessionStatus` to the existing `crate::session` imports. Add a selected zoomed-session index helper and make the existing reference helper delegate to it:

```rust
fn selected_zoomed_session_index(&self) -> Option<usize> {
    let indices = self.zoomed_room_session_indices();
    if indices.is_empty() {
        return None;
    }
    Some(indices[self.view_selected_agent.min(indices.len() - 1)])
}

fn selected_zoomed_session(&self) -> Option<&Session> {
    self.selected_zoomed_session_index()
        .and_then(|index| self.sessions.get(index))
}
```

Add the testable action:

```rust
fn schedule_continue_for_with<F>(&mut self, real_idx: usize, schedule: F)
where
    F: FnOnce(&str, chrono::DateTime<chrono::Utc>) -> Result<(), String>,
{
    let Some(session) = self.sessions.get(real_idx) else {
        return;
    };
    let (Some(pane_id), SessionStatus::Limited(limit)) =
        (session.pane_id.clone(), session.status.clone())
    else {
        return;
    };

    if schedule(&pane_id, limit.reset_at).is_ok() {
        self.sessions[real_idx].status = SessionStatus::ContinueScheduled(limit);
    }
}

fn schedule_continue_for(&mut self, real_idx: usize) {
    self.schedule_continue_for_with(real_idx, tmux::schedule_continue);
}
```

In table view, map `KeyCode::Char('c')` to `resolve_selected()` followed by `schedule_continue_for`. In the zoomed-room key block, map `c` to `selected_zoomed_session_index()` followed by the same action, then return.

- [ ] **Step 4: Render and document the new states**

In `ui.rs`, map both variants to a red dot:

```rust
SessionStatus::Limited(_) | SessionStatus::ContinueScheduled(_) => ("●", Color::Red),
```

Add `c continue` to the table footer. In `view_ui.rs`, use the Input sprite for both variants, keep animation frame zero, map both colors to red, and add `c continue` to the zoomed footer only.

In `README.md`, add Limit and Queued to the state table and status explanation. Add `c` to both keybinding tables with the text `Schedule continue at session reset`.

- [ ] **Step 5: Run app and UI tests**

Run:

```bash
cargo test app::tests -- --nocapture
cargo test view_ui::tests -- --nocapture
```

Expected: all app and view tests pass.

- [ ] **Step 6: Commit dashboard behavior**

```bash
git add src/app.rs src/ui.rs src/view_ui.rs README.md
git commit -m "feat: bind scheduled continue for limited sessions"
```

---

### Task 5: Verify the complete feature

**Files:**
- Modify only if verification exposes a defect in the files above.

**Interfaces:**
- Consumes: the completed feature from Tasks 1 through 4.
- Produces: formatting, test, lint, and live read-only evidence.

- [ ] **Step 1: Format and verify formatting**

Run:

```bash
cargo fmt
cargo fmt --check
```

Expected: `cargo fmt --check` exits successfully with no output.

- [ ] **Step 2: Run the complete test suite**

Run:

```bash
cargo test
```

Expected: all unit tests pass with zero failures.

- [ ] **Step 3: Run lint checks**

Run:

```bash
cargo clippy --all-targets -- -D warnings
```

Expected: clippy exits successfully with no warnings.

- [ ] **Step 4: Verify live limited panes read-only**

Run:

```bash
cargo run --quiet -- json | jq -r '.sessions[] | select(.status | startswith("Limit ") or startswith("Queued ")) | [.pane_target, .status] | @tsv'
```

Expected: the currently limited Claude panes appear with `Limit 1:10` or `Queued 1:10`. Do not press `c` against a real pane during automated verification.

- [ ] **Step 5: Review the final diff and commit formatting fixes if needed**

Run:

```bash
git diff --check
git status --short
git log -5 --oneline
```

Expected: no uncommitted files remain and the feature is represented by the task commits.
