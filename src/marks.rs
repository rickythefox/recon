//! User marks on sessions, keyed by stable conversation id and persisted to
//! `~/.local/state/recon/marks.json` as `{ "<stable_id>": "<marked_at RFC3339>" }`.
//! A mark expires `TTL_DAYS` after it was set.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};

const TTL_DAYS: i64 = 30;

type Entries = HashMap<String, DateTime<Utc>>;

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

    /// Toggle the mark for `id` against the current file contents.
    /// Returns whether `id` is marked afterwards.
    pub fn toggle(&mut self, id: &str) -> Result<bool, String> {
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
            entries.insert(id.to_string(), now);
        }
        self.mtime = Some(write_entries(path, &entries)?);
        self.entries = entries;
        Ok(marked)
    }

    /// Whether `id` carries an unexpired mark.
    pub fn is_marked(&self, id: &str) -> bool {
        self.entries
            .get(id)
            .is_some_and(|at| !is_expired(*at, Utc::now()))
    }
}

fn is_expired(marked_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - marked_at >= Duration::days(TTL_DAYS)
}

/// Drop expired entries; returns whether anything was removed.
fn prune(entries: &mut Entries, now: DateTime<Utc>) -> bool {
    let before = entries.len();
    entries.retain(|_, at| !is_expired(*at, now));
    entries.len() != before
}

/// Read the mark file; a missing file is an empty set, a malformed one an error.
fn read_entries(path: &Path) -> Result<Entries, String> {
    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    let raw: HashMap<String, String> =
        serde_json::from_str(&data).map_err(|e| format!("Malformed {}: {e}", path.display()))?;
    raw.into_iter()
        .map(|(id, at)| {
            DateTime::parse_from_rfc3339(&at)
                .map(|t| (id, t.with_timezone(&Utc)))
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
    let raw: HashMap<&str, String> = entries
        .iter()
        .map(|(id, at)| (id.as_str(), at.to_rfc3339()))
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

    #[test]
    fn mark_persists_across_instances_and_unmarks() {
        let path = temp_path("persist");
        let mut a = Marks::new(Some(path.clone()));
        assert!(a.toggle("conv-1").unwrap());

        // A relaunched dashboard sees the mark
        let mut b = Marks::new(Some(path.clone()));
        b.sync().unwrap();
        assert!(b.is_marked("conv-1"));

        // Second press removes it, visible to the other instance on sync
        assert!(!b.toggle("conv-1").unwrap());
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
        a.toggle("from-a").unwrap();
        b.toggle("from-b").unwrap();
        assert!(b.is_marked("from-a"));
        assert!(b.is_marked("from-b"));
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
        assert!(m.toggle("x").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
    }
}
