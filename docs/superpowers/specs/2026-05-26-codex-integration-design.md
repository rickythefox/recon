# Codex CLI Integration - Design Spec

**Date**: 2026-05-26
**Approach**: Parallel module (`codex.rs`) with shared types

## Overview

Add Codex CLI session monitoring to recon with full parity: detect running sessions, show name/CWD/status/tokens, create new sessions, resume, park/unpark, and history. Codex sessions appear in the same table as Claude sessions, distinguished by accent color on the session name.

## Constraints

- Cannot modify Codex CLI upstream
- Codex has no PID-to-session JSON files (unlike Claude's `~/.claude/sessions/{PID}.json`)
- Codex stores metadata in SQLite (`~/.codex/state_5.sqlite`), not flat files

## Data Model Changes

### `AgentKind` enum

New enum in `session.rs`:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum AgentKind { Claude, Codex }
```

### `Session` struct

Add one field:

```rust
pub agent: AgentKind,
```

All existing fields remain. Codex sessions populate the same fields:
- `effort` = Codex's `reasoning_effort` (e.g. `high`, `xhigh`, `medium`)
- `total_input_tokens` = Codex's `tokens_used` (single total; `total_output_tokens` stays 0)
- `jsonl_path` = rollout file path from SQLite `rollout_path` column
- `last_file_size` = 0 (not used for Codex; metadata comes from SQLite)

### `model.rs` additions

Add Codex models to existing match arms:

**`display_name()`**:
- `"gpt-5.5"` -> `"GPT-5.5"`
- `"gpt-5.4"` -> `"GPT-5.4"`
- `"o4-mini"` -> `"o4-mini"`
- `"o3"` -> `"o3"`

**`context_window()`**:
- `"gpt-5.5"` -> `1_000_000`
- All others -> `200_000` (existing default)

**`format_with_effort()`**: Already generic - appends `(effort)` to display name. Codex effort values (`high`, `xhigh`, `medium`) render as e.g. `GPT-5.5 (xhigh)`.

**`id_from_display_name()`**: Add reverse mappings for the new display names.

## Process Discovery

### Extended pane enumeration

The existing `discover_claude_tmux_panes()` is generalized to `discover_agent_tmux_panes()`. Candidate panes are those where `pane_current_command` is `node`, `claude`, `codex`, starts with a digit (Claude version numbers like `2.1.150`), or is `bash`/`sh`/`zsh`.

Detection order per pane:

1. Check if `~/.claude/sessions/{PID}.json` exists -> **Claude** (instant file check)
2. For `node`/`codex` commands: try Codex cache first (`find_codex_session_cached`) -> **Codex** (instant on cache hit)
3. Fall through to `find_claude_child_pid(pid)` via `pgrep` -> **Claude**

### Performance: lsof caching

`lsof` is expensive (~200-500ms per call on macOS) and `find_codex_session` recurses 3 levels deep through the process tree. A static `CODEX_PID_CACHE: Mutex<HashMap<i32, (i32, String)>>` caches positive results keyed by pane PID. Only new PIDs trigger lsof; stale entries are evicted when pane PIDs disappear from tmux.

### Detection order matters for performance

The `node`/`codex` command check with Codex cache **must come before** `find_claude_child_pid`. Otherwise every `node` pane (including cached Codex sessions) triggers a `pgrep` call (~20ms each). With 3+ Codex panes this adds 60ms+ per refresh, causing visible TUI lag.

Shell panes (`bash`/`sh`/`zsh`) only run `find_claude_child_pid` (fast file existence check per child). `fish` is excluded from shell candidates since Claude/Codex never appear as bare `fish` in `pane_current_command` - they always show as `node`, a version number, or `claude`/`codex`.

### Process tree depth

The Codex process chain is `fish/bash -> node (JS wrapper) -> codex (native binary)`. The rollout JSONL file is only held open by the native binary (grandchild of the shell), so `find_codex_session()` recurses up to 3 levels deep through the process tree via `pgrep -P`. This is necessary because `lsof` on the node wrapper alone finds nothing.

### Return type

Change from `Vec<(pid, session_name, pane_target, pane_cwd)>` to:

```rust
struct DiscoveredPane {
    pid: i32,
    tmux_session: String,
    pane_target: String,
    pane_cwd: String,
    agent: AgentKind,
    codex_session_id: Option<String>,  // UUID from lsof rollout filename
}
```

### `build_live_session_map()` changes

For Claude panes: existing logic (lookup in `read_pid_session_map()`).

For Codex panes: the session ID is already known from lsof (stored in `codex_session_id`). Insert directly into the live map keyed by that session ID. If lsof didn't find a session ID (brand new process), use placeholder key `"codex-tmux-{pane_target}"`.

`LiveSessionInfo` gains an `agent: AgentKind` field so that `discover_sessions()` knows whether to fetch metadata from Claude JSONL or Codex SQLite.

## Codex Metadata (new `codex.rs` module)

### SQLite query

Given a session UUID, query `~/.codex/state_5.sqlite` read-only:

```sql
SELECT model, reasoning_effort, tokens_used, cwd, title,
       git_branch, updated_at, created_at, rollout_path
FROM threads WHERE id = ?
```

### Field mapping

| SQLite column | Session field |
|---|---|
| `model` | `model` |
| `reasoning_effort` | `effort` |
| `tokens_used` | `total_input_tokens` |
| `cwd` | `cwd` |
| `git_branch` | `branch` |
| `updated_at` | `last_activity` (epoch -> ISO string) |
| `created_at` | `started_at` |
| `rollout_path` | `jsonl_path` |

### Connection management

- Open read-only connection per refresh cycle (every 2s), not held between polls
- Use WAL mode to avoid conflicts with Codex's writes
- If database missing or locked, Codex session appears with zeroed metadata (fail gracefully)

### `lsof` for PID-to-session

Run `lsof -p {PID}` on the process and its descendants (up to 3 levels deep via recursive `pgrep -P`). The rollout JSONL is held open by the native Codex binary, which is a grandchild of the tmux pane's shell process (`fish -> node -> codex`). Parse output for lines matching the rollout path pattern. Extract session UUID from filename:

```
rollout-{ISO_TIMESTAMP}-{SESSION_UUID}.jsonl
```

The UUID is the last 36 characters before `.jsonl` (standard 8-4-4-4-12 UUID format). The timestamp prefix also contains hyphens, so split by `-` is not reliable - match the UUID pattern instead.

## Status Detection

### New `codex_pane_status()` in `codex.rs`

Same interface as `pane_status()`: takes pane target, returns `SessionStatus`. Runs `tmux capture-pane -t <target> -p`, collects last 15 non-empty lines for analysis.

**Key insight:** Unlike Claude Code where the prompt disappears during work, the Codex `›` prompt persists in the pane while output streams below it. A naive "first `›` = Idle" approach is wrong - the prompt's *position* relative to the bottom of the pane determines state. The Codex TUI layout (bottom-up) is: status bar, then either the prompt line (idle) or active streaming content (working).

| Priority | Pattern | Status |
|---|---|---|
| 1 | `esc to interrupt` anywhere in tail | **Working** |
| 2 | Contains `Allow Codex to run`, `Codex wants to edit`, `Action Required`, or spaced headers: `E X E C`, `P E R M I S S I O N S`, `D I F F`, `P A T C H`, `E L I C I T A T I O N` | **Input** |
| 3 | `›` (U+203A) within 2 lines of bottom (just above status bar) | **Idle** |
| 4 | `›` found but buried deeper (active content pushed it up) | **Working** |
| 5 | `Worked for` separator visible | **Idle** |
| 6 | Default (no prompt visible, content streaming) | **Working** |

### `determine_status()` dispatch

Add `AgentKind` parameter. Dispatch to `pane_status()` (Claude) or `codex::codex_pane_status()` (Codex). The New check (0 tokens + Idle) applies to both.

## Session Creation (`recon new`)

### Form changes

Add a third field to `new_session.rs`: agent kind selector (Claude / Codex), defaulting to Claude. Cycle with a keybinding (e.g. Tab on the field, or a toggle key).

### tmux.rs changes

- Add `which_codex()` alongside `which_claude()`
- `create_session()`: when command is None, use `which_claude()` or `which_codex()` based on the agent kind parameter
- Add `agent: AgentKind` parameter to `create_session()`

## Session Resume

### tmux.rs changes

- `resume_session()` dispatches by `AgentKind`:
  - Claude: `claude --resume {session_id}`
  - Codex: `codex resume {session_id}`
- The `RECON_RESUMED_FROM` env var pattern can be reused for Codex

### history.rs changes

- Add `discover_codex_sessions()`: query `state_5.sqlite` `threads` table for sessions not in the live map, ordered by `updated_at DESC`
- Merge Claude and Codex dead sessions into one list, sorted by recency
- Display `AgentKind` indicator in the picker (color-coded)
- Store `AgentKind` in the history entry so resume knows which tool to invoke

## Park/Unpark

### park.rs changes

- Snapshot struct gains `agent: AgentKind` field
- Unpark uses the agent kind to decide resume command
- Otherwise unchanged - tmux kill/create is tool-agnostic

## UI Changes

### Table view (`ui.rs`)

- **Session name color**: Claude sessions use default color. Codex sessions use `Color::Cyan` for the session name cell.
- **Title bar**: Change from `" recon - Claude Code Sessions "` to `" recon "`.
- All other columns render identically for both agent types.

### Tamagotchi view (`view_ui.rs`)

- Same sprites for both agent types
- Name label uses Cyan for Codex sessions (matching table view)

### JSON output (`app.rs`)

Add `"agent": "claude"` or `"agent": "codex"` to serialized session objects.

## New Dependency

```toml
rusqlite = { version = "0.34", features = ["bundled"] }
```

The `bundled` feature statically compiles SQLite - no system library needed.

## Module Summary

| Module | Changes |
|---|---|
| `session.rs` | Add `AgentKind` enum, `agent` field to `Session`, generalize pane discovery, dispatch status detection |
| `codex.rs` | **New** - lsof PID lookup, SQLite metadata query, `codex_pane_status()`, Codex history discovery |
| `model.rs` | Add Codex model entries to all match arms |
| `tmux.rs` | Add `which_codex()`, agent-aware `create_session()` and `resume_session()` |
| `new_session.rs` | Add agent kind selector field |
| `history.rs` | Merge Codex dead sessions from SQLite, show agent indicator |
| `park.rs` | Store/restore `AgentKind` in snapshots |
| `ui.rs` | Cyan accent for Codex sessions, update title bar |
| `view_ui.rs` | Cyan name label for Codex |
| `app.rs` | Add `agent` to JSON output |
| `Cargo.toml` | Add `rusqlite` dependency |

## Out of Scope

- Codex JSONL rollout parsing (using SQLite instead)
- Codex subagent/spawn tracking (`thread_spawn_edges` table)
- Codex goals/budget display
- Different sprite sets for Codex in tamagotchi view
