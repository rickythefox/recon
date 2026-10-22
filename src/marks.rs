//! User marks on sessions, keyed by stable conversation id and persisted to
//! `~/.local/state/recon/marks.json` as
//! `{ "<stable_id>": { "marked_at": "<RFC3339>", "session": { <Snapshot> } } }`.
//! The snapshot (agent, folder, title, ...) is refreshed while the session runs,
//! so a marked session stays listed - and resumable - after its agent exits.
//! A mark expires `TTL_DAYS` after it was set.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::session::{AgentKind, Session, SessionStatus};

const TTL_DAYS: i64 = 30;

/// What a marked session needs to be listed and resumed once its agent exits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub agent: AgentKind,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// tmux session name, reused when resuming
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    /// User-renamed tmux window (automatic names are not kept)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Snapshot {
    pub fn of(s: &Session) -> Self {
        Snapshot {
            agent: s.agent.clone(),
            cwd: s.cwd.clone(),
            title: s.session_name.clone(),
            tmux_session: s.tmux_session.clone(),
            window: s
                .tmux_window
                .clone()
                .filter(|w| !s.window_automatic_rename && !w.is_empty()),
            project_name: s.project_name.clone(),
            relative_dir: s.relative_dir.clone(),
            branch: s.branch.clone(),
            model: s.model.clone(),
        }
    }

    /// Table row for the conversation `id` while no agent is running it.
    /// It has no tmux session or pane, so kill/switch actions cannot hit a
    /// live session that happens to reuse the stored name.
    fn closed_session(&self, id: &str) -> Session {
        Session {
            session_id: id.to_string(),
            project_name: self.project_name.clone(),
            branch: self.branch.clone(),
            cwd: self.cwd.clone(),
            relative_dir: self.relative_dir.clone(),
            tmux_session: None,
            tmux_window: self.window.clone(),
            window_automatic_rename: false,
            pane_target: None,
            pane_id: None,
            model: self.model.clone(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            status: SessionStatus::Closed,
            pid: None,
            effort: None,
            last_activity: None,
            started_at: 0,
            jsonl_path: PathBuf::new(),
            last_file_size: 0,
            tags: HashMap::new(),
            recorded_session_name: self.title.clone(),
            session_name: self.title.clone(),
            agent: self.agent.clone(),
            context_window: None,
        }
    }
}

struct Mark {
    marked_at: DateTime<Utc>,
    // None for entries written before snapshots existed, until seen live again
    snapshot: Option<Snapshot>,
}

type Entries = HashMap<String, Mark>;

pub struct Marks {
    path: Option<PathBuf>,
    entries: Entries,
    // Modification time of the file as last read or written; None forces a read.
    mtime: Option<SystemTime>,
}

impl Marks {
    /// Create an unloaded mark set; `sync` performs the first read.
    pub fn new(path: Option<PathBuf>) -> Self {
        Marks {
            path,
            entries: Entries::new(),
            mtime: None,
        }
    }

    /// Default location next to `parked.json`.
    pub fn default_path() -> Option<PathBuf> {
        crate::state::state_dir().map(|d| d.join("marks.json"))
    }

    /// Reload when the file changed on disk, pruning expired entries from it.
    pub fn sync(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime == self.mtime {
            return Ok(());
        }
        let mut entries = read_entries(path)?;
        if prune(&mut entries, Utc::now()) {
            self.mtime = Some(write_entries(path, &entries)?);
        } else {
            self.mtime = mtime;
        }
        self.entries = entries;
        Ok(())
    }

    /// Toggle the mark for `id` against the current file contents, storing
    /// `snapshot` when marking. Returns whether `id` is marked afterwards.
    pub fn toggle(&mut self, id: &str, snapshot: Snapshot) -> Result<bool, String> {
        let path = self
            .path
            .as_ref()
            .ok_or("Could not determine home directory for marks")?;
        // Re-read so marks written by another dashboard are preserved
        let mut entries = read_entries(path)?;
        let now = Utc::now();
        prune(&mut entries, now);
        let marked = entries.remove(id).is_none();
        if marked {
            entries.insert(
                id.to_string(),
                Mark {
                    marked_at: now,
                    snapshot: Some(snapshot),
                },
            );
        }
        self.mtime = Some(write_entries(path, &entries)?);
        self.entries = entries;
        Ok(marked)
    }

