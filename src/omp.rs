use std::path::{Path, PathBuf};

use crate::session::SessionStatus;

/// Mapping from a tmux pane TTY to an OMP session jsonl.
#[derive(Debug, Clone, PartialEq)]
pub struct OmpTerminalSession {
    pub cwd: String,
    pub jsonl_path: PathBuf,
    pub session_id: String,
}

/// Current OMP metadata and the byte offset through the last complete record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OmpSessionMeta {
    pub session_id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub last_file_size: u64,
}

/// Live context usage parsed from the OMP status footer.
#[derive(Debug, Clone, PartialEq)]
pub struct OmpTokenInfo {
    pub used_tokens: u64,
    pub context_window: u64,
}

/// Basename of a tmux `#{pane_tty}` path (`/dev/ttys010` → `ttys010`).
pub fn tty_basename(pane_tty: &str) -> Option<&str> {
    let name = pane_tty.rsplit('/').next().unwrap_or(pane_tty).trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Parse `~/.omp/agent/terminal-sessions/{tty}` (cwd line + jsonl path line).
pub fn parse_terminal_session(content: &str) -> Option<OmpTerminalSession> {
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    let cwd = lines.next()?.to_string();
    let jsonl = lines.next()?;
    let session_id = session_id_from_jsonl_path(jsonl)?;
    Some(OmpTerminalSession {
        cwd,
        jsonl_path: PathBuf::from(jsonl),
        session_id,
    })
}

/// UUID after the last `_` in `2026-08-21T11-29-54-321Z_{uuid}.jsonl`.
pub fn session_id_from_jsonl_path(path: &str) -> Option<String> {
    let name = Path::new(path).file_stem()?.to_str()?;
    let id = name.rsplit('_').next()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Apply metadata records without allocating the ignored message/tool payloads.
fn apply_jsonl_meta(meta: &mut OmpSessionMeta, line: &[u8]) {
    #[derive(serde::Deserialize)]
    struct Message<'a> {
        #[serde(borrow)]
        role: Option<std::borrow::Cow<'a, str>>,
        #[serde(borrow)]
        provider: Option<std::borrow::Cow<'a, str>>,
        #[serde(borrow)]
        model: Option<std::borrow::Cow<'a, str>>,
    }

    #[derive(serde::Deserialize)]
    struct Record<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        id: Option<String>,
        cwd: Option<String>,
        title: Option<String>,
        model: Option<String>,
        #[serde(borrow)]
        message: Option<Message<'a>>,
    }

    // Only top-level metadata participates; quoted records in messages do not.
    let Ok(record) = serde_json::from_slice::<Record<'_>>(line) else {
        return;
    };
    match record.kind {
        "title" => {
            if let Some(title) = record.title.filter(|t| !t.is_empty()) {
                meta.title = Some(title);
            }
        }
        "title_change" if meta.title.is_none() => {
            meta.title = record.title.filter(|t| !t.is_empty());
        }
        "session" => {
            if let Some(id) = record.id {
                meta.session_id = id;
            }
            if record.cwd.is_some() {
                meta.cwd = record.cwd;
            }
            if meta.title.is_none() {
                meta.title = record.title.filter(|t| !t.is_empty());
            }
        }
        "model_change" => {
            if record.model.is_some() {
                meta.model = record.model;
            }
        }
        // Sessions without a model_change record still name the model on each reply.
        "message" => {
            if let Some(Message {
                role: Some(role),
                provider: Some(provider),
                model: Some(model),
            }) = record.message
            {
                if role == "assistant" {
                    meta.model = Some(format!("{provider}/{model}"));
                }
            }
        }
        _ => {}
    }
}

/// Parse `◫ 23.0%/500K` from the OMP pane footer (last 15 non-empty lines).
/// Quoted status lines higher in the pane are ignored.
pub fn parse_context_footer(content: &str) -> Option<OmpTokenInfo> {
    let tail = content
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(15);
    for line in tail {
        if let Some(info) = parse_context_marker(line) {
            return Some(info);
        }
    }
    None
}

/// Parse one footer context marker. Prefers `◫ N%/window`, then `▶ N% ┃ window ◀`.
fn parse_context_marker(line: &str) -> Option<OmpTokenInfo> {
    parse_diamond_context(line).or_else(|| parse_bar_context(line))
}

