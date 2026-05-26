# Session Persistence & Digit Key Switching

## Feature 1: Remember Last Selected Session

**Goal:** When recon launches, the previously selected session is re-selected automatically.

### Persistence

- File: `~/.config/recon/state.json`
- Format: `{"last_session_id": "<session_id>"}`
- Create `~/.config/recon/` directory on first write if it doesn't exist.

### Write behavior

- Write on switch (Enter or digit key) before exiting - the session being switched to is saved.
- Write on quit (q) - save whatever session is currently highlighted.
- Do NOT write on every up/down navigation or every 2s refresh cycle.

### Read behavior

- On launch, read the file. If it contains a valid `last_session_id`, find the matching session in the current list and set `selected` to its filtered index.
- If the session ID is not found (session ended, no longer live), default to `selected = 0`.
- If the file doesn't exist or is malformed, default to `selected = 0`.

### Implementation

- Add a `state.rs` module with `load_state()` and `save_state()` functions.
- `load_state()` returns `Option<String>` (the session ID).
- `save_state(session_id: &str)` writes the JSON file.
- Call `load_state()` in `App::new()` to set an initial `last_session_id` field.
- After `refresh()` populates sessions, resolve the saved ID to a selected index.
- Call `save_state()` in the Enter/digit/quit key handlers.

## Feature 2: Digit Keys Switch to Session

**Goal:** Pressing 1-9 or 0 immediately switches to the corresponding numbered session.

### Key mapping

- Keys `1`-`9` map to the session whose `#` column shows that number.
- Key `0` maps to the session whose `#` column shows `10`.
- Only active in `ViewMode::Table` when filter input is NOT focused.

### Behavior

- Resolve the digit to the displayed row number, find the session with that `#` value in the filtered view.
- If that row exists, perform the same action as Enter: `switch_to_pane()` + save state + quit.
- If the row doesn't exist (e.g., pressing `7` when only 5 sessions visible), do nothing.

### Number display

- The `#` column already shows numbers via `real_idx + 1`. These numbers are the hotkeys.
- When a filter is active, the `#` column values stay as the original row numbers - the digit key maps to the number shown on screen, not the filtered position.

## Scope

- Two new behaviors in `app.rs` key handling.
- One new `state.rs` module (~30 lines).
- No changes to session discovery, JSONL parsing, or tmux integration.
- No new dependencies (serde_json is already used).
