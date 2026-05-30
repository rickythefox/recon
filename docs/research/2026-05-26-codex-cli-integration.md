---
date: "2026-05-26T14:00:00+02:00"
researcher: Claude (Opus 4.7)
git_commit: 823e79d85f4612f10f7120c7de21766f94856f02
branch: main
repository: recon
topic: "Adding Codex CLI support to recon - session detection, names, CWD, and status"
tags: [research, codebase, codex, integration, session-discovery, status-detection]
status: complete
last_updated: "2026-05-26"
last_updated_by: Claude (Opus 4.7)
---

# Research: Codex CLI Integration into Recon

**Date**: 2026-05-26
**Git Commit**: `823e79d`
**Branch**: main
**Repository**: [gavraz/recon](https://github.com/gavraz/recon)

## Research Question

How can we add Codex CLI session monitoring to recon, detecting sessions, names, working directories, and status the same way we do for Claude Code?

## Summary

Codex CLI (v0.133.0) is a Rust binary wrapped by a Node.js entry point. It stores session data in SQLite (`~/.codex/state_5.sqlite`) and JSONL rollout files (`~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl`). The biggest gap vs Claude Code: **Codex does not write PID-to-session JSON files**, so linking a running tmux pane to a specific session requires either SQLite + process tree inspection or matching by CWD/rollout timestamps. Status detection is feasible via pane capture but uses entirely different text patterns than Claude Code.

---

## Detailed Findings

### 1. Codex Binary Architecture

- **JS wrapper**: `~/.asdf/installs/nodejs/24.8.0/lib/node_modules/@openai/codex/bin/codex.js`
- **Native binary**: `~/.asdf/installs/nodejs/24.8.0/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex` (Mach-O arm64, compiled Rust from `codex-rs/tui/`)
- **Process chain**: `fish -> node (JS wrapper) -> codex (native) -> node (child workers)`
- **tmux `pane_current_command`**: Shows as **`node`** (the JS wrapper is the direct shell child)

### 2. Session Storage - SQLite vs File-Based

#### Primary session registry: `~/.codex/state_5.sqlite` `threads` table

This is the single source of truth for all Codex sessions (310 threads observed). Key columns:

| Column | Type | Description | Claude Equivalent |
|--------|------|-------------|-------------------|
| `id` | TEXT (UUID) | Session ID | JSONL filename stem |
| `rollout_path` | TEXT | Absolute path to JSONL log | `~/.claude/projects/*/{id}.jsonl` |
| `cwd` | TEXT | Working directory | JSONL `cwd` field |
| `model` | TEXT | e.g. `gpt-5.5`, `o4-mini` | JSONL `message.model` |
| `reasoning_effort` | TEXT | `high`, `xhigh`, `medium` | JSONL effort field |
| `tokens_used` | INTEGER | Total tokens consumed | Cumulative from JSONL `usage` |
| `title` | TEXT | Session title / first message | (no equivalent) |
| `source` | TEXT | `cli`, `exec`, `vscode`, `subagent` | (always CLI) |
| `git_branch` | TEXT | Branch name at session start | (not stored per-session) |
| `git_sha` | TEXT | Commit at session start | (not stored) |
| `git_origin_url` | TEXT | Repository URL | (not stored) |
| `created_at` / `updated_at` | INTEGER | Unix epoch | JSONL timestamps |
| `cli_version` | TEXT | e.g. `0.133.0` | (not stored) |
| `archived` | INTEGER | 0/1 flag | (no equivalent) |
| `sandbox_policy` | TEXT | e.g. `danger-full-access` | (no equivalent) |
| `approval_mode` | TEXT | e.g. `never` | (no equivalent) |

#### No PID-to-session files

**Critical difference**: Codex does NOT write `~/.codex/sessions/{PID}.json` files. The only PID reference is in `~/.codex/logs_2.sqlite` `logs` table, where `process_uuid` has format `pid:{PID}:{UUID}` mapped to `thread_id`. This database is 176 MB / 78K rows - not ideal for polling.

### 3. JSONL Rollout Files

**Path pattern**: `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-{ISO_TIMESTAMP}-{SESSION_UUID}.jsonl`

**Example**: `rollout-2026-05-26T09-05-13-019e631a-104b-7f73-b26b-d6ea1a6efcd1.jsonl`

#### Event types

| Event type | Subtype | Description |
|------------|---------|-------------|
| `session_meta` | - | First line: `id`, `cwd`, `cli_version`, `source`, `model_provider`, `git` |
| `turn_context` | - | Per-turn: `model`, `effort`, `cwd`, `timezone` |
| `event_msg` | `task_started` | Turn start: `turn_id`, `started_at`, `model_context_window` |
| `event_msg` | `task_complete` | Turn end: `duration_ms`, `time_to_first_token_ms` |
| `event_msg` | `token_count` | Cumulative: `input_tokens`, `cached_input_tokens`, `output_tokens`, `reasoning_output_tokens`, `total_tokens` |
| `event_msg` | `user_message` | User input text |
| `event_msg` | `agent_message` | Agent response, `phase: "final_answer"` |
| `response_item` | `message` | Full message with `role` and `content` |
| `response_item` | `function_call` | Tool invocation: `name`, `arguments`, `call_id` |
| `response_item` | `function_call_output` | Tool result |
| `response_item` | `reasoning` | Encrypted reasoning content |

Token tracking uses cumulative `token_count` events (last one = final total), same pattern as Claude Code.

### 4. Other Data Files

| Path | Purpose |
|------|---------|
| `~/.codex/session_index.jsonl` | Lightweight index: `id`, `thread_name`, `updated_at` |
| `~/.codex/history.jsonl` | User prompt history: `session_id`, `ts`, `text` |
| `~/.codex/goals_1.sqlite` | Per-session objectives with `token_budget`, `tokens_used`, status |
| `~/.codex/config.toml` | Global config: model, sandbox, approval mode |
| `~/.codex/models_cache.json` | Available models with reasoning levels |
| `~/.codex/log/codex-tui.log` | TUI log with `session_loop{thread_id=...}` spans |
| `~/.codex/shell_snapshots/` | Shell env snapshots per session |

### 5. Status Detection via Pane Capture

The Codex TUI renders a distinct two-line bottom area:

**Prompt line**: `› {placeholder text}` - the `›` character is U+203A (single right-pointing angle quotation mark)

**Status line**: `{model} {effort} · {cwd} · Context {N}% left · {rate_limit} · {thread_id_or_name}`

#### Status patterns

| Status | Detection Pattern |
|--------|------------------|
| **Idle** | Last lines contain `›` (U+203A) prompt AND status line with `· Context` |
| **Working** | `›` prompt absent from last lines; streaming `•` bullet lines visible; or `Pursuing goal` text |
| **Input/Approval** | `Allow Codex to run`, `Codex wants to edit`, `E X E C`, `D I F F`, `P A T C H`, `P E R M I S S I O N S`, `E L I C I T A T I O N`, `[ ! ] Action Required`, `Reviewing approval request` |
| **New** | No meaningful content yet |
| **Error** | `turn aborted`, `ran out of room`, `Goal budget reached`, `interrupted`, `out of credits`, `Usage limit reached` |
| **Work complete** | Horizontal separator `-- Worked for {duration} --` appears above prompt |
| **Multi-agent** | `main needs input`, `main interrupted`, `Ctrl+C to return` |

#### Comparison with Claude Code detection

| Aspect | Claude Code | Codex CLI |
|--------|-------------|-----------|
| Working | Spinner char (U+2720-2767) + `...` (U+2026) | No spinners; `•` bullet lines streaming, `›` prompt absent |
| Input | `Esc to cancel` on last line | `Allow Codex to run`, `[ ! ] Action Required`, spaced headers |
| Idle | Default (no match) | `›` prompt visible + status line |
| New | 0 tokens + Idle pane | No JSONL content yet |
| Prompt char | `>` | `›` (U+203A) |

---

## Integration Strategy

### A. Process Discovery (extending `discover_claude_tmux_panes`)

Current Claude Code detection checks `pane_current_command` for: starts with digit, or equals `claude`/`node`/`bash`/`sh`/`zsh`.

For Codex, `pane_current_command` is `node` (same as some Claude instances). Disambiguation options:

1. **Check process args**: Run `ps -o args= -p {pid}` and look for `codex` in the command line
2. **Check child process**: Use `pgrep -P {pid}` and inspect the child binary path for `@openai/codex`
3. **Check session files**: If `~/.claude/sessions/{pid}.json` exists, it's Claude; otherwise check Codex's SQLite

**Recommended**: Option 3 (check session files) as the primary discriminator, with option 1 as fallback.

### B. PID-to-Session Mapping (the hard part)

Claude has `~/.claude/sessions/{PID}.json` - a direct PID-to-session-ID lookup. Codex has no equivalent.

Options:

1. **SQLite query on `logs_2.sqlite`**: Parse `process_uuid` field (`pid:{PID}:{UUID}`) to extract PID, join with `thread_id`. Problem: 176 MB database, 78K rows, heavy for 2-second polling.

2. **Match by rollout file mtime**: List running Codex processes, find rollout files with recent writes matching the session `updated_at` in `state_5.sqlite`. Fragile.

3. **Match by CWD**: Cross-reference the tmux pane CWD (`#{pane_current_path}`) with the `cwd` column in `state_5.sqlite` threads that have a recent `updated_at`. Problem: multiple sessions can share a CWD.

4. **Parse `codex-tui.log`**: The TUI log at `~/.codex/log/codex-tui.log` contains `session_loop{thread_id=...}` entries with PIDs extractable from tracing spans. Could be tailed for recent entries.

5. **Contribute upstream**: File a PR to Codex CLI to write `~/.codex/sessions/{PID}.json` files, mirroring Claude Code's approach. This would make integration trivial.

**Recommended**: Option 5 is the cleanest long-term. For an immediate solution, option 1 (SQLite query) is the most reliable - query just the latest entries where `process_uuid LIKE 'pid:{PID}:%'` with an index.

### C. Session Metadata

Once a session ID is linked, metadata comes from two sources:

- **`state_5.sqlite` `threads` table**: model, tokens_used, cwd, title, git_branch, source, timestamps - all available in a single row query
- **JSONL rollout file** (path from `rollout_path` column): per-turn token breakdowns, model changes, detailed activity timestamps

The `threads` table is the more efficient source for dashboard polling since it has cumulative data already aggregated.

### D. Status Detection (extending `pane_status`)

Add a Codex-specific branch in `pane_status()` or a parallel `codex_pane_status()`:

```
fn codex_pane_status(pane_target: &str) -> SessionStatus {
    // capture pane, iterate last 10 non-empty lines bottom-to-top
    
    // Check 1 - Approval prompt (Input)
    // Look for: "Allow Codex to run", "Codex wants to edit",
    //           "[ ! ] Action Required", "E X E C", "P E R M I S S I O N S",
    //           "D I F F", "P A T C H", "E L I C I T A T I O N"
    
    // Check 2 - Idle (has prompt)
    // Look for: line starts with '›' (U+203A)
    // AND another line contains "· Context"
    
    // Check 3 - Working (no prompt visible in last lines)
    // Default if neither idle nor input patterns found
    
    // Note: Codex has no spinner characters to detect
}
```

### E. Session Name and CWD

- **Name**: `threads.title` (session title / first user message) or `session_index.jsonl` `thread_name`
- **CWD**: `threads.cwd` column, or JSONL `session_meta.cwd`, or tmux `#{pane_current_path}`
- **Git info**: Already stored in `threads` table (`git_branch`, `git_sha`, `git_origin_url`) - no need to shell out to git

---

## Key Gaps and Risks

1. **No PID-to-session file**: The single biggest integration hurdle. Every other piece of data is accessible, but linking a running process to its session without this file requires workarounds.

2. **`node` process disambiguation**: Both Claude Code and Codex show as `node` in tmux. Need reliable differentiation.

3. **SQLite locking**: Polling `state_5.sqlite` every 2 seconds could conflict with Codex's own writes. Use `PRAGMA journal_mode=wal` and read-only connections.

4. **Rollout file discovery**: Unlike Claude Code where JSONL files live in a predictable `~/.claude/projects/{hash}/` structure, Codex uses date-based directories and the path is only reliably found via the `rollout_path` column in SQLite.

5. **Status patterns may change**: Codex is actively developed (v0.133.0). The TUI text patterns for status detection are not a stable API.

---

## Code References

### Recon (current Claude Code detection)
- `src/session.rs:1159-1225` - `discover_claude_tmux_panes()` - would need Codex equivalent
- `src/session.rs:1117-1155` - `read_pid_session_map()` - reads `~/.claude/sessions/{PID}.json`
- `src/session.rs:1050-1096` - `pane_status()` - would need Codex patterns
- `src/session.rs:1101-1107` - `is_spinner()` - Claude-specific, not applicable to Codex
- `src/session.rs:690-838` - `parse_jsonl()` - would need Codex JSONL schema support
- `src/model.rs` - model ID mapping, needs Codex models added

### Codex data sources
- `~/.codex/state_5.sqlite` - `threads` table (session registry)
- `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-*.jsonl` - conversation logs
- `~/.codex/logs_2.sqlite` - `logs` table (has PID in `process_uuid`)
- `~/.codex/session_index.jsonl` - lightweight name index
- `~/.codex/log/codex-tui.log` - TUI trace logs with thread IDs

## Open Questions

- Should recon support a unified `Session` struct for both tools, or separate `ClaudeSession` / `CodexSession` types?
- Is querying SQLite every 2 seconds acceptable, or should we use file-watching on the rollout JSONL instead?
- Would it be worth contributing PID-to-session JSON file support upstream to Codex CLI?
- How should the UI differentiate Claude vs Codex sessions (icon? color? column?)?
