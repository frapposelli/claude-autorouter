//! Exact, reviewed provider identities. Family-like aliases are not capabilities.

use serde::Serialize;
use serde_json::Value;

pub const MODEL_CATALOG_REVIEWED_AT: &str = "2026-10-05";
pub const MODEL_CAPABILITY_SOURCES: &[(&str, &str)] = &[
    (
        "thinking",
        "https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting",
    ),
    (
        "effort",
        "https://platform.claude.com/docs/en/build-with-claude/effort",
    ),
    (
        "sonnet55",
        "https://platform.claude.com/docs/en/models/sonnet-5-5/migration-guide",
    ),
    (
        "opus55",
        "https://platform.claude.com/docs/en/models/opus-5-5/whats-new-opus-5-5",
    ),
    ("auto", "https://code.claude.com/docs/en/permission-modes"),
    (
        "subscriptionContext",
        "https://code.claude.com/docs/en/model-config",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    pub max_output_tokens: u64,
    pub capacity_upgrade_source: bool,
    pub tool_references: bool,
    pub auto_mode: bool,
    pub shared_auto: bool,
    pub thinking_types: &'static [&'static str],
    pub effort_levels: &'static [&'static str],
    pub forced_tool_choice: bool,
    pub assistant_prefill: bool,
    pub default_sampling_only: bool,
    pub mid_conversation_system: bool,
    pub per_message_effort: bool,
    pub task_budget: bool,
    pub reviewed_at: &'static str,
    pub source: &'static str,
    pub family: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_thinking_adaptation: Option<&'static str>,
}

const BASIC_EFFORT: &[&str] = &["low", "medium", "high"];
const FOUR_EFFORT: &[&str] = &["low", "medium", "high", "max"];
const FIVE_EFFORT: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const HAIKU_45: ModelCapabilities = ModelCapabilities {
    context_window: Some(200_000),
    max_output_tokens: 64_000,
    capacity_upgrade_source: true,
    tool_references: true,
    auto_mode: false,
    shared_auto: false,
    thinking_types: &["disabled", "enabled"],
    effort_levels: &[],
    forced_tool_choice: true,
    assistant_prefill: true,
    default_sampling_only: false,
    mid_conversation_system: false,
    per_message_effort: false,
    task_budget: false,
    reviewed_at: MODEL_CATALOG_REVIEWED_AT,
    source: "https://platform.claude.com/docs/en/models/haiku-4-5/overview",
    family: "haiku",
    disabled_thinking_adaptation: None,
};
const SONNET_45: ModelCapabilities = ModelCapabilities {
    family: "sonnet",
    source: "https://platform.claude.com/docs/en/models/sonnet-4-5/overview",
    ..HAIKU_45
};
const OPUS_45: ModelCapabilities = ModelCapabilities {
    family: "opus",
    effort_levels: BASIC_EFFORT,
    source: "https://platform.claude.com/docs/en/models/opus-4-5/overview",
    ..HAIKU_45
};
// 4.6 subscription clients may need an explicit 1M opt-in. Do not advertise
// an unconditional window even though the API has larger capacity.
const SONNET_46: ModelCapabilities = ModelCapabilities {
    context_window: None,
    max_output_tokens: 128_000,
    auto_mode: true,
    thinking_types: &["disabled", "enabled", "adaptive"],
    effort_levels: FOUR_EFFORT,
    assistant_prefill: false,
    source: "https://platform.claude.com/docs/en/models/sonnet-4-6/overview",
    ..SONNET_45
};
const OPUS_46: ModelCapabilities = ModelCapabilities {
    family: "opus",
    source: "https://platform.claude.com/docs/en/models/opus-4-6/overview",
    ..SONNET_46
};
const OPUS_47: ModelCapabilities = ModelCapabilities {
    context_window: Some(1_000_000),
    capacity_upgrade_source: false,
    thinking_types: &["disabled", "adaptive"],
    effort_levels: FIVE_EFFORT,
    default_sampling_only: true,
    source: "https://platform.claude.com/docs/en/models/opus-4-7/overview",
    ..OPUS_46
};
const OPUS_48: ModelCapabilities = ModelCapabilities {
    source: "https://platform.claude.com/docs/en/models/opus-4-8/overview",
    ..OPUS_47
};
const SONNET_5: ModelCapabilities = ModelCapabilities {
    family: "sonnet",
    shared_auto: true,
    source: "https://platform.claude.com/docs/en/models/sonnet-5/overview",
    ..OPUS_47
};
const SONNET_55: ModelCapabilities = ModelCapabilities {
    thinking_types: &["adaptive", "between_tools"],
    forced_tool_choice: false,
    mid_conversation_system: true,
    per_message_effort: true,
    task_budget: true,
    disabled_thinking_adaptation: Some("between_tools"),
    source: "https://platform.claude.com/docs/en/models/sonnet-5-5/overview",
    ..SONNET_5
};
const OPUS_5: ModelCapabilities = ModelCapabilities {
    family: "opus",
    mid_conversation_system: true,
    per_message_effort: true,
    task_budget: true,
    disabled_thinking_adaptation: Some("adaptive"),
    source: "https://platform.claude.com/docs/en/models/opus-5/overview",
    ..SONNET_5
};
const OPUS_55: ModelCapabilities = ModelCapabilities {
    thinking_types: &["adaptive"],
    forced_tool_choice: false,
    source: "https://platform.claude.com/docs/en/models/opus-5-5/overview",
    ..OPUS_5
};

