// Detects whether the terminal's kitty config remaps Shift+Enter to Ctrl+J
// (`send_text` of a newline byte, 0x0a). The zoom binding is triggered by
// Ctrl+J; this detection only decides whether the footer can advertise the
// friendlier "S-Enter" label or must fall back to the literal "C-j".

use std::path::{Path, PathBuf};

/// True if any discovered kitty config maps Shift+Enter to a newline byte.
pub fn shift_enter_sends_ctrl_j() -> bool {
    for path in config_files() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if conf_maps_shift_enter_newline(&contents) {
                return true;
            }
            // Follow one level of `include` directives (split-config pattern).
            let dir = path.parent().unwrap_or(Path::new("."));
            for inc in includes(&contents, dir) {
                if let Ok(inc_contents) = std::fs::read_to_string(&inc) {
                    if conf_maps_shift_enter_newline(&inc_contents) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Candidate top-level kitty.conf paths, honoring KITTY_CONFIG_DIRECTORY and XDG.
fn config_files() -> Vec<PathBuf> {
    let dir = if let Ok(d) = std::env::var("KITTY_CONFIG_DIRECTORY") {
        PathBuf::from(d)
    } else if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("kitty")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("kitty")
    } else {
        return Vec::new();
    };
    vec![dir.join("kitty.conf")]
}

/// Resolve `include`/`globinclude` paths (relative to the config dir) listed in
/// a config's contents. Best-effort: ignores globs, returns plain paths only.
fn includes(contents: &str, dir: &Path) -> Vec<PathBuf> {
    contents
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("include ").map(str::trim))
        .map(|rel| {
            let p = Path::new(rel);
            if p.is_absolute() { p.to_path_buf() } else { dir.join(p) }
        })
        .collect()
}

/// Pure check: does this config text contain an active `map shift+enter
/// send_text ... \n` (or \x0a) directive? Ignores comments and \r (= Enter).
fn conf_maps_shift_enter_newline(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        if line.starts_with('#') || !line.starts_with("map ") {
            return false;
        }
        line.contains("shift+enter")
            && line.contains("send_text")
            && (line.contains("\\n") || line.contains("\\x0a"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_standard_mapping() {
        assert!(conf_maps_shift_enter_newline("map shift+enter send_text all \\n"));
    }

    #[test]
    fn matches_hex_escape_and_indentation() {
        assert!(conf_maps_shift_enter_newline("   map shift+enter send_text all \\x0a"));
    }

    #[test]
    fn ignores_commented_line() {
        assert!(!conf_maps_shift_enter_newline("# map shift+enter send_text all \\n"));
    }

    #[test]
    fn ignores_carriage_return_mapping() {
        // \r is Enter (0x0d), not Ctrl+J.
        assert!(!conf_maps_shift_enter_newline("map shift+enter send_text all \\r"));
    }

    #[test]
    fn ignores_unrelated_mapping() {
        assert!(!conf_maps_shift_enter_newline("map ctrl+enter send_text all \\n"));
        assert!(!conf_maps_shift_enter_newline("map shift+enter no_op"));
    }

    #[test]
    fn finds_mapping_among_many_lines() {
        let conf = "font_size 12\n\nmap shift+enter send_text all \\n\nmap f1 launch\n";
        assert!(conf_maps_shift_enter_newline(conf));
    }

    #[test]
    fn parses_include_directives() {
        let dir = Path::new("/home/x/.config/kitty");
        let incs = includes("include keys.conf\ninclude /abs/extra.conf\n", dir);
        assert_eq!(incs, vec![
            PathBuf::from("/home/x/.config/kitty/keys.conf"),
            PathBuf::from("/abs/extra.conf"),
        ]);
    }
}
