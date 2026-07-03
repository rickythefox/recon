/// Map raw model IDs to human-friendly display names.
pub fn display_name(model_id: &str) -> &str {
    match model_id {
        "claude-fable-5" => "Fable 5",
        "claude-sonnet-5" => "Sonnet 5",
        "claude-opus-4-8" => "Opus 4.8",
        "claude-opus-4-6" => "Opus 4.6",
        "claude-sonnet-4-6" => "Sonnet 4.6",
        "claude-sonnet-4-5-20250514" => "Sonnet 4.5",
        "claude-haiku-4-5-20251001" => "Haiku 4.5",
        "claude-opus-4-20250514" => "Opus 4",
        "claude-sonnet-4-20250514" => "Sonnet 4",
        "gpt-5.5" => "GPT-5.5",
        "gpt-5.4" => "GPT-5.4",
        "o4-mini" => "o4-mini",
        "o3" => "o3",
        _ => model_id,
    }
}

/// Context window size for a given model ID.
pub fn context_window(model_id: &str) -> u64 {
    match model_id {
        "claude-opus-4-8" | "claude-opus-4-6" | "gpt-5.5" => 1_000_000,
        _ => 200_000,
    }
}

/// Reverse lookup: display name (from /model output) -> model ID.
/// Returns None if the display name is not recognized.
pub fn id_from_display_name(display: &str) -> Option<&'static str> {
    match display {
        "Fable 5" => Some("claude-fable-5"),
        "Sonnet 5" => Some("claude-sonnet-5"),
        "Opus 4.8" | "Opus 4.8 (1M context)" => Some("claude-opus-4-8"),
        "Opus 4.6" | "Opus 4.6 (1M context)" => Some("claude-opus-4-6"),
        "Sonnet 4.6" => Some("claude-sonnet-4-6"),
        "Sonnet 4.5" => Some("claude-sonnet-4-5-20250514"),
        "Haiku 4.5" => Some("claude-haiku-4-5-20251001"),
        "Opus 4" => Some("claude-opus-4-20250514"),
        "Sonnet 4" => Some("claude-sonnet-4-20250514"),
        "GPT-5.5" => Some("gpt-5.5"),
        "GPT-5.4" => Some("gpt-5.4"),
        "o4-mini" => Some("o4-mini"),
        "o3" => Some("o3"),
        _ => None,
    }
}

/// Format model name with optional effort level.
pub fn format_with_effort(model_id: &str, effort: &str) -> String {
    let name = display_name(model_id);
    if effort.is_empty() || effort == "default" {
        name.to_string()
    } else {
        format!("{name} ({effort})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_opus_4_8_compactly() {
        assert_eq!(display_name("claude-opus-4-8"), "Opus 4.8");
        assert_eq!(format_with_effort("claude-opus-4-8", ""), "Opus 4.8");
    }

    #[test]
    fn maps_opus_4_8_display_name_back_to_id() {
        assert_eq!(id_from_display_name("Opus 4.8"), Some("claude-opus-4-8"));
        assert_eq!(
            id_from_display_name("Opus 4.8 (1M context)"),
            Some("claude-opus-4-8")
        );
    }

    #[test]
    fn opus_4_8_uses_one_million_context_window() {
        assert_eq!(context_window("claude-opus-4-8"), 1_000_000);
    }
}
