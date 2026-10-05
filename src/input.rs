use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Input schema (only fields we actually use)
// ─────────────────────────────────────────────────────────────────────

/// Deserialize an optional sub-object leniently: a shape this binary does
/// not expect (a field that is an object where a string was assumed, say)
/// costs that one segment, never the whole status line. The 2026-10-06
/// lesson: `prompt_cache.last_miss_cause` arrived as an object, the parse of
/// the whole payload failed, and every line went blank.
pub(crate) fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let v = serde_json::Value::deserialize(d)?;
    if v.is_null() {
        return Ok(None);
    }
    Ok(serde_json::from_value(v).ok())
}

#[derive(Deserialize, Default)]
pub(crate) struct Input {
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) model: Option<Model>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) workspace: Option<Workspace>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) context_window: Option<ContextWindow>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) cost: Option<Cost>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) vim: Option<Vim>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) agent: Option<Agent>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) rate_limits: Option<RateLimits>,
    pub(crate) version: Option<String>,
    pub(crate) session_id: Option<String>,
    /// UUID of the user prompt being processed; one value per user turn.
    pub(crate) prompt_id: Option<String>,
    /// The most recent API response crossed 200k tokens (input, cache and
    /// output together); a fixed threshold, the long-context pricing tier.
    pub(crate) exceeds_200k_tokens: Option<bool>,
    pub(crate) fast_mode: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) effort: Option<Effort>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) thinking: Option<Thinking>,
    /// Custom name from `--name` or `/rename`, else the generated title;
    /// absent for the default `my-app-3f` style display name.
    pub(crate) session_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) prompt_cache: Option<PromptCache>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) pr: Option<Pr>,
    #[serde(default, deserialize_with = "lenient")]
    pub(crate) worktree: Option<Worktree>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Effort {
    /// `low`, `medium`, `high`, `xhigh` or `max`; live, follows `/effort`.
    pub(crate) level: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Thinking {
    pub(crate) enabled: Option<bool>,
}

/// Prompt cache statistics for the main conversation (Claude Code 2.1.251+).
#[derive(Deserialize, Default)]
pub(crate) struct PromptCache {
    pub(crate) warm: Option<bool>,
    pub(crate) caching_observed: Option<bool>,
    /// `"5m"` or `"1h"`.
    pub(crate) ttl: Option<String>,
    /// Epoch seconds when the cached prefix goes cold; null without cache tokens.
    pub(crate) expires_at: Option<i64>,
    pub(crate) misses: Option<i64>,
    /// Cache reads over all input tokens this session, 0 to 1.
    pub(crate) hit_ratio: Option<f64>,
    /// Documented as the likely cause of the last miss; the live payload
    /// (2.1.289) sends `{"causes": ["ttl_expired_1h"]}`, older text said a
    /// string. Kept as a JSON value and read by `miss_cause_text`.
    pub(crate) last_miss_cause: Option<serde_json::Value>,
    pub(crate) recache_tokens_if_cold: Option<i64>,
}

/// The open pull request (or GitLab merge request, `kind: "mr"`) for the branch.
#[derive(Deserialize, Default)]
pub(crate) struct Pr {
    pub(crate) number: Option<i64>,
    /// `approved`, `pending`, `changes_requested` or `draft`.
    pub(crate) review_state: Option<String>,
    pub(crate) kind: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct Worktree {
    pub(crate) name: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Model {
    pub(crate) display_name: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Workspace {
    pub(crate) project_dir: Option<String>,
    pub(crate) current_dir: Option<String>,
    pub(crate) git_worktree: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ContextWindow {
    pub(crate) total_input_tokens: Option<i64>,
    pub(crate) total_output_tokens: Option<i64>,
    pub(crate) context_window_size: Option<i64>,
    pub(crate) used_percentage: Option<f64>,
    pub(crate) current_usage: Option<CurrentUsage>,
}

#[derive(Deserialize, Default)]
pub(crate) struct CurrentUsage {
    pub(crate) input_tokens: Option<i64>,
    pub(crate) output_tokens: Option<i64>,
    pub(crate) cache_read_input_tokens: Option<i64>,
    pub(crate) cache_creation_input_tokens: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct Cost {
    pub(crate) total_cost_usd: Option<f64>,
    pub(crate) total_duration_ms: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct Vim {
    pub(crate) mode: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Agent {
    pub(crate) name: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RateLimits {
    pub(crate) five_hour: Option<RateWindow>,
    pub(crate) seven_day: Option<RateWindow>,
}

#[derive(Deserialize)]
pub(crate) struct RateWindow {
    pub(crate) used_percentage: Option<f64>,
    pub(crate) resets_at: Option<i64>,
}
