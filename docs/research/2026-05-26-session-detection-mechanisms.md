---
date: "2026-05-26T12:00:00+02:00"
researcher: Claude (Opus 4.7)
git_commit: 823e79d85f4612f10f7120c7de21766f94856f02
branch: main
repository: recon
topic: "How Claude Code sessions, names, running directories, and status are detected"
tags: [research, codebase, session-discovery, tmux, jsonl, status-detection]
status: complete
last_updated: "2026-05-26"
last_updated_by: Claude (Opus 4.7)
---

# Research: Session Detection Mechanisms in Recon

**Date**: 2026-05-26
**Git Commit**: `823e79d`
**Branch**: main
**Repository**: [gavraz/recon](https://github.com/gavraz/recon)

## Research Question

How does recon detect Claude Code sessions, their names, working directories, and status?

## Summary

Recon discovers sessions by joining four data sources every 2 seconds: (1) `tmux list-panes` for live PIDs and pane metadata, (2) `~/.claude/sessions/{PID}.json` for PID-to-session-ID mapping, (3) `~/.claude/projects/*/*.jsonl` for tokens/model/activity, and (4) `tmux capture-pane` for status detection. The session "name" in the UI is the raw tmux session name. The working directory comes from the JSONL `cwd` field (with tmux pane path as fallback). Status is determined by pattern-matching the last 10 non-empty lines of the pane for spinner characters (Working), "Esc to cancel" or selection menus (Input), or defaulting to Idle/New.

---

## Detailed Findings

### 1. Session Discovery Pipeline

The entry point is `discover_sessions()` at [`session.rs:161`](https://github.com/gavraz/recon/blob/823e79d/src/session.rs#L161), called every 2 seconds from `app.rs:49-68` via `App::refresh()`.

#### Phase 1 - Build Live Session Map

`build_live_session_map()` at [`session.rs:456-490`](https://github.com/gavraz/recon/blob/823e79d/src/session.rs#L456) joins two sources:

**Source A - PID Session Files** (`read_pid_session_map()` at `session.rs:1117-1155`):
- Scans `~/.claude/sessions/` for all `.json` files
- Each file contains `pid`, `sessionId`, and `startedAt`
- Returns `HashMap<i32 pid, SessionFileInfo>`

**Source B - Tmux Pane Enumeration** (`discover_claude_tmux_panes()` at `session.rs:1159-1225`):
- Runs `tmux list-panes -a -F "#{pane_pid}|||#{session_name}|||#{pane_current_command}|||#{pane_current_path}|||#{window_index}|||#{pane_index}"`
- Filters panes where `pane_current_command` matches Claude patterns (starts with ASCII digit, or equals `claude`, `node`, `bash`, `sh`, `zsh`)
- Constructs pane targets as `session_name:window_index.pane_index`

**Join logic** (`session.rs:461-488`): For each tmux pane, if the PID has a session file, inserts into the map keyed by `session_id`. If no session file exists yet (brand-new), inserts with placeholder key `"tmux-{pane_target}"`.

The resulting `LiveSessionInfo` struct (`session.rs:443-449`):
```
pid: i32              -- Claude process PID
tmux_session: String  -- tmux session name
pane_target: String   -- "session:window.pane"
pane_cwd: String      -- #{pane_current_path}
started_at: u64       -- Unix timestamp (0 if brand-new)
```

#### Phase 2 - Scan JSONL Files

At `session.rs:182-303`, iterates `~/.claude/projects/*/` directories:
- JSONL filename stem = session_id, looked up in `live_map`
- If not in map, session is dead/stale and skipped
- If duplicate session_id across directories, larger file wins
- Calls `parse_jsonl()` incrementally with previous state
- Resolves CWD, git info, status, and tmux tags
- Pushes fully populated `Session` struct

#### Phase 3 - Handle Unmatched Live Sessions

At `session.rs:306-423`, processes live_map entries with no matched JSONL:
- For resumed sessions, `find_jsonl_for_resumed_session()` at `session.rs:862` searches by `RECON_RESUMED_FROM` env or ps args
- For truly new sessions, pushes with `SessionStatus::New`

#### Phase 4 - Sort

At `session.rs:428-433`, sorts by `last_activity` truncated to minute resolution (prevents table reorder every 2s), with `started_at` as tiebreaker.

**Child PID resolution**: When the pane's direct PID has no session file, `find_claude_child_pid()` runs `pgrep -P {pid}` and checks each child for a matching `~/.claude/sessions/{child_pid}.json`.

---

### 2. Session Names

The "Session" column in the UI displays `session.tmux_session` - the raw tmux session name from `#{session_name}` in `list-panes` output.

**Reading** (`session.rs:1188`): Extracted as `parts[1]` from the tmux format string, stored verbatim.

**Display** (`ui.rs:63-66`):
```rust
let tmux_name = session.tmux_session.as_deref().unwrap_or("--");
```

**Creation/sanitization** (`tmux.rs:175-188`): When creating new sessions, `sanitize_session_name()`:
- Replaces non-alphanumeric (except `_`) with `-`
- Strips leading `-` (prevents flag injection)
- Falls back to `"claude"` for empty results
- Preserves Unicode alphanumerics

**Default name** (`tmux.rs:122-133`): `default_new_session_info()` uses the CWD's directory name as the proposed session name. `unique_session_name()` appends `-2`, `-3` for conflicts.

The tmux session name and project name are independent - a session named `mywork` can show project `recon::src::main`.

---

### 3. Working Directory (CWD) Detection

CWD resolution follows a priority chain at `session.rs:267-270`:

1. **JSONL `cwd` field** - parsed from `user`/`system`/`assistant` entries (last value wins, `session.rs:784-790`)
2. **Previous poll's cached CWD** for the same session_id
3. **`decode_project_path()`** (`session.rs:641-656`) - reverse-decodes the Claude projects directory name (e.g., `-Users-richard-src-recon` becomes `/Users/richard/src/recon`)

For sessions with no JSONL match (`session.rs:367`): falls back to `live.pane_cwd` (tmux `#{pane_current_path}`).

**Project name derivation** via `git_project_info()` (`session.rs:518-554`):
- Results cached in `GIT_CACHE` with 30-second TTL
- Repo name: `git rev-parse --git-common-dir` (stable across worktrees), falls back to CWD directory name
- Relative dir: `git rev-parse --show-toplevel`, then strips prefix from CWD
- Branch: `git rev-parse --abbrev-ref HEAD`

**"Project" column** in UI (`ui.rs:94-105`) renders as composite spans:
- `project_name` (default color) + `::` (DarkGray) + `relative_dir` (Cyan) + `::` (DarkGray) + `branch` (Green)
- Example: `recon::src/tools::main`

**"Directory" column** (`ui.rs:91`): Raw `session.cwd` with home directory replaced by `~` via `shorten_home()`.

---

### 4. Status Detection

#### The `SessionStatus` Enum (`session.rs:72-88`)

| Status | Meaning | Color | Dot |
|--------|---------|-------|-----|
| `New` | 0 tokens and pane looks idle | Blue | ● |
| `Working` | Spinner character visible in pane | Green | ● |
| `Idle` | No activity indicators found | DarkGray | ● |
| `Input` | Waiting for user (permission/selection) | Yellow | ● |

Input rows additionally get a dark amber background (`Color::Rgb(50, 40, 0)`) across the entire row.

#### `determine_status()` (`session.rs:1025-1041`)

Orchestrates status by combining token counts with pane inspection:
- If pane_target exists, calls `pane_status(target)`
- If `input_tokens == 0 && output_tokens == 0` AND pane is Idle, returns `New`
- Otherwise returns the raw pane status
- A Working/Input pane overrides the zero-token check

#### `pane_status()` (`session.rs:1050-1096`)

Runs `tmux capture-pane -t <target> -p` (plain text, no ANSI). Iterates lines bottom-to-top, checking at most 10 non-empty lines:

**Check 1 - Input (bottommost line only)**: `trimmed.contains("Esc to cancel")` -> `Input`

**Check 2 - Working**: First character passes `is_spinner()` AND line contains `\u{2026}` (ellipsis) -> `Working`

**Check 3 - Input (selection menu)**: Line contains `❯` (U+276F) followed by an ASCII digit -> `Input`

**Default**: `Idle`

#### `is_spinner()` (`session.rs:1101-1107`)

Matches Unicode ranges used by Claude Code's TUI spinner:
- `\u{2720}..=\u{2767}` - Dingbats (various star/cross symbols)
- `\u{23FA}` - Record symbol
- `\u{00B7}` - Middle dot

---

### 5. JSONL Parsing and Token/Model Extraction

#### `parse_jsonl()` (`session.rs:690-838`)

**Incremental parsing**: Tracks `last_file_size` per session. On each poll:
- If file size unchanged, returns all previous values immediately (fast path)
- If file grew, seeks to `prev_file_size` offset and reads only new bytes
- Lines capped at 10 MB via `read_line_capped()` (`session.rs:19-63`)

**From `assistant` entries** (line 760):
- Skips synthetic entries containing `"<synthetic>"`
- `timestamp` -> `last_activity`
- `message.model` -> `model` (raw API ID like `"claude-sonnet-4-6"`)
- `usage.input_tokens + cache_creation + cache_read` -> `total_input`
- `usage.output_tokens` -> `total_output`
- Token counts **replace** (not accumulate) - JSONL stores cumulative totals per message

**From `user`/`system` entries** (line 784):
- `timestamp` -> `last_activity`
- `cwd` -> working directory
- `/model` command stdout: parses `<local-command-stdout>Set model to ...` to extract model name and effort level
- `model::id_from_display_name()` converts display name back to API ID

#### Model Display (`model.rs`)

- `display_name()` (lines 2-12): `"claude-opus-4-6"` -> `"Opus 4.6"`, etc.
- `context_window()` (lines 15-25): Opus = 1M, all others = 200k
- `format_with_effort()` (lines 42-49): Appends effort if non-default, e.g. `"Opus 4.6 (max)"`
- Token display: `"{used/1000}k / {window}"` where window is `"200k"` or `"1M"`

---

## Architecture Insights

### Data Flow Diagram

```
tmux list-panes -a -F "..."              ~/.claude/sessions/{PID}.json
  |                                        |
  v                                        v
Vec<(pid, session, target, cwd)>         HashMap<pid, {session_id, started_at}>
  |                                        |
  +------ JOIN on pid ------->  HashMap<session_id, LiveSessionInfo>
                                           |
                                           v
              ~/.claude/projects/*/*.jsonl (filter by live_map key match)
                         |
                         v
                  parse_jsonl() incremental
                         |
                         v
               tmux capture-pane -t <target> -p
                         |
                         v
               pane_status() -> SessionStatus
                         |
                         v
               git rev-parse (cached 30s)
                         |
                         v
                    Vec<Session> sorted by last_activity
```

### Key Design Decisions

1. **PID-based matching** over CWD-based heuristics - avoids token-swapping bugs when multiple sessions share a directory
2. **Incremental JSONL parsing** with byte-offset seeking - avoids re-reading entire conversation files every 2 seconds
3. **Minute-truncated sort** - prevents the session table from reordering on every poll cycle
4. **No mtime filtering** - the live_map check already gates liveness, so stat() calls were removed as unnecessary overhead
5. **10-line pane scan limit** - balances accuracy with performance for status detection
6. **Child PID resolution** via pgrep - handles shell wrapper scenarios where the pane PID isn't the Claude process itself
7. **Git cache with 30s TTL** - branch can change between refreshes, but repo name doesn't

---

## Code References

- `src/session.rs:161` - `discover_sessions()` entry point
- `src/session.rs:456-490` - `build_live_session_map()` join logic
- `src/session.rs:1117-1155` - `read_pid_session_map()` reads PID JSON files
- `src/session.rs:1159-1225` - `discover_claude_tmux_panes()` tmux enumeration
- `src/session.rs:690-838` - `parse_jsonl()` incremental parser
- `src/session.rs:1025-1041` - `determine_status()` orchestrator
- `src/session.rs:1050-1096` - `pane_status()` pane content analysis
- `src/session.rs:1101-1107` - `is_spinner()` character matching
- `src/session.rs:518-554` - `git_project_info()` with cache
- `src/session.rs:567-588` - `fetch_canonical_repo_name()` via `--git-common-dir`
- `src/session.rs:641-656` - `decode_project_path()` fallback
- `src/ui.rs:63-66` - Session name display
- `src/ui.rs:69-74` - Status dot/color mapping
- `src/ui.rs:94-105` - Project column composite rendering
- `src/model.rs:2-49` - Model ID mapping, context windows, effort formatting
- `src/tmux.rs:175-188` - `sanitize_session_name()`
- `src/app.rs:49-68` - `App::refresh()` 2-second poll loop

## Open Questions

- The Claude detection heuristic (`pane_current_command` matching) could miss Claude processes running under unexpected names
- The `/model` command parsing relies on `<local-command-stdout>` tags - if Claude Code changes that format, model detection from user entries would break
- `is_spinner()` hardcodes specific Unicode ranges - new spinner characters in future Claude Code versions would need updates
