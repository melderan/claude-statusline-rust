use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────
// Everything fancy is opt-in. Default output is plain ASCII with color
// on numeric values only (percentages, ages). Config file lives at
// ~/.config/claude-statusline-rust/config.json; env vars override.

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) bar: bool,
    #[serde(default)]
    pub(crate) glyphs: bool,
    #[serde(default = "default_true")]
    pub(crate) color: bool,
    /// Residue line: how many recent user turns to show with their context
    /// cost (`res: +84k +3.1k ...`). 0 (the default) hides the line. Any JSON
    /// value is accepted here, a number or a numeric string, and clamped in
    /// code to 0..=RESIDUE_MAX by residue_turns(); anything else means 0. A
    /// bad value never fails the whole config and drops the other settings.
    /// The numbers sum to less than the ctx figure by the fixed baseline that
    /// ctx adds for the system prompt and tools.
    #[serde(default)]
    pub(crate) residue: serde_json::Value,
    /// Where metrics go. Unset: `~/.config/dbg/statusline-metrics.db` with
    /// WAL, as always. Set (or `CSR_METRICS_DB`): that file, opened with the
    /// `unix-dotfile` VFS and a DELETE journal, so it may live on a network
    /// or virtiofs mount that rejects SQLite's default locks. `~` and `~/`
    /// expand; a relative path is taken from $HOME, never from the working
    /// directory. The lock is a `<file>.lock` directory. A render that cannot
    /// take it within the busy timeout skips its row and says so once on
    /// stderr; it never removes a lock, because a lock that looks stale may
    /// belong to a slow writer, and removing it corrupts the database. A lock
    /// left by a killed process is cleared by whoever owns the file, when
    /// nothing can be writing.
    #[serde(default)]
    pub(crate) metrics_db: Option<String>,
    /// Prompt cache segment on the ctx line (`cache 98% warm 1h, cold in 42m`).
    /// On by default; `cache: false` or `CSR_CACHE=0` hides it.
    #[serde(default = "default_true")]
    pub(crate) cache: bool,
    /// The newer hook fields as segments: pace on the rate-limit lines, the
    /// 200k+ marker, PR state on the git line, effort, fast mode, session and
    /// worktree names on the misc line. On by default; `extras: false` or
    /// `CSR_EXTRAS=0` hides them all.
    #[serde(default = "default_true")]
    pub(crate) extras: bool,
}

/// Longest residue window; past ten turns the line stops being readable.
pub(crate) const RESIDUE_MAX: i64 = 10;

pub(crate) fn clamp_residue(n: i64) -> usize {
    n.clamp(0, RESIDUE_MAX) as usize
}

/// Turns in the residue window from any JSON value: 6, 6.0, "6", 300 (10),
/// -1 (0), 1e30 (10), "x" (0), null (0).
pub(crate) fn residue_turns(v: &serde_json::Value) -> usize {
    use serde_json::Value;
    let n: Option<f64> = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match n {
        Some(f) if f.is_finite() => clamp_residue(f.clamp(0.0, RESIDUE_MAX as f64) as i64),
        _ => 0,
    }
}

pub(crate) fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bar: false,
            glyphs: false,
            color: true,
            residue: serde_json::Value::from(0),
            metrics_db: None,
            cache: true,
            extras: true,
        }
    }
}

pub(crate) fn truthy(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    pub(crate) fn load() -> Self {
        let mut cfg = Self::from_file().unwrap_or_default();
        if let Ok(v) = std::env::var("CSR_BAR") {
            cfg.bar = truthy(&v);
        }
        if let Ok(v) = std::env::var("CSR_GLYPHS") {
            cfg.glyphs = truthy(&v);
        }
        // Standard NO_COLOR convention disables color whenever set (even empty).
        if std::env::var_os("NO_COLOR").is_some() {
            cfg.color = false;
        }
        if let Ok(v) = std::env::var("CSR_COLOR") {
            cfg.color = truthy(&v);
        }
        if let Ok(v) = std::env::var("CSR_RESIDUE") {
            cfg.residue = serde_json::Value::from(v);
        }
        if let Ok(v) = std::env::var("CSR_CACHE") {
            cfg.cache = truthy(&v);
        }
        if let Ok(v) = std::env::var("CSR_EXTRAS") {
            cfg.extras = truthy(&v);
        }
        cfg.apply_metrics_env(std::env::var("CSR_METRICS_DB").ok());
        cfg.residue = serde_json::Value::from(residue_turns(&cfg.residue) as i64);
        cfg
    }

    /// `CSR_METRICS_DB` overrides `metrics_db` from the file; a blank value is
    /// "not set" and leaves the file's value alone.
    pub(crate) fn apply_metrics_env(&mut self, env: Option<String>) {
        if let Some(v) = env
            && !v.trim().is_empty()
        {
            self.metrics_db = Some(v);
        }
        if let Some(p) = &self.metrics_db
            && p.trim().is_empty()
        {
            self.metrics_db = None;
        }
    }

    pub(crate) fn from_file() -> Option<Self> {
        let home = std::env::var("HOME").ok()?;
        let path = format!("{}/.config/claude-statusline-rust/config.json", home);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }
}
