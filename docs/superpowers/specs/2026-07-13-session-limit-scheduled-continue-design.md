# Session Limit Scheduled Continue Design

## Goal

Make Claude Code panes that have exhausted their session allowance visible in recon, then let the user schedule an automatic continuation from the dashboard.

The observed Claude marker is:

```text
You've hit your session limit · resets 1:10am (Asia/Nicosia)
```

## User Experience

- A limited Claude pane appears in the table with a red `Limit 1:10` status.
- Pressing `c` on the selected limited pane schedules `continue` followed by Enter for its reset deadline.
- After scheduling, the status changes to `Queued 1:10` so the action has visible confirmation.
- Pressing `c` again does not create another timer.
- The binding works in table view and for the selected agent in a zoomed Tamagotchi room.
- `c` does nothing for non-limited sessions.
- If the reset deadline has already elapsed, the scheduled command runs immediately.

## Status Detection

Extend Claude pane parsing with a structured session-limit status containing the reset time, timezone, and absolute deadline.

The parser recognizes only the complete Claude marker, parses the AM/PM time and IANA timezone, and rejects incomplete or malformed markers. It resolves the displayed time to the first matching instant after the rate-limit event timestamp. The JSONL file modification time provides that event reference; current time is used only when the event timestamp is unavailable. This handles both reset times after midnight and recon being opened after the reset has already occurred.

Existing live signals keep priority over historical pane content:

1. Active permission/input prompt
2. Active working indicator
3. Session limit or queued continuation
4. Background agent or shell count
5. Idle

This prevents a stale visible limit line from overriding a pane that has resumed working.

`Limit` and `Queued` use red in the table. Tamagotchi view treats them as attention states and renders their explicit status label. They are not included in the `i` or Tab input-navigation behavior because no user interaction can unblock them before reset.

## Scheduling

Recon delegates the delay to `tmux run-shell -b`, so the timer belongs to the tmux server and survives the recon popup exiting.

Pane discovery records tmux's immutable `#{pane_id}` in addition to the existing human-readable pane target. Scheduling accepts only a validated pane ID of `%` followed by digits. The background command:

1. Sleeps for the non-negative duration until the absolute deadline.
2. Verifies that the pane still exists.
3. Sends literal text `continue` with `tmux send-keys -l`.
4. Sends Enter separately.
5. Clears the scheduling marker.

A pane-local tmux option stores the scheduled Unix timestamp. Discovery reads the option as part of `list-panes`, which makes queued state survive closing and reopening recon. The option also prevents duplicate timers. If launching the background job fails, recon removes the option and leaves the pane in `Limit` state.

No retry is attempted if the pane disappears or Claude rejects the continuation. The feature performs exactly one scheduled send.

## Code Boundaries

- `session.rs`: parse the Claude marker, resolve the reset deadline, expose limited and queued statuses, and read pane IDs plus scheduling options during discovery.
- `tmux.rs`: validate pane IDs and schedule the background continuation.
- `app.rs`: bind `c` to the selected limited session in both dashboard views and update the in-memory status after success.
- `ui.rs` and `view_ui.rs`: render the new attention states and keybinding help.
- `README.md`: document both statuses and the `c` binding.

## Testing

Unit tests cover:

- The exact live Claude marker produces `Limit 1:10`.
- PM reset times and IANA timezones parse correctly.
- A reset after midnight resolves to the following date.
- An already elapsed deadline produces a zero-second delay.
- Malformed or unrelated usage-limit text is ignored.
- Working and Input take precedence over stale limit text.
- A matching pane scheduling option produces `Queued 1:10`.
- Pane ID validation rejects shell metacharacters and human-readable targets.
- Scheduling command construction uses literal text followed by Enter.
- The `c` key schedules only a selected limited pane and does not duplicate a queued schedule.

Run the complete Rust test suite and formatting checks after the focused tests pass.

## Out of Scope

- Automatically scheduling every limited pane without a keypress.
- Canceling or editing a scheduled continuation.
- Retrying after the one `continue` submission.
- Supporting limit-message formats from agents other than Claude Code.