    /// Refresh stored snapshots of marked live sessions (title, branch, ...),
    /// writing only when one changed.
    pub fn record(&mut self, live: &[Session]) -> Result<(), String> {
        let changed: Vec<(&str, Snapshot)> = live
            .iter()
            .filter_map(|s| {
                let id = s.stable_id()?;
                let mark = self.entries.get(id)?;
                let snapshot = Snapshot::of(s);
                (mark.snapshot.as_ref() != Some(&snapshot)).then_some((id, snapshot))
            })
            .collect();
        if changed.is_empty() {
            return Ok(());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        // Re-read so marks toggled by another dashboard are neither lost nor revived
        let mut entries = read_entries(path)?;
        prune(&mut entries, Utc::now());
        for (id, snapshot) in changed {
            if let Some(mark) = entries.get_mut(id) {
                mark.snapshot = Some(snapshot);
            }
        }
        self.mtime = Some(write_entries(path, &entries)?);
        self.entries = entries;
        Ok(())
    }

    /// Rows for marked conversations not running in `live`, oldest mark first.
    /// Marks without a snapshot are skipped: there is nothing to resume from.
    pub fn closed_sessions(&self, live: &[Session]) -> Vec<Session> {
        let live_ids: HashSet<&str> = live.iter().filter_map(|s| s.stable_id()).collect();
        let now = Utc::now();
        let mut closed: Vec<(&String, &Mark, &Snapshot)> = self
            .entries
            .iter()
            .filter(|(id, mark)| {
                !is_expired(mark.marked_at, now) && !live_ids.contains(id.as_str())
            })
            .filter_map(|(id, mark)| Some((id, mark, mark.snapshot.as_ref()?)))
            .collect();
        closed.sort_by(|a, b| a.1.marked_at.cmp(&b.1.marked_at).then_with(|| a.0.cmp(b.0)));
        closed
            .into_iter()
            .map(|(id, _, snapshot)| snapshot.closed_session(id))
            .collect()
    }

    /// Stored snapshot for `id`, if marked.
    pub fn snapshot(&self, id: &str) -> Option<&Snapshot> {
        self.entries.get(id)?.snapshot.as_ref()
    }

    /// Whether `id` carries an unexpired mark.
    pub fn is_marked(&self, id: &str) -> bool {
        self.entries
            .get(id)
            .is_some_and(|m| !is_expired(m.marked_at, Utc::now()))
    }
}

fn is_expired(marked_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - marked_at >= Duration::days(TTL_DAYS)
}

/// Drop expired entries; returns whether anything was removed.
fn prune(entries: &mut Entries, now: DateTime<Utc>) -> bool {
    let before = entries.len();
    entries.retain(|_, m| !is_expired(m.marked_at, now));
    entries.len() != before
}

/// On-disk entry. A bare timestamp string is the pre-snapshot format.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredMark {
    Full {
        marked_at: String,
        #[serde(default)]
        session: Option<Snapshot>,
    },
    Legacy(String),
}

#[derive(Serialize)]
struct StoredMarkOut<'a> {
    marked_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a Snapshot>,
}

/// Read the mark file; a missing file is an empty set, a malformed one an error.
fn read_entries(path: &Path) -> Result<Entries, String> {
    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    let raw: HashMap<String, StoredMark> =
        serde_json::from_str(&data).map_err(|e| format!("Malformed {}: {e}", path.display()))?;
    raw.into_iter()
        .map(|(id, stored)| {
            let (at, snapshot) = match stored {
                StoredMark::Full { marked_at, session } => (marked_at, session),
                StoredMark::Legacy(marked_at) => (marked_at, None),
            };
            DateTime::parse_from_rfc3339(&at)
                .map(|t| {
                    let marked_at = t.with_timezone(&Utc);
                    (
                        id,
                        Mark {
                            marked_at,
                            snapshot,
                        },
                    )
                })
                .map_err(|e| format!("Malformed timestamp in {}: {e}", path.display()))
        })
        .collect()
}

