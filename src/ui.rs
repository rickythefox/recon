use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::session::SessionStatus;

const TABLE_BORDER_WIDTH: u16 = 2;
const TABLE_COLUMN_SPACING: u16 = 1;
const NUMBER_COLUMN_WIDTH: u16 = 4;
const ROW_HEIGHT: u16 = 2;
const SESSION_COLUMN_WIDTH: u16 = 4;
const STATUS_COLUMN_WIDTH: u16 = 11;
const MODEL_COLUMN_WIDTH: u16 = 11;
const ACTIVITY_COLUMN_WIDTH: u16 = 12;
const SESSION_TITLE_SEPARATOR: char = '•';
const BACKGROUND_TASK_COLOR: Color = Color::Green;

pub fn render(frame: &mut Frame, app: &App) {
    let show_search = app.filter_active || !app.filter_text.is_empty();
    let chunks = if show_search {
        Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area())
    } else {
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area())
    };

    render_table(frame, app, chunks[0]);
    if show_search {
        render_search_bar(frame, app, chunks[1]);
        render_footer(frame, app, chunks[2]);
    } else {
        render_footer(frame, app, chunks[1]);
    }
}

fn render_table(frame: &mut Frame, app: &App, area: Rect) {
    let filtered = app.filtered_indices();

    // Hide Session column when all visible sessions share the same tmux name
    let show_session_col = {
        let mut names = filtered
            .iter()
            .filter_map(|&i| app.sessions[i].tmux_session.as_deref());
        let first = names.next();
        first.is_some() && !names.all(|n| Some(n) == first)
    };
    let project_width = project_column_width(area.width, show_session_col);

    let mut header_cells = vec![Cell::from(if show_session_col { " #/S" } else { " # " })];
    header_cells.extend([
        Cell::from("Project"),
        Cell::from("Status"),
        Cell::from("Model/Ctx"),
        Cell::from("Last Activity"),
    ]);
    let header = Row::new(header_cells).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(display_idx, &real_idx)| {
            let session = &app.sessions[real_idx];
            let num = format!(" {} ", real_idx + 1);
            let is_active_row = display_idx == app.selected;
            let is_selected = display_idx == app.selected && session.status != SessionStatus::Input;
            // Brighten dim colors on selected row so they stay visible
            let dim = if is_selected {
                Color::Gray
            } else {
                Color::DarkGray
            };

            let tmux_name = session.tmux_session.as_deref().unwrap_or("—");

            // Status: colored dot + label
            let (status_dot, status_color) = match session.status {
                SessionStatus::New => ("●", Color::Blue),
                SessionStatus::Working => ("●", Color::Green),
                SessionStatus::Idle => ("●", dim),
                SessionStatus::Input => ("●", Color::Yellow),
                SessionStatus::BackgroundTasks(_) => ("●", BACKGROUND_TASK_COLOR),
                SessionStatus::BackgroundAgents(_) => ("●", BACKGROUND_TASK_COLOR),
            };
            let status_label = session.status.label();

            let token_ratio = session.token_ratio();
            let token_style = if token_ratio > 0.9 {
                Style::default().fg(Color::Red)
            } else if token_ratio > 0.75 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };

            let activity = session
                .last_activity
                .as_deref()
                .map(format_timestamp)
                .unwrap_or_else(|| "—".to_string());

            // Project spans two lines:
            //   line 1: repo::relative_dir
            //   line 2: session title, then branch (if any)
            // Each line is a styled-char sequence so it can marquee-scroll.
            let project_cell = {
                // Line 1: repo path
                let mut line1: Vec<StyledChar> = Vec::new();
                push_segment(&mut line1, &session.project_name, Style::default());
                if let Some(dir) = &session.relative_dir {
                    push_segment(&mut line1, "::", Style::default().fg(dim));
                    push_segment(&mut line1, dir, Style::default().fg(Color::Cyan));
                }

                // Line 2: session title followed by branch
                let mut line2: Vec<StyledChar> = Vec::new();
                let has_title = session.session_name.is_some();
                // Branch is highlighted white on the selected row, magenta otherwise
                let branch_color = if is_selected {
                    Color::White
                } else {
                    Color::Magenta
                };
                if let Some(name) = &session.session_name {
                    // Session title stays green even when selected
                    push_segment(&mut line2, name, Style::default().fg(Color::Green));
                }
                if has_title {
                    // With a title present, hide uninteresting default branches
                    if let Some(b) = visible_branch(&session.branch) {
                        push_segment(&mut line2, " ", Style::default());
                        push_segment(&mut line2, b, Style::default().fg(branch_color));
                    }
                } else if let Some(b) = &session.branch {
                    // No title: always show the branch, even main/master
                    push_segment(&mut line2, b, Style::default().fg(branch_color));
                } else {
                    // No title and no git repo
                    push_segment(&mut line2, "(no repo)", Style::default().fg(dim));
                }

                let tick = app.selected_scroll_tick();
                let w1 = window_content(&line1, project_width, tick, is_active_row);
                let w2 = window_content(&line2, project_width, tick, is_active_row);
                Cell::from(Text::from(vec![
                    Line::from(group_spans(&w1)),
                    Line::from(group_spans(&w2)),
                ]))
            };

            // Status: colored dot + label
            let status_cell = Cell::from(Line::from(vec![
                Span::styled(status_dot, Style::default().fg(status_color)),
                Span::styled(
                    format!(" {status_label}"),
                    Style::default().fg(status_color),
                ),
            ]));

            // Id column: row number on line 1, tmux session name on line 2
            let id_cell = if show_session_col {
                let name_style = if session.agent == crate::session::AgentKind::Codex {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                };
                Cell::from(Text::from(vec![
                    Line::from(num),
                    Line::from(Span::styled(format!(" {tmux_name}"), name_style)),
                ]))
            } else {
                Cell::from(num)
            };
            let mut cells = vec![id_cell];
            // Model/Context: model on line 1, token usage on line 2
            let model_cell = Cell::from(Text::from(vec![
                Line::from(session.model_display()),
                Line::from(Span::styled(session.token_display(), token_style)),
            ]));

            cells.extend([
                project_cell,
                status_cell,
                model_cell,
                Cell::from(activity),
            ]);
            let row = Row::new(cells).height(ROW_HEIGHT);

            if session.status == SessionStatus::Input {
                row.style(Style::default().bg(Color::Rgb(50, 40, 0)))
            } else if display_idx == app.selected {
                row.style(Style::default().bg(Color::Rgb(50, 50, 55)))
            } else {
                row
            }
        })
        .collect();

    // Id column holds the number, plus the session name on line 2 when shown
    let id_width = if show_session_col {
        SESSION_COLUMN_WIDTH
    } else {
        NUMBER_COLUMN_WIDTH
    };
    let mut widths = vec![Constraint::Length(id_width)];
    widths.extend([
        Constraint::Min(20),                        // Project (repo + branch)
        Constraint::Length(STATUS_COLUMN_WIDTH),    // Status
        Constraint::Length(MODEL_COLUMN_WIDTH),     // Model/Context
        Constraint::Length(ACTIVITY_COLUMN_WIDTH),  // Last Activity
    ]);

    // Scroll the row window with the cursor when rows overflow the viewport.
    // Viewport = area minus top/bottom borders (2) and the header line (1),
    // divided by the per-row height.
    let capacity = (area.height.saturating_sub(3) / ROW_HEIGHT) as usize;
    let offset = scroll_offset(app.selected, capacity, rows.len());
    let visible_rows: Vec<Row> = rows.into_iter().skip(offset).take(capacity.max(1)).collect();

    let table = Table::new(visible_rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" recon "));

    frame.render_widget(table, area);
}