/// Legacy footer: `◫ 23.0%/500K`.
fn parse_diamond_context(line: &str) -> Option<OmpTokenInfo> {
    let marker = line.rfind('\u{25EB}')?; // ◫
    let after = line[marker + '\u{25EB}'.len_utf8()..].trim_start();
    let pct_end = after.find('%')?;
    let pct: f64 = after[..pct_end].trim().parse().ok()?;
    let rest = after[pct_end + 1..].trim_start();
    let rest = rest.strip_prefix('/').unwrap_or(rest).trim_start();
    let win_end = rest
        .find(|c: char| c.is_whitespace() || "⟲>├│╮╭─".contains(c))
        .unwrap_or(rest.len());
    tokens_from_percent(pct, rest[..win_end].trim())
}

/// Current footer: `▶─────34%─────┃─500K─◀`.
fn parse_bar_context(line: &str) -> Option<OmpTokenInfo> {
    if !(line.contains('\u{25B6}') && line.contains('\u{25C0}')) {
        return None;
    }
    let pct_end = line.rfind('%')?;
    let pct = trailing_number(&line[..pct_end])?;
    let window_raw = line[pct_end + 1..]
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .find(|part| parse_token_window(part).is_some())?;
    tokens_from_percent(pct, window_raw)
}

fn tokens_from_percent(pct: f64, window_raw: &str) -> Option<OmpTokenInfo> {
    let window = parse_token_window(window_raw)?;
    let used = ((pct / 100.0) * window as f64).round() as u64;
    Some(OmpTokenInfo {
        used_tokens: used,
        context_window: window,
    })
}

/// Digits (and one decimal) immediately before `end` of `s`.
fn trailing_number(s: &str) -> Option<f64> {
    let start = s
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.')
        .last()
        .map(|(i, _)| i)?;
    s[start..].parse().ok()
}

/// `500K` → 500_000, `1M` → 1_000_000, bare digits as-is.
fn parse_token_window(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (num, mult) = match raw.as_bytes().last().map(|b| b.to_ascii_uppercase()) {
        Some(b'K') => (&raw[..raw.len() - 1], 1_000u64),
        Some(b'M') => (&raw[..raw.len() - 1], 1_000_000u64),
        _ => (raw, 1u64),
    };
    let value: f64 = num.parse().ok()?;
    Some((value * mult as f64).round() as u64)
}

/// Status from captured OMP pane text. Input/Working only trust the footer.
pub fn omp_status_from_content(content: &str) -> SessionStatus {
    let tail: Vec<&str> = content
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(15)
        .collect();

    // Ask widgets sit in the footer; ignore a scrolled-back copy higher up.
    if tail.iter().any(|l| is_omp_input_line(l)) {
        return SessionStatus::Input;
    }

    // The live footer (bottom-most "╭──" line) is the sole busy authority: a
    // braille spinner with elapsed time follows the border while a turn runs
    // ("╭── ⠸ 19s >"); idle shows "╭── π >". Quoted markers in scrollback
    // (another pane's footer, an "⟦esc⟧" hint, a waiting line) never count.
    let live_footer = tail.iter().find(|l| l.starts_with("╭──"));
    let live_footer_busy = live_footer.is_some_and(|l| footer_has_spinner(l));

    if live_footer_busy {
        // A tool running in the foreground ("⠹ Read file ⟦esc⟧" or "⎋ Doing x")
        // sits directly above the footer. That is real work, so it beats any
        // background job count.
        let footer_idx = tail.iter().position(|l| l.starts_with("╭──"));
        let above_footer = footer_idx.and_then(|i| tail.get(i + 1));
        if above_footer.is_some_and(|l| is_live_tool_line(l)) {
            return SessionStatus::Working;
        }
        // Waiting on background jobs: "ⓘ waiting on N job(s)" above the footer.
        // Trusted only while the footer spins; an idle "π" footer wins over a
        // quoted or captured waiting line lingering in scrollback.
        if let Some(count) = tail.iter().find_map(|l| waiting_job_count(l)) {
            return SessionStatus::BackgroundTasks(count);
        }
        // The footer's "⚙ N" gear counts live background jobs even when the
        // waiting line has scrolled away (e.g. "Polling in 60 min (bg_2)").
        if let Some(count) = live_footer.and_then(|l| footer_job_count(l)) {
            return SessionStatus::BackgroundTasks(count);
        }
        return SessionStatus::Working;
    }

    SessionStatus::Idle
}

