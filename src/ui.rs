use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::session::SessionStatus;

const TABLE_BORDER_WIDTH: u16 = 2;
const TABLE_COLUMN_SPACING: u16 = 1;
const NUMBER_COLUMN_WIDTH: u16 = 4;
const SESSION_COLUMN_WIDTH: u16 = 4;
const STATUS_COLUMN_WIDTH: u16 = 11;
const MODEL_COLUMN_WIDTH: u16 = 11;
const CONTEXT_COLUMN_WIDTH: u16 = 11;
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

    let mut header_cells = vec![Cell::from(" # ")];
    if show_session_col {
        header_cells.push(Cell::from("Sess"));
    }
    header_cells.extend([
        Cell::from("Project"),
        Cell::from("Status"),
        Cell::from("Model"),
        Cell::from("Context"),
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

            // Project: repo::relative_dir::branch (session name)
            let project_cell = {
                let mut spans = vec![Span::raw(&session.project_name)];
                if let Some(dir) = &session.relative_dir {
                    spans.push(Span::styled("::", Style::default().fg(dim)));
                    spans.push(Span::styled(dir.clone(), Style::default().fg(Color::Cyan)));
                }
                if let Some(b) = &session.branch {
                    spans.push(Span::styled("::", Style::default().fg(dim)));
                    spans.push(Span::styled(b, Style::default().fg(Color::Green)));
                }
                if let Some(name) = &session.session_name {
                    let title_budget = session_title_budget(session, project_width);
                    let name_color = if is_selected {
                        Color::White
                    } else {
                        Color::Magenta
                    };
                    if title_budget > 0 {
                        spans.extend(session_title_spans(
                            name,
                            title_budget,
                            app.selected_scroll_tick(),
                            name_color,
                            is_active_row,
                        ));
                    }
                }
                Cell::from(Line::from(spans))
            };

            // Status: colored dot + label
            let status_cell = Cell::from(Line::from(vec![
                Span::styled(status_dot, Style::default().fg(status_color)),
                Span::styled(
                    format!(" {status_label}"),
                    Style::default().fg(status_color),
                ),
            ]));

            let mut cells = vec![Cell::from(num)];
            if show_session_col {
                cells.push(Cell::from(Span::styled(
                    tmux_name.to_string(),
                    if session.agent == crate::session::AgentKind::Codex {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    },
                )));
            }
            cells.extend([
                project_cell,
                status_cell,
                Cell::from(session.model_display()),
                Cell::from(session.token_display()).style(token_style),
                Cell::from(activity),
            ]);
            let row = Row::new(cells);

            if session.status == SessionStatus::Input {
                row.style(Style::default().bg(Color::Rgb(50, 40, 0)))
            } else if display_idx == app.selected {
                row.style(Style::default().bg(Color::Rgb(50, 50, 55)))
            } else {
                row
            }
        })
        .collect();

    let mut widths = vec![Constraint::Length(NUMBER_COLUMN_WIDTH)]; // #
    if show_session_col {
        widths.push(Constraint::Length(SESSION_COLUMN_WIDTH)); // Session
    }
    widths.extend([
        Constraint::Min(20),                        // Project (repo + branch)
        Constraint::Length(STATUS_COLUMN_WIDTH),    // Status
        Constraint::Length(MODEL_COLUMN_WIDTH),     // Model
        Constraint::Length(CONTEXT_COLUMN_WIDTH),   // Context
        Constraint::Length(ACTIVITY_COLUMN_WIDTH),  // Last Activity
    ]);

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" recon "));

    frame.render_widget(table, area);
}

/// Estimate the rendered Project column width for budgeting the session title.
fn project_column_width(area_width: u16, show_session_col: bool) -> usize {
    let session_width = if show_session_col {
        SESSION_COLUMN_WIDTH
    } else {
        0
    };
    let column_count = if show_session_col { 7 } else { 6 };
    let fixed_width = TABLE_BORDER_WIDTH
        + NUMBER_COLUMN_WIDTH
        + session_width
        + STATUS_COLUMN_WIDTH
        + MODEL_COLUMN_WIDTH
        + CONTEXT_COLUMN_WIDTH
        + ACTIVITY_COLUMN_WIDTH
        + TABLE_COLUMN_SPACING * (column_count - 1);

    area_width.saturating_sub(fixed_width) as usize
}

/// Return the number of title characters that can fit after project metadata.
fn session_title_budget(session: &crate::session::Session, project_width: usize) -> usize {
    let mut used = display_len(&session.project_name);
    if let Some(dir) = &session.relative_dir {
        used += 2 + display_len(dir);
    }
    if let Some(branch) = &session.branch {
        used += 2 + display_len(branch);
    }

    project_width.saturating_sub(used + 3)
}

/// Scroll a session title through a fixed-width viewport.
fn scroll_session_title(title: &str, width: usize, tick: u64) -> String {
    if width == 0 {
        return String::new();
    }

    let title_chars: Vec<char> = title.chars().collect();
    if title_chars.len() <= width {
        return title.to_string();
    }

    let mut cycle_chars = title_chars;
    cycle_chars.extend([' ', SESSION_TITLE_SEPARATOR, ' ']);
    let cycle_len = cycle_chars.len();
    let start = tick as usize % cycle_len;
    let mut output = String::new();
    for offset in 0..width {
        let pos = (start + offset) % cycle_len;
        output.push(cycle_chars[pos]);
    }

    output
}

/// Build title spans while coloring the loop separator independently.
fn session_title_spans(
    title: &str,
    width: usize,
    tick: u64,
    title_color: Color,
    is_active_row: bool,
) -> Vec<Span<'static>> {
    let title = session_title_text(title, width, tick, is_active_row);
    let title_style = Style::default().fg(title_color);
    let separator_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(" (", title_style)];
    let mut chunk = String::new();

    for ch in title.chars() {
        if ch == SESSION_TITLE_SEPARATOR {
            if !chunk.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut chunk), title_style));
            }
            spans.push(Span::styled(ch.to_string(), separator_style));
        } else {
            chunk.push(ch);
        }
    }

    if !chunk.is_empty() {
        spans.push(Span::styled(chunk, title_style));
    }
    spans.push(Span::styled(")", title_style));
    spans
}

/// Return the active-row marquee text or an inactive static clipped title.
fn session_title_text(title: &str, width: usize, tick: u64, is_active_row: bool) -> String {
    if is_active_row {
        scroll_session_title(title, width, tick)
    } else {
        title.chars().take(width).collect()
    }
}

/// Count display characters for the ASCII-first labels this UI renders.
fn display_len(text: &str) -> usize {
    text.chars().count()
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

    #[test]
    fn session_title_fits_without_scrolling() {
        assert_eq!(scroll_session_title("short", 8, 42), "short");
    }

    #[test]
    fn session_title_loops_with_separator_when_over_budget() {
        assert_eq!(scroll_session_title("abcdef", 4, 0), "abcd");
        assert_eq!(scroll_session_title("abcdef", 4, 1), "bcde");
        assert_eq!(scroll_session_title("abcdef", 4, 2), "cdef");
        assert_eq!(scroll_session_title("abcdefghij", 8, 8), "ij • abc");
    }

    #[test]
    fn inactive_session_title_does_not_scroll() {
        assert_eq!(session_title_text("abcdefghij", 4, 0, false), "abcd");
        assert_eq!(session_title_text("abcdefghij", 4, 3, false), "abcd");
        assert_eq!(session_title_text("abcdefghij", 4, 3, true), "defg");
    }

    #[test]
    fn session_title_handles_zero_width() {
        assert_eq!(scroll_session_title("abcdef", 0, 3), "");
    }
}