/// Top row index of the scroll window so the selected row stays visible.
/// Anchors to the top until the cursor reaches the bottom edge, then scrolls
/// so the selected row is the last visible one.
fn scroll_offset(selected: usize, capacity: usize, total: usize) -> usize {
    if capacity == 0 || total <= capacity {
        return 0;
    }
    if selected < capacity {
        0
    } else {
        (selected + 1 - capacity).min(total - capacity)
    }
}

/// Estimate the rendered Project column width for budgeting the session title.
fn project_column_width(area_width: u16, show_session_col: bool) -> usize {
    // Id column width: wider when it also carries the session name on line 2
    let id_width = if show_session_col {
        SESSION_COLUMN_WIDTH
    } else {
        NUMBER_COLUMN_WIDTH
    };
    // Columns: id, project, status, model/ctx, activity
    let column_count = 5;
    let fixed_width = TABLE_BORDER_WIDTH
        + id_width
        + STATUS_COLUMN_WIDTH
        + MODEL_COLUMN_WIDTH
        + ACTIVITY_COLUMN_WIDTH
        + TABLE_COLUMN_SPACING * (column_count - 1);

    area_width.saturating_sub(fixed_width) as usize
}

/// A single rendered character paired with its style, used to scroll the
/// whole Project column while preserving per-segment colors.
type StyledChar = (char, Style);

