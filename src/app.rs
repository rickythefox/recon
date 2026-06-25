use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::session::{self, Session};
use crate::state;
use crate::tmux;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewMode {
    Table,
    View,
}

pub struct App {
    pub sessions: Vec<Session>,
    pub selected: usize,
    pub should_quit: bool,
    pub view_mode: ViewMode,
    pub tick: u64,
    selected_changed_tick: u64,
    pub view_page: usize,
    pub view_zoomed_room: Option<String>, // room name when zoomed in
    pub view_zoom_index: Option<usize>,  // pending zoom request from key press
    pub view_selected_agent: usize,      // selected agent within zoomed room
    pub filter_active: bool,              // search input has focus
    pub filter_text: String,              // current search query
    pub filter_cursor: usize,             // cursor position in query
    last_session_id: Option<String>,      // restored from ~/.config/recon/state.json
    prev_session_id: Option<String>,      // for 'b' to toggle back
    prev_sessions: HashMap<String, Session>,
}

impl App {
    pub fn new() -> Self {
        let saved = state::load();
        App {
            sessions: Vec::new(),
            selected: 0,
            should_quit: false,
            view_mode: ViewMode::Table,
            tick: 0,
            selected_changed_tick: 0,
            view_page: 0,
            view_zoomed_room: None,
            view_zoom_index: None,
            view_selected_agent: 0,
            filter_active: false,
            filter_text: String::new(),
            filter_cursor: 0,
            last_session_id: saved.last_session_id,
            prev_session_id: saved.prev_session_id,
            prev_sessions: HashMap::new(),
        }
    }

    pub fn refresh(&mut self) {
        let sessions: Vec<Session> = session::discover_sessions(&self.prev_sessions)
            .into_iter()
            .filter(|s| s.tmux_session.is_some())
            .collect();

        self.prev_sessions = sessions
            .iter()
            .map(|s| (s.session_id.clone(), s.clone()))
            .collect();

        self.sessions = sessions;

        let count = self.filtered_indices().len();
        if count == 0 {
            self.set_selected(0);
        } else if self.selected >= count {
            self.set_selected(count - 1);
        }
    }

    pub fn advance_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn selected_scroll_tick(&self) -> u64 {
        self.tick.saturating_sub(self.selected_changed_tick)
    }

    fn set_selected(&mut self, selected: usize) {
        if self.selected != selected {
            self.selected = selected;
            self.selected_changed_tick = self.tick;
        }
    }