/// Write via temp file + rename so readers never see a partial file.
/// Returns the new file's modification time.
fn write_entries(path: &Path, entries: &Entries) -> Result<SystemTime, String> {
    let err = |e: std::io::Error| format!("Failed to write {}: {e}", path.display());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(err)?;
    }
    let raw: HashMap<&str, StoredMarkOut> = entries
        .iter()
        .map(|(id, m)| {
            let out = StoredMarkOut {
                marked_at: m.marked_at.to_rfc3339(),
                session: m.snapshot.as_ref(),
            };
            (id.as_str(), out)
        })
        .collect();
    let json = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&tmp, json).map_err(err)?;
    fs::rename(&tmp, path).map_err(err)?;
    fs::metadata(path).and_then(|m| m.modified()).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fresh per-test path under the system temp dir
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recon-marks-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("marks.json")
    }

    fn snapshot(title: &str) -> Snapshot {
        Snapshot {
            agent: AgentKind::Omp,
            cwd: "/src/app".to_string(),
            title: Some(title.to_string()),
            tmux_session: Some("app".to_string()),
            window: None,
            project_name: "app".to_string(),
            relative_dir: None,
            branch: Some("feature".to_string()),
            model: None,
        }
    }

    // A running OMP session for conversation `id`
    fn live(id: &str, title: &str) -> Session {
        let mut s = snapshot(title).closed_session(id);
        s.status = SessionStatus::Idle;
        s.tmux_session = Some("app".to_string());
        s
    }

    #[test]
    fn mark_persists_across_instances_and_unmarks() {
        let path = temp_path("persist");
        let mut a = Marks::new(Some(path.clone()));
        assert!(a.toggle("conv-1", snapshot("t")).unwrap());

        // A relaunched dashboard sees the mark
        let mut b = Marks::new(Some(path.clone()));
        b.sync().unwrap();
        assert!(b.is_marked("conv-1"));

        // Second press removes it, visible to the other instance on sync
        assert!(!b.toggle("conv-1", snapshot("t")).unwrap());
        a.mtime = None;
        a.sync().unwrap();
        assert!(!a.is_marked("conv-1"));
    }

    #[test]
    fn toggle_keeps_marks_written_by_another_dashboard() {
        let path = temp_path("concurrent");
        let mut a = Marks::new(Some(path.clone()));
        let mut b = Marks::new(Some(path.clone()));
        a.sync().unwrap();
        b.sync().unwrap();
        a.toggle("from-a", snapshot("a")).unwrap();
        b.toggle("from-b", snapshot("b")).unwrap();
        assert!(b.is_marked("from-a"));
        assert!(b.is_marked("from-b"));
    }

    #[test]
    fn marked_session_is_listed_as_closed_after_its_agent_exits() {
        let path = temp_path("closed");
        let mut a = Marks::new(Some(path.clone()));
        a.toggle("conv-1", snapshot("first title")).unwrap();
        // While running, the latest title is recorded
        a.record(&[live("conv-1", "renamed")]).unwrap();
        assert!(a.closed_sessions(&[live("conv-1", "renamed")]).is_empty());

        // After the agent exits, a relaunched dashboard still lists it
        let mut b = Marks::new(Some(path.clone()));
        b.sync().unwrap();
        let closed = b.closed_sessions(&[]);
        assert_eq!(closed.len(), 1);
        let row = &closed[0];
        assert_eq!(row.status, SessionStatus::Closed);
        assert_eq!(row.stable_id(), Some("conv-1"));
        assert_eq!(row.agent, AgentKind::Omp);
        assert_eq!(row.cwd, "/src/app");
        assert_eq!(row.session_name.as_deref(), Some("renamed"));
        // No tmux handle, so kill/switch can't touch a reused session name
        assert_eq!(row.tmux_session, None);
        assert_eq!(
            b.snapshot("conv-1").unwrap().tmux_session.as_deref(),
            Some("app")
        );
    }

    #[test]
    fn record_does_not_mark_or_revive_unmarked_sessions() {
        let path = temp_path("record");
        let mut a = Marks::new(Some(path.clone()));
        let mut b = Marks::new(Some(path.clone()));
        a.toggle("conv-1", snapshot("t")).unwrap();
        b.sync().unwrap();
        // Another dashboard unmarks while `b` still holds the old entry
        a.toggle("conv-1", snapshot("t")).unwrap();
        b.record(&[live("conv-1", "new title"), live("conv-2", "x")])
            .unwrap();
        assert!(read_entries(&path).unwrap().is_empty());
    }

    #[test]
    fn legacy_timestamp_entries_stay_marked_without_closed_row() {
        let path = temp_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let at = Utc::now().to_rfc3339();
        fs::write(&path, format!(r#"{{"conv-1":"{at}"}}"#)).unwrap();

        let mut m = Marks::new(Some(path.clone()));
        m.sync().unwrap();
        assert!(m.is_marked("conv-1"));
        assert!(m.closed_sessions(&[]).is_empty());
        // Seen live once, it gains a snapshot and becomes resumable
        m.record(&[live("conv-1", "t")]).unwrap();
        assert_eq!(m.closed_sessions(&[]).len(), 1);
    }

    #[test]
    fn expired_marks_are_hidden_and_pruned_on_load() {
        let path = temp_path("expiry");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let now = Utc::now();
        let old = (now - Duration::days(TTL_DAYS)).to_rfc3339();
        let fresh = (now - Duration::days(TTL_DAYS - 1)).to_rfc3339();
        fs::write(&path, format!(r#"{{"old":"{old}","fresh":"{fresh}"}}"#)).unwrap();

        let mut m = Marks::new(Some(path.clone()));
        m.sync().unwrap();
        assert!(!m.is_marked("old"));
        assert!(m.is_marked("fresh"));
        let on_disk = read_entries(&path).unwrap();
        assert!(!on_disk.contains_key("old"));
        assert!(on_disk.contains_key("fresh"));
    }

    #[test]
    fn malformed_file_is_reported_and_not_overwritten() {
        let path = temp_path("malformed");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();

        let mut m = Marks::new(Some(path.clone()));
        assert!(m.sync().is_err());
        assert!(m.toggle("x", snapshot("t")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
    }
}
