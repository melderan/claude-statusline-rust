use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Input schema (only fields we actually use)
// ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub(crate) struct Input {
    pub(crate) model: Option<Model>,
    pub(crate) workspace: Option<Workspace>,
    pub(crate) context_window: Option<ContextWindow>,
    pub(crate) cost: Option<Cost>,
    pub(crate) vim: Option<Vim>,
    pub(crate) agent: Option<Agent>,
    pub(crate) rate_limits: Option<RateLimits>,
    pub(crate) subagents: Option<Subagents>,
    pub(crate) version: Option<String>,
    pub(crate) session_id: Option<String>,
    /// UUID of the user prompt being processed; one value per user turn.
    pub(crate) prompt_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Subagents {
    pub(crate) count: Option<u32>,
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