/// Append each character of `text` to `buf` carrying `style`.
fn push_segment(buf: &mut Vec<StyledChar>, text: &str, style: Style) {
    for ch in text.chars() {
        buf.push((ch, style));
    }
}

/// Return the branch name unless it is a default branch worth hiding.
fn visible_branch(branch: &Option<String>) -> Option<&str> {
    branch
        .as_deref()
        .filter(|b| *b != "main" && *b != "master")
}

/// Window `content` to `width`, marquee-scrolling the active row when it
/// overflows and statically clipping every other row.
fn window_content(
    content: &[StyledChar],
    width: usize,
    tick: u64,
    is_active_row: bool,
) -> Vec<StyledChar> {
    if width == 0 {
        return Vec::new();
    }
    if content.len() <= width {
        return content.to_vec();
    }
    if !is_active_row {
        return content.iter().take(width).copied().collect();
    }

    // Loop the content past a styled separator for the active-row marquee.
    let separator_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut cycle = content.to_vec();
    cycle.push((' ', separator_style));
    cycle.push((SESSION_TITLE_SEPARATOR, separator_style));
    cycle.push((' ', separator_style));
    let cycle_len = cycle.len();
    let start = tick as usize % cycle_len;
    (0..width).map(|o| cycle[(start + o) % cycle_len]).collect()
}

/// Group consecutive same-style characters into spans for rendering.
fn group_spans(chars: &[StyledChar]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chunk = String::new();
    let mut chunk_style: Option<Style> = None;
    for &(ch, style) in chars {
        if chunk_style != Some(style) {
            if let Some(s) = chunk_style {
                spans.push(Span::styled(std::mem::take(&mut chunk), s));
            }
            chunk_style = Some(style);
        }
        chunk.push(ch);
    }
    if let Some(s) = chunk_style {
        spans.push(Span::styled(chunk, s));
    }
    spans
}

