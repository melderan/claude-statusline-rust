use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Hook fields that arrived after the first version: prompt cache, pace,
// effort, fast mode, the 200k tier, PR state, session and worktree names.
// Every function here is pure and takes `now` so the tests pin the clock.
// Field meanings: https://code.claude.com/docs/en/statusline
// ─────────────────────────────────────────────────────────────────────

pub(crate) const GREEN: &str = "\x1b[38;2;74;222;128m";
pub(crate) const AMBER: &str = "\x1b[38;2;251;191;36m";
pub(crate) const ROSE: &str = "\x1b[38;2;251;113;133m";

pub(crate) const FIVE_HOURS: i64 = 5 * 3600;
pub(crate) const SEVEN_DAYS: i64 = 7 * 86400;

pub(crate) fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `HH:MMZ` for an epoch second, so an expiry reads as a clock time that
/// stays right between repaints (a countdown goes stale on a quiet screen).
pub(crate) fn fmt_hm_utc(ts: i64) -> String {
    let t = ts.rem_euclid(86400);
    format!("{:02}:{:02}Z", t / 3600, (t % 3600) / 60)
}

/// `in 42m`, `in 1h05m`, `in 2d3h`, or `now` once the moment has passed.
pub(crate) fn fmt_in(remaining: i64) -> String {
    if remaining <= 0 {
        return "now".to_string();
    }
    let days = remaining / 86400;
    let hours = (remaining % 86400) / 3600;
    let mins = (remaining % 3600) / 60;
    if days > 0 {
        format!("in {}d{}h", days, hours)
    } else if hours > 0 {
        format!("in {}h{:02}m", hours, mins)
    } else {
        format!("in {}m", mins.max(1))
    }
}

/// The prompt cache segment: `cache 98% warm 1h, cold in 42m (22:58Z)`,
/// or `cache cold (+254k to rewarm)`, plus `miss:N (cause)` once a miss
/// has been diagnosed. None until caching has been observed at all, so a
/// provider that never reports cache tokens costs no space.
pub(crate) fn cache_segment(pc: &PromptCache, now: i64, cfg: &Config) -> Option<String> {
    if pc.caching_observed != Some(true) {
        return None;
    }
    let rst = reset(cfg);
    let mut s = String::new();
    let hit = pc.hit_ratio.map(|h| (h * 100.0).round() as i64);
    if pc.warm == Some(true) {
        let color = if hit.unwrap_or(100) >= 90 {
            GREEN
        } else {
            AMBER
        };
        s.push_str("cache ");
        if let Some(h) = hit {
            let _ = write!(s, "{}{}%{} ", c(cfg, color), h, rst);
        }
        let _ = write!(s, "{}warm{}", c(cfg, color), rst);
        if let Some(ttl) = pc.ttl.as_deref() {
            let _ = write!(s, " {}", ttl);
        }
        if let Some(exp) = pc.expires_at {
            let _ = write!(s, ", cold {} ({})", fmt_in(exp - now), fmt_hm_utc(exp));
        }
    } else {
        let _ = write!(s, "cache {}cold{}", c(cfg, ROSE), rst);
        if let Some(h) = hit {
            let _ = write!(s, " {}%", h);
        }
        if let Some(t) = pc.recache_tokens_if_cold
            && t > 0
        {
            let _ = write!(s, " (+{}k to rewarm)", t / 1000);
        }
    }
    if let Some(m) = pc.misses
        && m > 0
    {
        let _ = write!(s, " {}miss:{}{}", c(cfg, AMBER), m, rst);
        if let Some(cause) = pc.last_miss_cause.as_ref().and_then(miss_cause_text) {
            let _ = write!(s, " ({})", cause);
        }
    }
    Some(s)
}

/// Usage pace over a rate-limit window: used fraction divided by the
/// elapsed fraction of the window. 1.0 means an even spend lands exactly
/// at the reset; above it the window runs out early. None for the first
/// 5% of a window, where the ratio is noise.
pub(crate) fn pace(used_pct: f64, resets_at: i64, window_secs: i64, now: i64) -> Option<f64> {
    if window_secs <= 0 {
        return None;
    }
    let remaining = (resets_at - now).clamp(0, window_secs);
    let elapsed = (window_secs - remaining) as f64 / window_secs as f64;
    if elapsed < 0.05 {
        return None;
    }
    Some((used_pct / 100.0) / elapsed)
}

/// ` pace 1.3x`, coloured: green at or under an even spend, amber up to
/// 1.2, rose above.
pub(crate) fn fmt_pace(p: f64, cfg: &Config) -> String {
    let color = if p > 1.2 {
        ROSE
    } else if p > 1.0 {
        AMBER
    } else {
        GREEN
    };
    format!(" {}pace {:.1}x{}", c(cfg, color), p, reset(cfg))
}

/// The open PR or MR for the branch: `PR#123 approved`, `MR!45 draft`.
pub(crate) fn pr_tag(pr: &Pr) -> Option<String> {
    let n = pr.number?;
    let mut s = if pr.kind.as_deref() == Some("mr") {
        format!("MR!{}", n)
    } else {
        format!("PR#{}", n)
    };
    if let Some(state) = pr.review_state.as_deref() {
        let _ = write!(s, " {}", state);
    }
    Some(s)
}

/// Session-mode tokens for the misc line: `effort:high`, `fast`,
/// `think:off`, `"session name"`, `wt:name`. Thinking only shows when it
/// is off, because on is the default and would be noise.
pub(crate) fn mode_tags(data: &Input) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(level) = data.effort.as_ref().and_then(|e| e.level.as_deref()) {
        tags.push(format!("effort:{}", level));
    }
    if data.fast_mode == Some(true) {
        tags.push("fast".to_string());
    }
    if data.thinking.as_ref().and_then(|t| t.enabled) == Some(false) {
        tags.push("think:off".to_string());
    }
    if let Some(name) = data.session_name.as_deref()
        && !name.is_empty()
    {
        tags.push(format!("\"{}\"", truncate(name, 32)));
    }
    if let Some(wt) = data.worktree.as_ref().and_then(|w| w.name.as_deref())
        && !wt.is_empty()
    {
        tags.push(format!("wt:{}", wt));
    }
    tags
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", cut)
}

/// The miss cause as text, from either shape the hook has sent: a string,
/// or an object whose `causes` array names one or more causes (joined with
/// `+`). Anything else is None, never a guess.
pub(crate) fn miss_cause_text(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_string());
    }
    let causes = v.get("causes")?.as_array()?;
    let names: Vec<&str> = causes.iter().filter_map(|c| c.as_str()).collect();
    (!names.is_empty()).then(|| names.join("+"))
}