/// True when a footer border line carries a braille spinner ("╭── ⠸ 19s >").
fn footer_has_spinner(line: &str) -> bool {
    line.strip_prefix("╭──")
        .and_then(|rest| rest.trim_start().chars().next())
        .is_some_and(|c| matches!(c, '\u{2800}'..='\u{28FF}'))
}

/// True for an in-flight tool line: braille spinner + "⟦esc⟧" hint, or "⎋".
fn is_live_tool_line(line: &str) -> bool {
    let spinner = line
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '\u{2800}'..='\u{28FF}'));
    (spinner && line.ends_with("⟦esc⟧")) || line.starts_with('\u{238B}') // ⎋
}

/// Parse the footer gear "⚙ N" → N background jobs; None when absent or zero.
fn footer_job_count(line: &str) -> Option<u32> {
    let pos = line.find('\u{2699}')?; // ⚙
    let count: u32 = line[pos + '\u{2699}'.len_utf8()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    (count > 0).then_some(count)
}

/// Parse "ⓘ waiting on 2 jobs" → 2, tolerating the singular/plural wording.
/// Anchored on the ⓘ marker so quoted text ("echo waiting on 1 job") is ignored.
fn waiting_job_count(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("ⓘ waiting on ")?;
    let mut words = rest.split_whitespace();
    let count = words.next()?.parse().ok()?;
    matches!(words.next()?, "job" | "jobs").then_some(count)
}

/// Footer markers for the OMP `ask` widget.
fn is_omp_input_line(line: &str) -> bool {
    line.contains("Enter select")
        || line.contains("Esc cancel")
        || line.contains("Ask 1 question")
        || line.contains("╭─ Ask")
        || line.contains("╭─── Ask")
}

/// Look up a live OMP session via `~/.omp/agent/terminal-sessions/{tty}`.
pub fn find_omp_session(pane_tty: &str) -> Option<OmpTerminalSession> {
    let tty = tty_basename(pane_tty)?;
    let path = dirs::home_dir()?
        .join(".omp")
        .join("agent")
        .join("terminal-sessions")
        .join(tty);
    let content = std::fs::read_to_string(path).ok()?;
    parse_terminal_session(&content)
}

/// Scan once, then read only appended records using the previous refresh state.
pub fn read_jsonl_meta(path: &Path, previous: Option<OmpSessionMeta>) -> Option<OmpSessionMeta> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    // Truncation invalidates both the offset and the accumulated metadata.
    let file = std::fs::File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();
    let mut meta = previous
        .filter(|m| m.last_file_size > 0 && m.last_file_size <= file_size)
        .unwrap_or_default();
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(meta.last_file_size)).ok()?;

    // Leave an unfinished final record for the next refresh, even across UTF-8 bytes.
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line).ok()?;
        if bytes == 0 || line.last() != Some(&b'\n') {
            break;
        }
        apply_jsonl_meta(&mut meta, &line);
        meta.last_file_size += bytes as u64;
    }
    (!meta.session_id.is_empty()).then_some(meta)
}

/// Capture a tmux pane and classify OMP Idle/Working/Input.
pub fn omp_pane_status(pane_target: &str) -> SessionStatus {
    let output = match std::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_target, "-p"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return SessionStatus::Idle,
    };
    let content = String::from_utf8_lossy(&output.stdout);
    omp_status_from_content(&content)
}