fn render_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled("/", Style::default().fg(Color::Cyan)),
        Span::raw(&app.filter_text),
    ];
    if !app.filter_active && !app.filter_text.is_empty() {
        let count = app.filtered_indices().len();
        spans.push(Span::styled(
            format!("  ({} match{})", count, if count == 1 { "" } else { "es" }),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);

    if app.filter_active {
        frame.set_cursor_position((area.x + 1 + app.filter_cursor as u16, area.y));
    }
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let spans = if app.filter_active {
        vec![
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::raw(" clear  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" keep filter  "),
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(" navigate"),
        ]
    } else {
        vec![
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw("/"),
            Span::styled("1-0", Style::default().fg(Color::Cyan)),
            Span::raw(" switch  "),
            Span::styled(
                if app.shift_enter_zoom { "S-Enter" } else { "C-j" },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(" zoom  "),
            Span::styled("b", Style::default().fg(Color::Cyan)),
            Span::raw(" back  "),
            Span::styled("x", Style::default().fg(Color::Cyan)),
            Span::raw(" kill  "),
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(" search  "),
            Span::styled("v", Style::default().fg(Color::Cyan)),
            Span::raw(" view  "),
            Span::styled("i", Style::default().fg(Color::Cyan)),
            Span::raw(" next input  "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" quit"),
        ]
    };
    let footer = Paragraph::new(Line::from(spans));
    frame.render_widget(footer, area);
}

/// Format an ISO timestamp into a relative or short time string.
fn format_timestamp(ts: &str) -> String {
    use chrono::{DateTime, Local, Utc};

    let parsed = ts.parse::<DateTime<Utc>>();
    match parsed {
        Ok(dt) => {
            let now = Utc::now();
            let diff = now - dt;

            if diff.num_seconds() < 60 {
                "< 1m".to_string()
            } else if diff.num_minutes() < 60 {
                format!("{}m ago", diff.num_minutes())
            } else if diff.num_hours() < 24 {
                format!("{}h ago", diff.num_hours())
            } else {
                dt.with_timezone(&Local).format("%b %d %H:%M").to_string()
            }
        }
        Err(_) => ts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a styled-char buffer from plain text for test assertions.
    fn styled(text: &str) -> Vec<StyledChar> {
        let mut buf = Vec::new();
        push_segment(&mut buf, text, Style::default());
        buf
    }

    /// Collapse a styled-char buffer back to plain text.
    fn plain(chars: &[StyledChar]) -> String {
        chars.iter().map(|(c, _)| *c).collect()
    }

    #[test]
    fn content_fits_without_scrolling() {
        let content = styled("short");
        assert_eq!(plain(&window_content(&content, 8, 42, true)), "short");
    }

    #[test]
    fn active_row_loops_with_separator_when_over_width() {
        let content = styled("abcdef");
        assert_eq!(plain(&window_content(&content, 4, 0, true)), "abcd");
        assert_eq!(plain(&window_content(&content, 4, 1, true)), "bcde");
        assert_eq!(plain(&window_content(&content, 4, 2, true)), "cdef");
        let long = styled("abcdefghij");
        assert_eq!(plain(&window_content(&long, 8, 8, true)), "ij • abc");
    }

    #[test]
    fn inactive_row_does_not_scroll() {
        let content = styled("abcdefghij");
        assert_eq!(plain(&window_content(&content, 4, 0, false)), "abcd");
        assert_eq!(plain(&window_content(&content, 4, 3, false)), "abcd");
        assert_eq!(plain(&window_content(&content, 4, 3, true)), "defg");
    }

    #[test]
    fn window_handles_zero_width() {
        let content = styled("abcdef");
        assert!(window_content(&content, 0, 3, true).is_empty());
    }

    #[test]
    fn scroll_offset_anchors_top_then_follows_cursor() {
        // Everything fits: never scroll.
        assert_eq!(scroll_offset(0, 5, 3), 0);
        assert_eq!(scroll_offset(2, 5, 3), 0);
        // Zero capacity (tiny viewport): no offset.
        assert_eq!(scroll_offset(9, 0, 20), 0);
        // Overflow, cursor still within first window: stay at top.
        assert_eq!(scroll_offset(0, 5, 20), 0);
        assert_eq!(scroll_offset(4, 5, 20), 0);
        // Cursor past the bottom edge: selected becomes the last visible row.
        assert_eq!(scroll_offset(5, 5, 20), 1);
        assert_eq!(scroll_offset(10, 5, 20), 6);
        // Cursor at the very end: clamp so the last window is full.
        assert_eq!(scroll_offset(19, 5, 20), 15);
    }

    #[test]
    fn visible_branch_hides_main_and_master() {
        assert_eq!(visible_branch(&Some("main".to_string())), None);
        assert_eq!(visible_branch(&Some("master".to_string())), None);
        assert_eq!(
            visible_branch(&Some("feature".to_string())),
            Some("feature")
        );
        assert_eq!(visible_branch(&None), None);
    }

    #[test]
    fn group_spans_merges_same_style_runs() {
        let mut buf = Vec::new();
        push_segment(&mut buf, "ab", Style::default().fg(Color::Cyan));
        push_segment(&mut buf, "cd", Style::default().fg(Color::Cyan));
        push_segment(&mut buf, "ef", Style::default().fg(Color::Green));
        let spans = group_spans(&buf);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "abcd");
        assert_eq!(spans[1].content, "ef");
    }
}