    fn reset_selected_scroll(&mut self) {
        self.selected_changed_tick = self.tick;
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter_text.is_empty() {
            return (0..self.sessions.len()).collect();
        }
        let query = self.filter_text.to_lowercase();
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.project_name.to_lowercase().contains(&query)
                    || s.tmux_session
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                    || s.session_name
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn clamp_selection(&mut self) {
        let count = self.filtered_indices().len();
        if count == 0 {
            self.set_selected(0);
        } else if self.selected >= count {
            self.set_selected(count - 1);
        }
    }

    /// Resolve filtered index to real session index.
    fn resolve_selected(&self) -> Option<usize> {
        let indices = self.filtered_indices();
        indices.get(self.selected).copied()
    }

    /// Restore selection to the last-used session after refresh.
    pub fn restore_selection(&mut self) {
        if let Some(ref saved_id) = self.last_session_id {
            let filtered = self.filtered_indices();
            for (display_idx, &real_idx) in filtered.iter().enumerate() {
                if self.sessions[real_idx].session_id == *saved_id {
                    self.set_selected(display_idx);
                    return;
                }
            }
        }
    }

    /// Switch to the session at the given real index, save state, and quit.
    fn switch_to_session(&mut self, real_idx: usize) {
        self.switch_to_session_inner(real_idx, false);
    }

    /// Switch to the session and zoom its tmux pane (if not already zoomed).
    fn switch_to_session_zoomed(&mut self, real_idx: usize) {
        self.switch_to_session_inner(real_idx, true);
    }

    fn switch_to_session_inner(&mut self, real_idx: usize, zoom: bool) {
        if let Some(session) = self.sessions.get(real_idx) {
            if let Some(target) = &session.pane_target {
                state::save(&session.session_id, self.last_session_id.as_deref());
                if zoom {
                    tmux::zoom_pane(target);
                }
                tmux::switch_to_pane(target);
                self.should_quit = true;
            }
        }
    }

    /// Save the currently highlighted session to state.
    fn save_selected_state(&self) {
        if let Some(real_idx) = self.resolve_selected() {
            if let Some(session) = self.sessions.get(real_idx) {
                state::save(&session.session_id, self.last_session_id.as_deref());
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.filter_active {
            self.handle_key_filter(key);
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::Char('i')) {
            self.jump_to_next_input();
            return;
        }
        match self.view_mode {
            ViewMode::Table => self.handle_key_table(key),
            ViewMode::View => self.handle_key_view(key),
        }
    }

    fn jump_to_next_input(&mut self) {
        if let Some(session) = self.sessions.iter().find(|s| s.status == session::SessionStatus::Input) {
            if let Some(target) = &session.pane_target {
                state::save(&session.session_id, self.last_session_id.as_deref());
                tmux::switch_to_pane(target);
                self.should_quit = true;
            }
        }
    }

    fn handle_key_table(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => {
                self.save_selected_state();
                self.should_quit = true;
            }
            KeyCode::Esc => {
                if !self.filter_text.is_empty() {
                    self.filter_text.clear();
                    self.set_selected(0);
                    self.reset_selected_scroll();
                } else {
                    self.save_selected_state();
                    self.should_quit = true;
                }
            }
            KeyCode::Char('/') => {
                self.filter_active = true;
                self.filter_text.clear();
                self.filter_cursor = 0;
                self.set_selected(0);
                self.reset_selected_scroll();
            }
            KeyCode::Char('v') => self.view_mode = ViewMode::View,
            // Ctrl+J (what the terminal's Shift+Enter remap emits): jump to the
            // pane and zoom it. Must precede the plain 'j' navigation arm.
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(real_idx) = self.resolve_selected() {
                    self.switch_to_session_zoomed(real_idx);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let count = self.filtered_indices().len();
                if count > 0 {
                    self.set_selected((self.selected + 1).min(count - 1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected > 0 {
                    self.set_selected(self.selected - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(real_idx) = self.resolve_selected() {
                    self.switch_to_session(real_idx);
                }
            }
            // Digit keys: switch to session by displayed # column number
            KeyCode::Char(c @ '1'..='9') => {
                let target_num = (c as usize) - ('0' as usize);
                let filtered = self.filtered_indices();
                if let Some(&real_idx) = filtered.iter().find(|&&ri| ri + 1 == target_num) {
                    self.switch_to_session(real_idx);
                }
            }
            KeyCode::Char('0') => {
                let filtered = self.filtered_indices();
                if let Some(&real_idx) = filtered.iter().find(|&&ri| ri + 1 == 10) {
                    self.switch_to_session(real_idx);
                }
            }
            KeyCode::Char('b') => {
                if let Some(ref prev_id) = self.prev_session_id.clone() {
                    let filtered = self.filtered_indices();
                    if let Some(&real_idx) = filtered.iter().find(|&&ri| self.sessions[ri].session_id == *prev_id) {
                        self.switch_to_session(real_idx);
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(real_idx) = self.resolve_selected() {
                    if let Some(session) = self.sessions.get(real_idx) {
                        if let Some(name) = &session.tmux_session {
                            tmux::kill_session(name);
                            self.refresh();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_key_view(&mut self, key: KeyEvent) {
        // Agent interaction keys (only when zoomed into a room)
        if self.view_zoomed_room.is_some() {
            match key.code {
                KeyCode::Char('l') | KeyCode::Right => {
                    self.view_selected_agent = self.view_selected_agent.saturating_add(1);
                    return;
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.view_selected_agent = self.view_selected_agent.saturating_sub(1);
                    return;
                }
                KeyCode::Enter => {
                    if let Some(session) = self.selected_zoomed_session() {
                        if let Some(target) = session.pane_target.clone() {
                            state::save(&session.session_id.clone(), self.last_session_id.as_deref());
                            tmux::switch_to_pane(&target);
                            self.should_quit = true;
                        }
                    }
                    return;
                }
                KeyCode::Char('x') => {
                    if let Some(session) = self.selected_zoomed_session() {
                        if let Some(name) = session.tmux_session.clone() {
                            tmux::kill_session(&name);
                            self.refresh();
                        }
                    }
                    return;
                }
                KeyCode::Char('n') => {
                    if let Some(cwd) = self.zoomed_room_cwd() {
                        let default_name = std::path::Path::new(&cwd)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "claude".to_string());
                        if let Ok(name) = tmux::create_session(&default_name, &cwd, None, &[], &crate::session::AgentKind::Claude) {
                            tmux::switch_to_pane(&name);
                            self.should_quit = true;
                        }
                    }
                    return;
                }
                _ => {} // fall through to shared keys
            }
        }

        match key.code {
            KeyCode::Char('/') => {
                self.filter_active = true;
                self.filter_text.clear();
                self.filter_cursor = 0;
                self.set_selected(0);
                self.reset_selected_scroll();
            }
            KeyCode::Char('q') => {
                self.save_selected_state();
                self.should_quit = true;
            }
            KeyCode::Esc => {
                if self.view_zoomed_room.is_some() {
                    self.view_zoomed_room = None;
                    self.view_selected_agent = 0;
                } else if !self.filter_text.is_empty() {
                    self.filter_text.clear();
                    self.set_selected(0);
                    self.reset_selected_scroll();
                } else {
                    self.save_selected_state();
                    self.should_quit = true;
                }
            }
            KeyCode::Char('v') => {
                self.view_zoomed_room = None;
                self.view_selected_agent = 0;
                self.view_mode = ViewMode::Table;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.view_page = self.view_page.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.view_page = self.view_page.saturating_sub(1);
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                self.view_zoom_index = Some(idx);
                self.view_selected_agent = 0;
            }
            _ => {}
        }
    }

    fn handle_key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter_active = false;
                self.filter_text.clear();
                self.filter_cursor = 0;
                self.set_selected(0);
                self.reset_selected_scroll();
            }
            KeyCode::Enter => {
                let indices = self.filtered_indices();
                if indices.len() == 1 {
                    if let Some(session) = self.sessions.get(indices[0]) {
                        if let Some(target) = &session.pane_target {
                            state::save(&session.session_id, self.last_session_id.as_deref());
                            tmux::switch_to_pane(target);
                            self.should_quit = true;
                            return;
                        }
                    }
                }
                self.filter_active = false;
            }
            KeyCode::Backspace => {
                if self.filter_cursor > 0 {
                    let byte_pos = self.filter_text.char_indices()
                        .nth(self.filter_cursor - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let next_byte = self.filter_text.char_indices()
                        .nth(self.filter_cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(self.filter_text.len());
                    self.filter_text.replace_range(byte_pos..next_byte, "");
                    self.filter_cursor -= 1;
                    self.clamp_selection();
                    self.reset_selected_scroll();
                }
            }
            KeyCode::Delete => {
                let char_count = self.filter_text.chars().count();
                if self.filter_cursor < char_count {
                    let byte_pos = self.filter_text.char_indices()
                        .nth(self.filter_cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(self.filter_text.len());
                    let next_byte = self.filter_text.char_indices()
                        .nth(self.filter_cursor + 1)
                        .map(|(i, _)| i)
                        .unwrap_or(self.filter_text.len());
                    self.filter_text.replace_range(byte_pos..next_byte, "");
                    self.clamp_selection();
                    self.reset_selected_scroll();
                }
            }
            KeyCode::Left => {
                if self.filter_cursor > 0 {
                    self.filter_cursor -= 1;
                }
            }
            KeyCode::Right => {
                let char_count = self.filter_text.chars().count();
                if self.filter_cursor < char_count {
                    self.filter_cursor += 1;
                }
            }
            KeyCode::Home => self.filter_cursor = 0,
            KeyCode::End => self.filter_cursor = self.filter_text.chars().count(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_cursor = 0;
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_cursor = self.filter_text.chars().count();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_text.clear();
                self.filter_cursor = 0;
                self.clamp_selection();
                self.reset_selected_scroll();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.filtered_indices().len();
                if count > 0 {
                    self.set_selected((self.selected + 1).min(count - 1));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.set_selected(self.selected - 1);
                }
            }
            KeyCode::Tab | KeyCode::Char('i') => {
                self.jump_to_next_input();
            }
            KeyCode::Char(c) => {
                let byte_pos = self.filter_text.char_indices()
                    .nth(self.filter_cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(self.filter_text.len());
                self.filter_text.insert(byte_pos, c);
                self.filter_cursor += 1;
                self.clamp_selection();
                self.reset_selected_scroll();
            }
            _ => {}
        }
    }

    fn zoomed_room_session_indices(&self) -> Vec<usize> {
        let Some(ref room_name) = self.view_zoomed_room else {
            return Vec::new();
        };
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let name = if s.project_name.is_empty() {
                    "unknown".to_string()
                } else {
                    s.room_id()
                };
                &name == room_name
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_zoomed_session(&self) -> Option<&Session> {
        let indices = self.zoomed_room_session_indices();
        if indices.is_empty() {
            return None;
        }
        let clamped = self.view_selected_agent.min(indices.len() - 1);
        self.sessions.get(indices[clamped])
    }

    fn zoomed_room_cwd(&self) -> Option<String> {
        self.selected_zoomed_session().map(|s| s.cwd.clone())
    }

    pub fn to_json(&self, tag_filters: &[String]) -> String {
        // Parse tag filters into key:value pairs
        let filters: Vec<(&str, &str)> = tag_filters
            .iter()
            .filter_map(|t| t.split_once(':'))
            .collect();

        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                filters.iter().all(|(k, v)| {
                    s.tags.get(*k).map_or(false, |tv| tv == v)
                })
            })
            .map(|(i, s)| {
                serde_json::json!({
                    "index": i + 1,
                    "session_id": s.session_id,
                    "project_name": s.project_name,
                    "branch": s.branch,
                    "cwd": s.cwd,
                    "room_id": s.room_id(),
                    "relative_dir": s.relative_dir,
                    "tmux_session": s.tmux_session,
                    "pane_target": s.pane_target,
                    "model": s.model,
                    "model_display": s.model_display(),
                    "total_input_tokens": s.total_input_tokens,
                    "total_output_tokens": s.total_output_tokens,
                    "context_display": s.token_display(),
                    "token_ratio": s.token_ratio(),
                    "status": s.status.label(),
                    "pid": s.pid,
                    "last_activity": s.last_activity,
                    "started_at": s.started_at,
                    "tags": s.tags,
                    "session_name": s.session_name,
                    "agent": match s.agent {
                        crate::session::AgentKind::Claude => "claude",
                        crate::session::AgentKind::Codex => "codex",
                    },
                })
            })
            .collect();

        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": sessions,
        }))
        .unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_session(id: &str) -> Session {
        Session {
            session_id: id.to_string(),
            project_name: "recon".to_string(),
            branch: None,
            cwd: "/tmp".to_string(),
            relative_dir: None,
            tmux_session: Some(format!("tmux-{id}")),
            pane_target: None,
            model: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            status: session::SessionStatus::Idle,
            pid: None,
            effort: None,
            last_activity: None,
            started_at: 0,
            jsonl_path: PathBuf::new(),
            last_file_size: 0,
            tags: HashMap::new(),
            session_name: Some("long session title".to_string()),
            agent: session::AgentKind::Claude,
            context_window: None,
        }
    }

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn changing_table_selection_resets_scroll_tick() {
        let mut app = App::new();
        app.sessions = vec![make_session("a"), make_session("b")];
        app.tick = 10;

        assert_eq!(app.selected_scroll_tick(), 10);

        app.handle_key(make_key(KeyCode::Down));

        assert_eq!(app.selected, 1);
        assert_eq!(app.selected_scroll_tick(), 0);

        app.advance_tick();
        assert_eq!(app.selected_scroll_tick(), 1);

        app.handle_key(make_key(KeyCode::Up));

        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_scroll_tick(), 0);
    }
}
