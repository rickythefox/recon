# Visible Claude Pane Title Design

## Goal

Show the title of the Claude session currently visible in a tmux pane, including a foregrounded background-agent session.

The observed mismatch is:

- Parent JSONL title: `Validate WDP bug report points`
- Visible Claude pane title: `validate-and-fix-victor-report-points`

Recon currently maps the pane process to the parent session and therefore renders the stale parent title.

## Behavior

For live Claude panes, the title rendered in Claude's prompt border is authoritative. If the pane does not render a recognizable title, recon falls back to the parsed JSONL title.

Navigating into a named background agent updates the recon label to that visible agent title. Navigating back to the parent restores the parent title on the next refresh.

This changes display only. Session identity, JSONL ownership, tokens, model, status, activity, and tmux targeting continue to use the parent process mapping. Codex sessions continue using Codex metadata unchanged.

## Parsing and Data Flow

Claude pane inspection returns one structure containing both status and an optional visible title. This keeps the existing single `tmux capture-pane` call per refresh.

The title parser accepts only a prompt-border line with horizontal box-drawing runs on both sides of non-empty text. It trims surrounding whitespace and rejects ordinary output, status text, and incomplete separators.

Discovery chooses the display title in this order:

1. Recognized visible Claude pane title.
2. Parsed Claude JSONL title.
3. No title.

Pane capture or title parsing failures are non-fatal and use the existing JSONL fallback.

## Testing

Regression tests cover:

- A visible custom title overrides a different parent JSONL title.
- Removing the visible title restores the JSONL fallback.
- Ordinary output and malformed border lines are not treated as titles.
- Status and title are derived from the same pane capture.
- Codex title behavior remains unchanged.

Run the focused session tests, then the complete Rust test suite and the repository's existing verification checks.

## Out of Scope

- Reassigning a pane to the foregrounded agent's session ID or JSONL.
- Reading Claude daemon, roster, or job files to infer foreground state.
- Displaying background agents that are not currently visible in a tmux pane.