pub const MODEL_IDS: &[&str] = &[
    "claude-haiku-4-5",
    "claude-haiku-4-5-20251001",
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929",
    "claude-opus-4-5",
    "claude-opus-4-5-20251101",
    "claude-sonnet-4-6",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-sonnet-5",
    "claude-sonnet-5-5",
    "claude-opus-5",
    "claude-opus-5-5",
];

pub fn model_capabilities(model: &str) -> Option<&'static ModelCapabilities> {
    match model {
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => Some(&HAIKU_45),
        "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929" => Some(&SONNET_45),
        "claude-opus-4-5" | "claude-opus-4-5-20251101" => Some(&OPUS_45),
        "claude-sonnet-4-6" => Some(&SONNET_46),
        "claude-opus-4-6" => Some(&OPUS_46),
        "claude-opus-4-7" => Some(&OPUS_47),
        "claude-opus-4-8" => Some(&OPUS_48),
        "claude-sonnet-5" => Some(&SONNET_5),
        "claude-sonnet-5-5" => Some(&SONNET_55),
        "claude-opus-5" => Some(&OPUS_5),
        "claude-opus-5-5" => Some(&OPUS_55),
        _ => None,
    }
}

/// JSON representation matching the historical modelCapabilities contract.
pub fn catalog(model: &str) -> Option<Value> {
    model_capabilities(model)
        .map(|facts| serde_json::to_value(facts).expect("static model facts are JSON serializable"))
}

pub fn model_context_window(model: &str) -> Option<u64> {
    model_capabilities(model).and_then(|facts| facts.context_window)
}

pub fn has_native_million_context(model: &str) -> bool {
    model_context_window(model) == Some(1_000_000)
}

pub fn can_upgrade_context(model: &str) -> bool {
    model_capabilities(model).is_some_and(|facts| facts.capacity_upgrade_source)
}

pub fn supports_tool_references(model: &str) -> bool {
    model_capabilities(model).is_some_and(|facts| facts.tool_references)
}

pub fn supports_auto_mode(model: &str) -> bool {
    model_capabilities(model).is_some_and(|facts| facts.auto_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_uses_exact_reviewed_ids_and_shared_alias_facts() {
        assert!(std::ptr::eq(
            model_capabilities("claude-haiku-4-5").unwrap(),
            model_capabilities("claude-haiku-4-5-20251001").unwrap()
        ));
        for id in MODEL_IDS {
            let facts = model_capabilities(id).unwrap();
            assert_eq!(facts.reviewed_at, "2026-10-05");
            assert!(
                facts
                    .source
                    .starts_with("https://platform.claude.com/docs/")
            );
        }
        for id in [
            "opus",
            "team/claude-opus-5-5",
            "claude-opus-5-5-future",
            "__proto__",
            "toString",
            "",
        ] {
            assert!(catalog(id).is_none());
            assert!(model_context_window(id).is_none());
            assert!(!supports_tool_references(id));
            assert!(!supports_auto_mode(id));
        }
    }

    #[test]
    fn subscription_sensitive_context_is_not_an_unconditional_million() {
        assert_eq!(model_context_window("claude-haiku-4-5"), Some(200_000));
        for model in ["claude-sonnet-4-6", "claude-opus-4-6"] {
            assert_eq!(model_context_window(model), None);
            assert!(!has_native_million_context(model));
            assert!(can_upgrade_context(model));
            assert!(supports_auto_mode(model));
            assert!(catalog(model).unwrap().get("contextWindow").is_none());
        }
        for model in [
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-opus-5-5",
        ] {
            assert!(has_native_million_context(model));
            assert!(!can_upgrade_context(model));
            assert!(supports_tool_references(model));
        }
    }
}
