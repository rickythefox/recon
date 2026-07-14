# Visible Claude Pane Title Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make recon display the title of the Claude session currently visible in each tmux pane, with JSONL metadata as the fallback.

**Architecture:** Refactor Claude pane inspection to return status and an optional visible prompt-border title from one `tmux capture-pane` call. Session discovery uses the visible title only as a display override, leaving the parent session identity and all other metadata unchanged.

**Tech Stack:** Rust 2021, tmux, chrono, serde, built-in Rust unit tests.

## Global Constraints

- A visible Claude prompt-border title is authoritative for display.
- Fall back to the parsed Claude JSONL title when no recognizable visible title exists.
- Keep session ID, JSONL ownership, tokens, model, status, activity, and tmux targeting unchanged.
- Keep Codex title behavior unchanged.
- Use one `tmux capture-pane` call per Claude pane refresh.
- Add comments above every new logical code block.
- Write each behavior test first and observe the expected failure before implementation.

---

### Task 1: Inspect Claude status and visible title together

**Files:**
- Modify: `src/session.rs:300-550`
- Modify: `src/session.rs:1210-1390`
- Test: `src/session.rs:2050-2205`

**Interfaces:**
- Consumes: Claude pane text, session-limit reference time, queued-continuation time, and the JSONL title.
- Produces: `PaneInspection { status: SessionStatus, visible_title: Option<String> }`, `inspect_claude_pane(...)`, `inspect_claude_pane_content_at(...)`, and `prefer_visible_title(...)`.

- [ ] **Step 1: Add failing pure regression tests**

Add these beside the existing Claude pane-status tests:

```rust
#[test]
fn claude_pane_inspection_reads_visible_custom_title_and_status() {
    // The foregrounded agent title is rendered immediately above the prompt.
    let content = "\
⎿  You've hit your session limit · resets 1:10am (Asia/Nicosia)
──────────────────────── validate-and-fix-victor-report-points ──
❯
────────────────────────────────────────────────────────────────
  Fable 5 | Ctx Used: 55.0% | .../worko/fabric-data-platform
  ⏵⏵ bypass permissions on · ← for agents
";
    let reference = Utc.with_ymd_and_hms(2026, 7, 13, 20, 0, 0).unwrap();

    let inspection = inspect_claude_pane_content_at(content, reference, None);

    assert_eq!(
        inspection.visible_title.as_deref(),
        Some("validate-and-fix-victor-report-points")
    );
    assert!(matches!(inspection.status, SessionStatus::Limited(_)));
}

#[test]
fn claude_pane_inspection_ignores_output_and_malformed_borders() {
    // Only a titled border immediately followed by Claude's prompt is valid.
    let content = "\
──────── historical section ────────
ordinary output
──────────────── malformed title
❯
────────────────────────────────────
";

    let inspection = inspect_claude_pane_content_at(content, Utc::now(), None);

    assert_eq!(inspection.visible_title, None);
}

#[test]
fn visible_claude_title_overrides_jsonl_and_falls_back_when_absent() {
    // Preserve the recorded title as the fallback when no visible title exists.
    let recorded = Some("Validate WDP bug report points".to_string());

    assert_eq!(
        prefer_visible_title(
            Some("validate-and-fix-victor-report-points".to_string()),
            recorded.clone(),
        ),
        Some("validate-and-fix-victor-report-points".to_string())
    );
    assert_eq!(prefer_visible_title(None, recorded.clone()), recorded);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test claude_pane_inspection -- --nocapture
cargo test visible_claude_title_overrides_jsonl -- --nocapture
```

Expected: compilation fails because `inspect_claude_pane_content_at` and `prefer_visible_title` do not exist.

- [ ] **Step 3: Add the combined inspection model and title parser**

Add these helpers near the existing pane-status functions:

```rust
#[derive(Debug, Clone, PartialEq)]
struct PaneInspection {
    status: SessionStatus,
    visible_title: Option<String>,
}

fn prefer_visible_title(
    visible_title: Option<String>,
    recorded_title: Option<String>,
) -> Option<String> {
    visible_title.or(recorded_title)
}

fn visible_claude_title(content: &str) -> Option<String> {
    // Claude renders the active title in the border immediately above its prompt.
    let lines: Vec<&str> = content.lines().collect();
    lines.windows(2).rev().find_map(|pair| {
        if !pair[1].trim_start().starts_with('❯') {
            return None;
        }

        let border = pair[0].trim();
        let leading = border.chars().take_while(|c| *c == '─').count();
        let trailing = border.chars().rev().take_while(|c| *c == '─').count();
        if leading < 2 || trailing < 2 {
            return None;
        }

        let title = border.trim_matches('─').trim();
        (!title.is_empty()).then(|| title.to_string())
    })
}
```

Make the content parser return status and title together while preserving the existing status-priority logic:

```rust
fn inspect_claude_pane_content_at(
    content: &str,
    reference: DateTime<Utc>,
    scheduled_continue_at: Option<i64>,
) -> PaneInspection {
    PaneInspection {
        status: pane_status_from_content_at(content, reference, scheduled_continue_at),
        visible_title: visible_claude_title(content),
    }
}
```

Replace `pane_status(...)` with `inspect_claude_pane(...)`. It performs the same single `tmux capture-pane` call and returns `PaneInspection { status: Idle, visible_title: None }` on capture failure.

- [ ] **Step 4: Thread the combined result through discovery**

Change `determine_status(...)` into a resolver returning `PaneInspection`:

- Claude calls `inspect_claude_pane(...)`.
- Codex wraps `codex_pane_status(...)` with `visible_title: None`.
- The zero-token Idle rule still produces `New`.

At both Claude construction sites with JSONL metadata, destructure the result and set:

```rust
status,
session_name: prefer_visible_title(visible_title, info.session_name),
```

For a live Claude pane without JSONL, use the inspected status and visible title instead of hard-coded `New` and `None`. Keep the existing Codex title assignment unchanged:

```rust
session_name: meta.as_ref().and_then(|metadata| metadata.title.clone()),
```

- [ ] **Step 5: Run focused and complete tests**

Run:

```bash
cargo test claude_pane_inspection -- --nocapture
cargo test visible_claude_title_overrides_jsonl -- --nocapture
cargo test
```

Expected: all focused tests and the complete suite pass.

- [ ] **Step 6: Verify the live mismatch and repository health**

Run:

```bash
cargo run --quiet -- json | jq '.sessions[] | select(.tmux_session == "W" and .pane_target == "W:1.0") | {session_id, session_name, status}'
cargo check
git diff --check
git status --short
```

Expected live result: `session_name` is `validate-and-fix-victor-report-points`, while `session_id` remains `5ed930fb-6c29-40fc-97ee-0385c0274bc6`. If that pane has closed or navigated back, rely on the pure regression tests and report that live verification was unavailable.

- [ ] **Step 7: Commit the implementation**

```bash
git add src/session.rs
git commit -m "fix: show visible Claude pane titles"
```