/// Capture a tmux pane and parse the live `◫ N%/window` footer.
pub fn omp_pane_tokens(pane_target: &str) -> Option<OmpTokenInfo> {
    let output = std::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_target, "-p"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_context_footer(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tty_basename_strips_dev_prefix() {
        assert_eq!(tty_basename("/dev/ttys010"), Some("ttys010"));
        assert_eq!(tty_basename("ttys011"), Some("ttys011"));
    }

    #[test]
    fn terminal_session_file_maps_cwd_and_session_id() {
        let content = "\
/Users/richard/src/oss/recon
/Users/richard/.omp/agent/sessions/-src-oss-recon/2026-08-21T11-29-54-321Z_01a02415-a891-7000-b483-5c69057c247e.jsonl
";
        let parsed = parse_terminal_session(content).unwrap();
        assert_eq!(parsed.cwd, "/Users/richard/src/oss/recon");
        assert_eq!(parsed.session_id, "01a02415-a891-7000-b483-5c69057c247e");
    }

    #[test]
    fn jsonl_meta_keeps_title_and_last_model() {
        let content = r#"
{"type":"title","v":1,"title":"Add omp agent support"}
{"type":"session","version":3,"id":"01a02415-a891-7000-b483-5c69057c247e","cwd":"/Users/richard/src/oss/recon","title":"Solve the merge conflicts"}
{"type":"title_change","title":"Solve the merge conflicts"}
{"type":"model_change","model":"anthropic/claude-opus-4-8"}
{"type":"model_change","model":"xai-oauth/grok-4.6"}
"#;
        let mut meta = OmpSessionMeta::default();
        for line in content.lines() {
            apply_jsonl_meta(&mut meta, line.as_bytes());
        }
        assert_eq!(meta.session_id, "01a02415-a891-7000-b483-5c69057c247e");
        assert_eq!(meta.cwd.as_deref(), Some("/Users/richard/src/oss/recon"));
        assert_eq!(meta.title.as_deref(), Some("Add omp agent support"));
        assert_eq!(meta.model.as_deref(), Some("xai-oauth/grok-4.6"));
    }

    #[test]
    fn falls_back_to_assistant_message_model_without_model_change() {
        // Some OMP transcripts never write model_change; replies still carry provider/model.
        let content = r#"
{"type":"session","version":3,"id":"no-model-change","cwd":"/work"}
{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}
{"type":"message","message":{"role":"assistant","content":[],"provider":"anthropic","model":"claude-opus-5-5"}}
{"type":"message","message":{"role":"toolResult","provider":"xai-oauth","model":"grok-4.7"}}
"#;
        let mut meta = OmpSessionMeta::default();
        for line in content.lines() {
            apply_jsonl_meta(&mut meta, line.as_bytes());
        }
        assert_eq!(meta.model.as_deref(), Some("anthropic/claude-opus-5-5"));

        // A later switch, then a reply from the new model, keeps the newest.
        apply_jsonl_meta(
            &mut meta,
            br#"{"type":"model_change","model":"xai-oauth/grok-4.7"}"#,
        );
        apply_jsonl_meta(
            &mut meta,
            br#"{"type":"message","message":{"role":"assistant","provider":"xai-oauth","model":"grok-4.7"}}"#,
        );
        assert_eq!(meta.model.as_deref(), Some("xai-oauth/grok-4.7"));
    }

    #[test]
    fn reads_model_switch_beyond_header_and_tail_window() {
        // The current model can sit deep in a long-lived transcript.
        let path =
            std::env::temp_dir().join(format!("recon-omp-late-model-{}.jsonl", std::process::id()));
        let mut content = String::from(
            "{\"type\":\"session\",\"id\":\"late-model\"}\n\
             {\"type\":\"model_change\",\"model\":\"openai-codex/gpt-5.6-luna\",\"role\":\"smol\"}\n",
        );
        content.push_str(&"{\"type\":\"message\"}\n".repeat(50));
        content.push_str(
            "{\"type\":\"model_change\",\"model\":\"openai-codex/gpt-6-astra\",\"role\":\"temporary\"}\n",
        );
        content.push_str(&"{\"type\":\"message\"}\n".repeat(4000));
        std::fs::write(&path, content).unwrap();
        let meta = read_jsonl_meta(&path, None).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(meta.model.as_deref(), Some("openai-codex/gpt-6-astra"));
    }

    #[test]
    fn refreshes_appended_model_only_after_record_is_complete() {
        use std::io::Write;

        // Preserve header metadata across refreshes and partial append writes.
        let path = std::env::temp_dir().join(format!(
            "recon-omp-append-model-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"id\":\"append-model\",\"cwd\":\"/work\",\"title\":\"Keep title\"}\n\
             {\"type\":\"model_change\",\"model\":\"openai-codex/gpt-5.6-luna\"}\n",
        )
        .unwrap();
        let initial = read_jsonl_meta(&path, None).unwrap();
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer
            .write_all(b"{\"type\":\"model_change\",\"model\":\"openai-codex/gpt-6-as")
            .unwrap();
        let partial = read_jsonl_meta(&path, Some(initial)).unwrap();
        assert_eq!(partial.model.as_deref(), Some("openai-codex/gpt-5.6-luna"));
        writer.write_all(b"tra\"}\n").unwrap();
        let updated = read_jsonl_meta(&path, Some(partial)).unwrap();
        assert_eq!(updated.model.as_deref(), Some("openai-codex/gpt-6-astra"));
        assert_eq!(updated.session_id, "append-model");
        assert_eq!(updated.cwd.as_deref(), Some("/work"));
        assert_eq!(updated.title.as_deref(), Some("Keep title"));

        // An older model mentioned in message payloads must not override the switch.
        writer
            .write_all(
                b"{\"type\":\"message\",\"message\":{\"type\":\"model_change\",\"model\":\"openai-codex/gpt-5.6-luna\"}}\n",
            )
            .unwrap();
        let unchanged = read_jsonl_meta(&path, Some(updated)).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(unchanged.model.as_deref(), Some("openai-codex/gpt-6-astra"));
    }

    #[test]
    fn discards_cached_metadata_after_truncation() {
        // Rewritten transcripts must not inherit a stale offset, model, or title.
        let path = std::env::temp_dir().join(format!(
            "recon-omp-truncated-model-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"id\":\"old-session\",\"cwd\":\"/old\",\"title\":\"Old title\"}\n\
             {\"type\":\"model_change\",\"model\":\"openai-codex/gpt-5.6-luna\"}\n",
        )
        .unwrap();
        let initial = read_jsonl_meta(&path, None).unwrap();
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"id\":\"new-session\"}\n\
             {\"type\":\"model_change\",\"model\":\"openai-codex/gpt-6-astra\"}\n",
        )
        .unwrap();
        let rewritten = read_jsonl_meta(&path, Some(initial)).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(rewritten.session_id, "new-session");
        assert_eq!(rewritten.model.as_deref(), Some("openai-codex/gpt-6-astra"));
        assert_eq!(rewritten.cwd, None);
        assert_eq!(rewritten.title, None);
    }

    #[test]
    fn footer_parses_percent_of_500k_window() {
        let content =
            "╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/oss/recon > ◫ 23.0%/500K ⟲ > (sub) ──╮";
        let tokens = parse_context_footer(content).unwrap();
        assert_eq!(tokens.context_window, 500_000);
        assert_eq!(tokens.used_tokens, 115_000);
    }

    #[test]
    fn footer_tokens_ignore_quoted_context_marker() {
        // A Working pane can quote another session's status line. The live
        // footer is the last ◫, not the first.
        let content = "\
  recon --json
  worko pane quoted: ◫ 20.8%/500K

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/oss/recon > ◫ 37.7%/500K ⟲ > (sub) ──╮
╰─                                                                                                                              ─╯
";
        let tokens = parse_context_footer(content).unwrap();
        assert_eq!(tokens.context_window, 500_000);
        assert_eq!(tokens.used_tokens, 188_500);
    }

    #[test]
    fn footer_parses_bar_percent_and_window() {
        let content = "\
╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/luna > ⑂ main > ▶─────34%─────┃─500K─◀ Format Claude logs with jq ──╮
╰─                                                                                                                              ─╯
";
        let tokens = parse_context_footer(content).unwrap();
        assert_eq!(tokens.context_window, 500_000);
        assert_eq!(tokens.used_tokens, 170_000);
    }

    #[test]
    fn idle_empty_prompt_box_is_idle() {
        let content = "\
 ※ recap: Dump workflow quota is fixed.

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/worko > ◫ 20.8%/500K ──╮
╰─                                                                                                                              ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn spinner_footer_without_esc_hint_is_working() {
        let content = "\
  ⎋ Capturing all omp pane footers
╭── ⠸ 19s  > ◒ Fable 5 👁 > ⑂ main *1 > S2.56 + 👁 S0.27 ▶─7%───┃──1M─◀ ⚙ 1 < Fix omp status ──╮
╰─                                                                                              ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn spinner_footer_with_gear_count_is_background_tasks() {
        // No "ⓘ waiting on" line on screen, but the live footer gear shows
        // one background job while the turn keeps spinning.
        let content = "\
 Polling in 60 min (bg_2). Summary of where we are:
  └─ ☑ Deploy V094 to DEV and TEST
╭── ⠇ 3m  > ◒ Fable 5 🙈 > S635.40 + 👁 S579.27 ▶────20%────┃────1M─◀ ⚙ 1 < wor-158 ──╮
╰─                                                                                      ─╯
";
        assert_eq!(
            omp_status_from_content(content),
            SessionStatus::BackgroundTasks(1)
        );
    }

    #[test]
    fn live_tool_line_beats_gear_count() {
        // Foreground tool running while a background job exists → Working.
        let content = "\
 Polling in 60 min (bg_2).
 ⠹ Read migration file ⟦esc⟧
╭── ⠇ 3m  > ◒ Fable 5 🙈 > S635.40 ▶────20%────┃────1M─◀ ⚙ 1 < wor-158 ──╮
╰─                                                                          ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Working);
    }

    #[test]
    fn footer_job_count_parses_gear() {
        assert_eq!(footer_job_count("▶─7%─◀ ⚙ 2 < title ──╮"), Some(2));
        assert_eq!(footer_job_count("▶─7%─◀ ⚙ 0 < title ──╮"), None);
        assert_eq!(footer_job_count("▶─7%─◀ < title ──╮"), None);
    }

    #[test]
    fn waiting_on_background_job_is_background_tasks() {
        let content = "\
ⓘ waiting on 1 job
└─ ⣯ bg_9 ⟦bash⟧ sleep 900

╭── ⠧ 7m  > ◒ Opus 4.6 👁 > S632.28 + 👁 S579.03 ▶────────14%──────────────────────────╮
╰─                                                                                      ─╯
";
        assert_eq!(
            omp_status_from_content(content),
            SessionStatus::BackgroundTasks(1)
        );
    }

    #[test]
    fn quoted_spinner_footer_above_idle_live_footer_stays_idle() {
        // Pane output quoting another session's working footer must not
        // override the idle "╭── π" live footer below it.
        let content = "\
=== 0:1.0 ===
╭── ⠹ 9m  > ◒ Opus 4.6 👁 > S632.28 ▶────14%───┃──1M─◀ ⚙ 1 < wor-158 ──╮
╰─                                                                      ─╯

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/worko > ◫ 20.8%/500K ──╮
╰─                                                                  ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn quoted_waiting_text_without_marker_stays_idle() {
        // Quoted or echoed "waiting on 1 job" lacks the ⓘ marker.
        let content = "\
echo waiting on 1 job

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/worko > ◫ 20.8%/500K ──╮
╰─                                                                  ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn captured_waiting_marker_above_idle_footer_stays_idle() {
        // An exact "ⓘ waiting on N job" line copied into scrollback (e.g. by
        // capture-pane output) must not beat the idle "╭── π" live footer.
        let content = "\
ⓘ waiting on 1 job
└─ ⣯ bg_9 ⟦bash⟧ sleep 900

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/worko > ◫ 20.8%/500K ──╮
╰─                                                                  ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn waiting_job_count_parses_singular_and_plural() {
        assert_eq!(waiting_job_count("ⓘ waiting on 1 job"), Some(1));
        assert_eq!(waiting_job_count("ⓘ waiting on 3 jobs"), Some(3));
        assert_eq!(waiting_job_count("ⓘ waiting on 2 agents"), None);
        assert_eq!(waiting_job_count("waiting on 1 job"), None);
    }

    #[test]
    fn quoted_esc_hint_above_idle_footer_stays_idle() {
        // "⟦esc⟧" in scrollback (quoted source, test text, or a captured
        // pane) must not beat the idle "╭── π" live footer; the footer
        // spinner is the sole busy authority.
        let content = "\
 ⠹ Capture recon OMP pane footer ⟦esc⟧

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/oss/recon > ◫ 26.3%/500K ──╮
╰─                                                                                                                              ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }

    #[test]
    fn ask_widget_in_footer_is_input() {
        let content = "\
 ⠦ Ask follow-up on remaining artifact quota risk ⟦esc⟧

╭─ Ask ──────────────────────────────────────────────────────────────────────────╮
│ Dump is green. What next?                                                      │
│ ❯ ○ Add short retention on other repos                                         │
│   ○ Leave other repos alone                                                    │
├────────────────────────────────────────────────────────────────────────────────┤
│ Enter select · n note · ↑/↓ move · ↓ scroll · Esc cancel                       │
╰────────────────────────────────────────────────────────────────────────────────╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Input);
    }

    #[test]
    fn scrolled_back_ask_box_does_not_override_idle_footer() {
        let content = "\
╭─ Ask ──────────────────────────────────────────────────────────────────────────╮
│ Enter select · n note · ↑/↓ move · ↓ scroll · Esc cancel                       │
╰────────────────────────────────────────────────────────────────────────────────╯

some later output
more later output
even more later output
still more later output
and more later output
keep going later output
padding line one
padding line two
padding line three
padding line four
padding line five
padding line six

╭── π  > ⬢ Grok 4.6 👁 · ◒ high > 📁 ~/src/worko > ◫ 20.8%/500K ──╮
╰─                                                                                                                              ─╯
";
        assert_eq!(omp_status_from_content(content), SessionStatus::Idle);
    }
}
