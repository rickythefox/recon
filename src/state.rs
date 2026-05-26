use std::fs;
use std::path::PathBuf;

pub struct SavedState {
    pub last_session_id: Option<String>,
    pub prev_session_id: Option<String>,
}

fn state_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"));
    path.push("recon");
    path.push("state.json");
    path
}

pub fn load() -> SavedState {
    let data = fs::read_to_string(state_path()).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
    SavedState {
        last_session_id: v.get("last_session_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
        prev_session_id: v.get("prev_session_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
    }
}

pub fn save(last: &str, prev: Option<&str>) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let data = serde_json::json!({
        "last_session_id": last,
        "prev_session_id": prev,
    });
    let _ = fs::write(path, data.to_string());
}
