use rusqlite::Connection;
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Read;

// System prompt + tools + MCP tokens not surfaced to the hook JSON.
// See github.com/anthropics/claude-code/issues/13783. PAI's documented baseline.
const CONTEXT_BASELINE: i64 = 22_600;

// ─────────────────────────────────────────────────────────────────────
// Input schema (only fields we actually use)
// ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct Input {
    model: Option<Model>,
    workspace: Option<Workspace>,
    context_window: Option<ContextWindow>,
    cost: Option<Cost>,
    vim: Option<Vim>,
    agent: Option<Agent>,
    rate_limits: Option<RateLimits>,
    subagents: Option<Subagents>,
    version: Option<String>,
    session_id: Option<String>,
    /// UUID of the user prompt being processed; one value per user turn.
    prompt_id: Option<String>,
}

#[derive(Deserialize)]
struct Subagents {
    count: Option<u32>,
}

#[derive(Deserialize)]
struct Model {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Workspace {
    project_dir: Option<String>,
    current_dir: Option<String>,
    git_worktree: Option<String>,
}

#[derive(Deserialize)]
struct ContextWindow {
    total_input_tokens: Option<i64>,
    total_output_tokens: Option<i64>,
    context_window_size: Option<i64>,
    used_percentage: Option<f64>,
    current_usage: Option<CurrentUsage>,
}

#[derive(Deserialize, Default)]
struct CurrentUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct Cost {
    total_cost_usd: Option<f64>,
    total_duration_ms: Option<i64>,
}

#[derive(Deserialize)]
struct Vim {
    mode: Option<String>,
}

#[derive(Deserialize)]
struct Agent {
    name: Option<String>,
}

#[derive(Deserialize)]
struct RateLimits {
    five_hour: Option<RateWindow>,
    seven_day: Option<RateWindow>,
}

#[derive(Deserialize)]
struct RateWindow {
    used_percentage: Option<f64>,
    resets_at: Option<i64>,
}

// ─────────────────────────────────────────────────────────────────────
// Display mode
// ─────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mode {
    Compact,
    Standard,
}

fn pick_mode(cols: usize) -> Mode {
    if cols < 60 {
        Mode::Compact
    } else {
        Mode::Standard
    }
}

fn detect_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80)
}

// ─────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────
// Everything fancy is opt-in. Default output is plain ASCII with color
// on numeric values only (percentages, ages). Config file lives at
// ~/.config/claude-statusline-rust/config.json; env vars override.

#[derive(Deserialize, Debug, Clone)]
struct Config {
    #[serde(default)]
    bar: bool,
    #[serde(default)]
    glyphs: bool,
    #[serde(default = "default_true")]
    color: bool,
    /// Residue line: how many recent user turns to show with their context
    /// cost (`res: +84k +3.1k ...`). 0 (the default) hides the line. Any JSON
    /// value is accepted here, a number or a numeric string, and clamped in
    /// code to 0..=RESIDUE_MAX by residue_turns(); anything else means 0. A
    /// bad value never fails the whole config and drops the other settings.
    /// The numbers sum to less than the ctx figure by the fixed baseline that
    /// ctx adds for the system prompt and tools.
    #[serde(default)]
    residue: serde_json::Value,
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
    metrics_db: Option<String>,
}

/// Longest residue window; past ten turns the line stops being readable.
const RESIDUE_MAX: i64 = 10;

fn clamp_residue(n: i64) -> usize {
    n.clamp(0, RESIDUE_MAX) as usize
}

/// Turns in the residue window from any JSON value: 6, 6.0, "6", 300 (10),
/// -1 (0), 1e30 (10), "x" (0), null (0).
fn residue_turns(v: &serde_json::Value) -> usize {
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

fn default_true() -> bool {
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
        }
    }
}

fn truthy(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    fn load() -> Self {
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
        cfg.apply_metrics_env(std::env::var("CSR_METRICS_DB").ok());
        cfg.residue = serde_json::Value::from(residue_turns(&cfg.residue) as i64);
        cfg
    }

    /// `CSR_METRICS_DB` overrides `metrics_db` from the file; a blank value is
    /// "not set" and leaves the file's value alone.
    fn apply_metrics_env(&mut self, env: Option<String>) {
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

    fn from_file() -> Option<Self> {
        let home = std::env::var("HOME").ok()?;
        let path = format!("{}/.config/claude-statusline-rust/config.json", home);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Formatters (pure, testable)
// ─────────────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[38;2;100;116;139m";
const EMPTY_BAR: &str = "\x1b[38;2;75;82;95m";

fn fmt_duration_ms(ms: i64) -> String {
    let sec = ms / 1000;
    if sec >= 3600 {
        format!("{}h{:02}m", sec / 3600, (sec % 3600) / 60)
    } else if sec >= 60 {
        format!("{}m{:02}s", sec / 60, sec % 60)
    } else {
        format!("{}s", sec)
    }
}

/// Bytes on disk. The unit always says "B" so it cannot be read as tokens:
/// the ctx line one row below uses a bare "k" for thousands of tokens.
fn fmt_bytes(b: u64) -> String {
    if b == 0 {
        "-".to_string()
    } else if b < 1024 {
        format!("{}B", b)
    } else {
        let kb = (b as f64 / 1024.0).round();
        if kb < 1024.0 {
            format!("{kb:.0}KB")
        } else {
            format!("{:.1}MB", b as f64 / (1024.0 * 1024.0))
        }
    }
}

/// Characters, with an explicit unit so it is neither bytes nor tokens:
/// `512ch`, `4.8kch`, `48kch`.
fn fmt_chars(n: u64) -> String {
    if n < 1000 {
        format!("{n}ch")
    } else if n < 10_000 {
        format!("{:.1}kch", n as f64 / 1000.0)
    } else {
        format!("{}kch", (n as f64 / 1000.0).round() as u64)
    }
}

/// Returns (label, 24-bit ANSI color).
fn fmt_age_secs(secs: i64) -> (String, &'static str) {
    let secs = secs.max(0);
    let mins = secs / 60;
    let hrs = secs / 3600;
    let days = secs / 86400;
    let label = if mins < 1 {
        "now".to_string()
    } else if hrs < 1 {
        format!("{}m", mins)
    } else if days < 1 {
        format!("{}h", hrs)
    } else {
        format!("{}d", days)
    };
    let color = if hrs < 1 {
        "\x1b[38;2;125;211;252m"
    } else if hrs < 24 {
        "\x1b[38;2;96;165;250m"
    } else if days < 7 {
        "\x1b[38;2;59;130;246m"
    } else {
        "\x1b[38;2;99;102;241m"
    };
    (label, color)
}

/// Unix timestamp → "in Xh Ym @ Mon Apr 14 18:30 UTC"
fn fmt_reset(resets_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let remaining = resets_at - now;

    let countdown = if remaining <= 0 {
        "now".to_string()
    } else {
        let days = remaining / 86400;
        let hours = (remaining % 86400) / 3600;
        let mins = (remaining % 3600) / 60;
        if days > 0 {
            format!("{}d{}h", days, hours)
        } else if hours > 0 {
            format!("{}h{:02}m", hours, mins)
        } else {
            format!("{}m", mins)
        }
    };

    let ts = resets_at;
    let secs_per_day: i64 = 86400;
    let days_since_epoch = ts.div_euclid(secs_per_day);
    let time_of_day = ts.rem_euclid(secs_per_day);
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;

    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let _y = if m <= 2 { y + 1 } else { y };

    let weekday = ((days_since_epoch % 7) + 4).rem_euclid(7);
    let day_names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month_names = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wday = day_names[weekday as usize];
    let mon = month_names[m as usize];

    format!(
        "{} @ {} {} {} {:02}:{:02} UTC",
        countdown, wday, mon, d, hh, mm
    )
}

// ─────────────────────────────────────────────────────────────────────
// Context bar (Tailwind green → yellow → orange → red gradient)
// ─────────────────────────────────────────────────────────────────────

// Breakpoints for the Tailwind-inspired gradient (green → yellow → orange → red).
const GRADIENT_STOPS: [(i32, i32, i32); 4] = [
    (74, 222, 128), // 0%   green
    (250, 204, 21), // 33%  yellow
    (251, 146, 60), // 66%  orange
    (239, 68, 68),  // 100% red
];

fn bucket_color(pos: usize, max: usize) -> String {
    let pct = (pos * 100 / max).min(100) as i32;
    let segment = ((pct / 33).min(2)) as usize;
    let t = pct - (segment as i32) * 33;
    let denom = if segment == 2 { 34 } else { 33 };
    let (r0, g0, b0) = GRADIENT_STOPS[segment];
    let (r1, g1, b1) = GRADIENT_STOPS[segment + 1];
    let r = r0 + (r1 - r0) * t / denom;
    let g = g0 + (g1 - g0) * t / denom;
    let b = b0 + (b1 - b0) * t / denom;
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

fn context_bar(width: usize, pct: i32) -> String {
    let pct = pct.clamp(0, 100) as usize;
    let filled = (pct * width).div_ceil(100).min(width);
    let mut s = String::with_capacity(width * 12);
    for i in 1..=width {
        if i <= filled {
            s.push_str(&bucket_color(i, width));
        } else {
            s.push_str(EMPTY_BAR);
        }
        s.push('\u{26C1}'); // ⛁
        s.push_str(RESET);
    }
    s
}

/// Plain-ASCII bar for NO_COLOR / glyph-free output: `[####....]`.
fn context_bar_plain(width: usize, pct: i32) -> String {
    let pct = pct.clamp(0, 100) as usize;
    let filled = (pct * width).div_ceil(100).min(width);
    let mut s = String::with_capacity(width + 2);
    s.push('[');
    for i in 1..=width {
        s.push(if i <= filled { '#' } else { '.' });
    }
    s.push(']');
    s
}

/// Tokens in the context after the most recent API response: everything the
/// model read plus what it wrote. None when the hook gave no usage or it is 0.
fn content_tokens(cu: Option<&CurrentUsage>) -> Option<i64> {
    let cu = cu?;
    let cache_read = cu.cache_read_input_tokens.unwrap_or(0);
    let cache_creation = cu.cache_creation_input_tokens.unwrap_or(0);
    let input = cu.input_tokens.unwrap_or(0);
    let output = cu.output_tokens.unwrap_or(0);
    let content = cache_read + cache_creation + input + output;
    if content <= 0 { None } else { Some(content) }
}

/// Baseline-corrected context percent (matches /context more closely than raw used_percentage).
fn computed_ctx_pct(cu: Option<&CurrentUsage>, cap: i64) -> Option<f64> {
    if cap <= 0 {
        return None;
    }
    let used = content_tokens(cu)? + CONTEXT_BASELINE;
    Some((used as f64) * 100.0 / (cap as f64))
}

/// Signed token delta for the residue line: `+512`, `+3.1k`, `+84k`, `-120k`.
fn fmt_delta(d: i64) -> String {
    let a = d.abs();
    if a < 1000 {
        return format!("{:+}", d);
    }
    // Round to one decimal first so 9_950 and 10_049 both print as +10k.
    let k = (d as f64 / 100.0).round() / 10.0;
    if k.abs() < 10.0 {
        format!("{k:+.1}k")
    } else {
        format!("{:+}k", k.round() as i64)
    }
}

/// Per-turn context deltas from metrics rows, newest first, as (turn key,
/// content tokens). The first row seen for a key is that turn's final state.
/// Returns the last `n` deltas oldest first. When the session start is inside
/// the window, the first turn's delta is measured from 0: the launch cost.
fn turn_deltas(rows_newest_first: &[(String, i64)], n: usize) -> Vec<i64> {
    if n == 0 {
        return Vec::new();
    }
    let mut turns: Vec<i64> = Vec::with_capacity(n + 1);
    let mut last_key: Option<&str> = None;
    for (key, content) in rows_newest_first {
        if last_key == Some(key.as_str()) {
            continue;
        }
        last_key = Some(key.as_str());
        turns.push(*content);
        if turns.len() > n {
            break;
        }
    }
    turns.reverse();
    let mut deltas = Vec::with_capacity(n);
    let mut prev = if turns.len() > n { turns.remove(0) } else { 0 };
    for t in turns {
        deltas.push(t - prev);
        prev = t;
    }
    deltas
}

// ─────────────────────────────────────────────────────────────────────
// Directory + memory
// ─────────────────────────────────────────────────────────────────────

fn home_dir() -> Option<String> {
    std::env::var("HOME").ok()
}

/// Shorten a path by substituting $HOME with ~.
fn tilde(path: &str) -> String {
    if let Some(home) = home_dir()
        && let Some(rest) = path.strip_prefix(&home)
    {
        return format!("~{}", rest);
    }
    path.to_string()
}

/// If `current` is under `project`, return the relative suffix (leading "/" stripped).
/// Otherwise return the full current path (tilde-shortened).
fn relative_current(project: &str, current: &str) -> Option<String> {
    if current == project {
        return None;
    }
    if let Some(rest) = current.strip_prefix(project) {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    } else {
        Some(tilde(current))
    }
}

/// Claude Code memory slug: absolute path with '/' → '-'.
/// Matches ~/.claude/projects/<slug>/memory/ layout.
fn path_to_memory_slug(abs_path: &str) -> String {
    abs_path.replace('/', "-")
}

/// Returns (MEMORY.md bytes, other-memory-files bytes).
///
/// Only the top-level MEMORY.md is loaded at session start; every other
/// `.md` under the memory directory, at any depth, is reachable by recall,
/// so the second number walks subdirectories too.
fn memory_bytes(project_dir: &str) -> (u64, u64) {
    let home = match home_dir() {
        Some(h) => h,
        None => return (0, 0),
    };
    let slug = path_to_memory_slug(project_dir);
    let dir = std::path::PathBuf::from(format!("{}/.claude/projects/{}/memory", home, slug));
    memory_bytes_in(&dir)
}

/// Depth limit for the memory walk; no sane memory tree is this deep, and it
/// bounds the work even if the visited set misses a loop.
const MEMORY_WALK_MAX_DEPTH: usize = 8;

fn memory_bytes_in(dir: &std::path::Path) -> (u64, u64) {
    let mut index = 0u64;
    let mut other = 0u64;
    // Symlinks are followed (the memory directory may itself be a link), so
    // remember each real path, directory or file, and count it once: a link
    // back up the tree cannot loop, and a file reached twice is one file.
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((path, depth)) = stack.pop() {
        match std::fs::canonicalize(&path) {
            Ok(real) => {
                if !seen.insert(real) {
                    continue;
                }
            }
            Err(_) => continue,
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // DirEntry::metadata does not follow a symlink; fs::metadata does.
            let meta = match std::fs::metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                if depth < MEMORY_WALK_MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !name_str.ends_with(".md") {
                continue;
            }
            if let Ok(real) = std::fs::canonicalize(entry.path())
                && !seen.insert(real)
            {
                continue;
            }
            if depth == 0 && name_str == "MEMORY.md" {
                index += meta.len();
            } else {
                other += meta.len();
            }
        }
    }
    (index, other)
}

// ─────────────────────────────────────────────────────────────────────
// Always-on text: what every turn carries before anyone speaks
// ─────────────────────────────────────────────────────────────────────

/// The files Claude Code loads into every turn of a session: the user
/// CLAUDE.md, every CLAUDE.md / .claude/CLAUDE.md / CLAUDE.local.md from the
/// project directory up to the root, their `@path` imports (depth-capped like
/// Claude Code's own 5), and the memory index MEMORY.md. Characters, not
/// tokens: roughly four characters to a token for English prose.
#[derive(Debug, Default, PartialEq)]
struct AlwaysOn {
    chars: u64,
    /// (path, chars), in load order, each file once.
    files: Vec<(String, u64)>,
}

const IMPORT_MAX_DEPTH: usize = 5;

fn always_on(project_dir: &str, home: &str) -> AlwaysOn {
    let mut out = AlwaysOn::default();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if !home.is_empty() {
        roots.push(
            std::path::PathBuf::from(home)
                .join(".claude")
                .join("CLAUDE.md"),
        );
    }
    if !project_dir.is_empty() {
        // Root first, project last: the order Claude Code shows in /memory.
        let mut dirs: Vec<std::path::PathBuf> = std::path::Path::new(project_dir)
            .ancestors()
            .map(|d| d.to_path_buf())
            .collect();
        dirs.reverse();
        for d in dirs {
            roots.push(d.join("CLAUDE.md"));
            roots.push(d.join(".claude").join("CLAUDE.md"));
            roots.push(d.join("CLAUDE.local.md"));
        }
        if !home.is_empty() {
            roots.push(
                std::path::PathBuf::from(home)
                    .join(".claude")
                    .join("projects")
                    .join(path_to_memory_slug(project_dir))
                    .join("memory")
                    .join("MEMORY.md"),
            );
        }
    }
    for r in roots {
        add_always_on_file(&r, home, 0, &mut seen, &mut out);
    }
    out
}

/// Largest file read for the always-on count; anything bigger is skipped so a
/// stray import of a log or a dump cannot stall the render.
const IMPORT_MAX_BYTES: u64 = 4 * 1024 * 1024;

fn add_always_on_file(
    path: &std::path::Path,
    home: &str,
    depth: usize,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut AlwaysOn,
) {
    let Ok(real) = std::fs::canonicalize(path) else {
        return;
    };
    // Regular files only: a FIFO or a device would hang every render.
    match std::fs::metadata(&real) {
        Ok(m) if m.is_file() && m.len() <= IMPORT_MAX_BYTES => {}
        _ => return,
    }
    if !seen.insert(real) {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    // Count what the model sees; a stray invalid byte is one replacement char.
    let text = String::from_utf8_lossy(&bytes);
    let n = text.chars().count() as u64;
    out.chars += n;
    out.files.push((path.to_string_lossy().into_owned(), n));
    if depth >= IMPORT_MAX_DEPTH {
        return;
    }
    let base = path.parent().unwrap_or(std::path::Path::new("/"));
    for imp in claude_md_imports(&text) {
        let target = if let Some(rest) = imp.strip_prefix("~/") {
            if home.is_empty() {
                continue;
            }
            std::path::PathBuf::from(home).join(rest)
        } else if imp.starts_with('/') {
            std::path::PathBuf::from(&imp)
        } else {
            base.join(&imp)
        };
        add_always_on_file(&target, home, depth + 1, seen, out);
    }
}

/// `@path` imports in a CLAUDE.md: an `@` at line start or after whitespace,
/// then the path up to the next whitespace. A bare name (`@HOUSE.md`) is an
/// import too; a name that is not a file is skipped at read time. Fenced
/// code blocks and inline code are skipped, as Claude Code skips them.
fn claude_md_imports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    // An open fence: (fence char, run length). Closed by a run of the same
    // char at least as long, alone on its line.
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if let Some((fc, n)) = fence {
            let run = t.chars().take_while(|&c| c == fc).count();
            if run >= n && t[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent <= 3 {
            let fc = t.chars().next().unwrap_or(' ');
            if fc == '`' || fc == '~' {
                let run = t.chars().take_while(|&c| c == fc).count();
                if run >= 3 {
                    fence = Some((fc, run));
                    continue;
                }
            }
        }
        // Indented code block: four spaces or a tab.
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let plain = strip_code_spans(line);
        // `@` counts at line start or after whitespace, so `me@example.com`
        // is not an import. Byte offsets index `plain`, never a char count.
        let mut at_token_start = true;
        let mut rest = plain.as_str();
        while let Some(i) = rest.find('@') {
            let starts_token = if i == 0 {
                at_token_start
            } else {
                rest[..i].ends_with(char::is_whitespace)
            };
            let after = &rest[i + 1..];
            if !starts_token {
                rest = after;
                at_token_start = false;
                continue;
            }
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            let cand = after[..end].trim_end_matches([',', ';', ')', ']', '.', ':']);
            if !cand.is_empty() {
                found.push(cand.to_string());
            }
            rest = &after[end..];
            at_token_start = false;
        }
    }
    found
}

/// Replace inline code spans with a space, CommonMark style: a run of N
/// backticks opens a span that the next run of exactly N closes; a run with
/// no partner is literal text and hides nothing after it.
fn strip_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut n = 0;
        while i + n < chars.len() && chars[i + n] == '`' {
            n += 1;
        }
        // Find a closing run of exactly n.
        let mut j = i + n;
        let mut close: Option<usize> = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let mut m = 0;
                while j + m < chars.len() && chars[j + m] == '`' {
                    m += 1;
                }
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(c) => {
                out.push(' ');
                i = c + n;
            }
            None => {
                for _ in 0..n {
                    out.push('`');
                }
                i += n;
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Git info via gix (pure Rust)
// ─────────────────────────────────────────────────────────────────────

struct GitInfo {
    branch: String,
    age_secs: Option<i64>,
    ahead: u32,
    behind: u32,
    dirty: bool,
}

fn git_info(path: &str) -> Option<GitInfo> {
    let repo = gix::discover(path).ok()?;

    let head = repo.head().ok()?;
    let branch = match head.referent_name() {
        Some(n) => n.shorten().to_string(),
        None => "detached".to_string(),
    };

    let head_commit = repo.head_commit().ok();
    let age_secs = head_commit.as_ref().and_then(|c| {
        let t = c.time().ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(now - t.seconds)
    });

    let (ahead, behind) = ahead_behind(&repo).unwrap_or((0, 0));
    let dirty = is_dirty(&repo).unwrap_or(false);

    Some(GitInfo {
        branch,
        age_secs,
        ahead,
        behind,
        dirty,
    })
}

fn ahead_behind(repo: &gix::Repository) -> Option<(u32, u32)> {
    let head = repo.head().ok()?;
    let head_ref = head.try_into_referent()?;
    let head_oid = head_ref.id();
    let upstream = head_ref
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)?
        .ok()?;
    let upstream_ref = repo.find_reference(upstream.as_ref()).ok()?;
    let upstream_oid = upstream_ref.id();
    let mut ahead = 0u32;
    let mut behind = 0u32;
    // left-right counts via rev_walk
    let platform = repo
        .rev_walk([head_oid.detach(), upstream_oid.detach()])
        .sorting(gix::revision::walk::Sorting::BreadthFirst);
    // Simpler: compute merge base, then count commits on each side.
    let base = repo
        .merge_base(head_oid.detach(), upstream_oid.detach())
        .ok()?;
    for info in repo.rev_walk([head_oid.detach()]).all().ok()? {
        let info = info.ok()?;
        if info.id == base {
            break;
        }
        ahead += 1;
    }
    for info in repo.rev_walk([upstream_oid.detach()]).all().ok()? {
        let info = info.ok()?;
        if info.id == base {
            break;
        }
        behind += 1;
    }
    let _ = platform;
    Some((ahead, behind))
}

fn is_dirty(repo: &gix::Repository) -> Option<bool> {
    // gix::status returns an iterator of changes; any item means dirty.
    let platform = repo
        .status(gix::progress::Discard)
        .ok()?
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: false });
    let mut iter = platform.into_iter(None).ok()?;
    Some(iter.next().is_some())
}

// ─────────────────────────────────────────────────────────────────────
// Render
// ─────────────────────────────────────────────────────────────────────

fn pct_color(pct: f64) -> &'static str {
    if pct <= 33.0 {
        "\x1b[38;2;74;222;128m"
    } else if pct <= 66.0 {
        "\x1b[38;2;250;204;21m"
    } else {
        "\x1b[38;2;251;113;133m"
    }
}

/// Returns the ANSI escape if color is on, else empty string. Use in format!
/// like `"{}{}{}"` with color_esc, text, reset() so output goes plain when off.
fn c<'a>(cfg: &Config, color_esc: &'a str) -> &'a str {
    if cfg.color { color_esc } else { "" }
}

fn reset(cfg: &Config) -> &'static str {
    if cfg.color { RESET } else { "" }
}

fn main() {
    if std::env::args().skip(1).any(|a| a == "--flush") {
        flush_main();
        return;
    }
    let mut buf = String::with_capacity(4096);
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return;
    }

    let data: Input = serde_json::from_str(&buf).unwrap_or_default();
    let cfg = Config::load();

    let mode = pick_mode(detect_width());
    let bar_width = if mode == Mode::Compact { 8 } else { 16 };

    let mut out = String::with_capacity(512);
    let rst = reset(&cfg);

    // ── Directories ──
    let project_dir = data
        .workspace
        .as_ref()
        .and_then(|w| w.project_dir.as_deref())
        .unwrap_or("");
    let current_dir = data
        .workspace
        .as_ref()
        .and_then(|w| w.current_dir.as_deref())
        .unwrap_or("");
    let rel_cur = if !project_dir.is_empty() && !current_dir.is_empty() {
        relative_current(project_dir, current_dir)
    } else {
        None
    };

    // ── Line 1: project [cd:cur] | model | CC | dur | mem ──
    if !project_dir.is_empty() {
        out.push_str(&tilde(project_dir));
        if let Some(suffix) = &rel_cur {
            if cfg.glyphs {
                let _ = write!(out, " {}\u{2192}{} {}", c(&cfg, DIM), rst, suffix);
            } else {
                let _ = write!(out, " {}|{} cd:{}", c(&cfg, DIM), rst, suffix);
            }
        }
    }

    if let Some(name) = data.model.as_ref().and_then(|m| m.display_name.as_deref()) {
        let short = name.strip_prefix("Claude ").unwrap_or(name);
        if !out.is_empty() {
            out.push_str(" | ");
        }
        out.push_str(short);
    }

    if let Some(cc) = data.version.as_deref() {
        let _ = write!(out, " {}|{} CC:{}", c(&cfg, DIM), rst, cc);
    }

    if let Some(ms) = data.cost.as_ref().and_then(|c| c.total_duration_ms) {
        let label = if cfg.glyphs { "\u{23F1}" } else { "dur:" };
        let _ = write!(
            out,
            " {}|{} {}{}",
            c(&cfg, DIM),
            rst,
            label,
            fmt_duration_ms(ms)
        );
    }

    // Memory bytes (only if project_dir known)
    if !project_dir.is_empty() {
        let (idx, other) = memory_bytes(project_dir);
        if idx > 0 || other > 0 {
            let _ = write!(
                out,
                " {}|{} mem:{}+{}",
                c(&cfg, DIM),
                rst,
                fmt_bytes(idx),
                fmt_bytes(other)
            );
        }
    }
    // Always-on characters: CLAUDE.md chain, imports and the memory index.
    let on = always_on(project_dir, &home_dir().unwrap_or_default());

    if on.chars > 0 {
        let _ = write!(out, " {}|{} on:{}", c(&cfg, DIM), rst, fmt_chars(on.chars));
    }

    // ── Line 2: ctx (bar if opted in), session tokens, cost ──
    let cap = data
        .context_window
        .as_ref()
        .and_then(|c| c.context_window_size)
        .unwrap_or(0);
    let raw_pct = data
        .context_window
        .as_ref()
        .and_then(|c| c.used_percentage)
        .unwrap_or(0.0);
    let cu_ref = data
        .context_window
        .as_ref()
        .and_then(|c| c.current_usage.as_ref());
    let computed_pct = computed_ctx_pct(cu_ref, cap).unwrap_or(raw_pct);
    let in_tok = data
        .context_window
        .as_ref()
        .and_then(|c| c.total_input_tokens)
        .unwrap_or(0);
    let out_tok = data
        .context_window
        .as_ref()
        .and_then(|c| c.total_output_tokens)
        .unwrap_or(0);

    // Metrics first, so the residue line can read the row this update wrote.
    // Best-effort: any SQLite failure leaves the display untouched.
    let content = content_tokens(cu_ref);
    let branch = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref());
    let home = home_dir().unwrap_or_default();
    let opened = open_metrics_db(&cfg, &home);
    if let Err(e) = &opened
        && cfg.metrics_db.is_some()
    {
        // The shared file is locked or unreachable: this row is skipped,
        // never forced. One line, so a Stop hook or a log shows it.
        eprintln!("claude-statusline-rust: metrics skipped: {e}");
    }
    let residue: Vec<i64> = opened
        .ok()
        .map(|conn| {
            let logged = log_metrics(
                &conn,
                project_dir,
                branch,
                data.model.as_ref().and_then(|m| m.display_name.as_deref()),
                data.session_id.as_deref(),
                data.prompt_id.as_deref(),
                content,
                in_tok,
                out_tok,
                cap,
                raw_pct,
                data.cost.as_ref().and_then(|c| c.total_cost_usd),
                data.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref()),
                data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()),
                Some(&on),
            );
            if let Err(e) = logged
                && cfg.metrics_db.is_some()
            {
                eprintln!("claude-statusline-rust: metrics row skipped: {e}");
            }
            match (residue_turns(&cfg.residue), data.session_id.as_deref()) {
                (n, Some(sid)) if n > 0 => residue_deltas(&conn, sid, n).unwrap_or_default(),
                _ => Vec::new(),
            }
        })
        .unwrap_or_default();

    if cap > 0 {
        let current_tok = ((computed_pct / 100.0) * cap as f64) as i64;
        let pct_int = computed_pct.round() as i32;
        out.push('\n');
        out.push_str("ctx ");
        if cfg.bar {
            // Bar respects cfg.color through its ANSI codes; if color off, emit
            // plain ASCII hashes/dots instead.
            if cfg.color {
                out.push_str(&context_bar(bar_width, pct_int));
            } else {
                out.push_str(&context_bar_plain(bar_width, pct_int));
            }
            out.push(' ');
        }
        let _ = write!(
            out,
            "{}{}%{} ({}k/{}k)",
            c(&cfg, pct_color(computed_pct)),
            pct_int,
            rst,
            current_tok / 1000,
            cap / 1000
        );
        if mode == Mode::Standard {
            let _ = write!(
                out,
                " {}|{} session in:{} out:{}",
                c(&cfg, DIM),
                rst,
                in_tok,
                out_tok
            );
        }
    }

    if let Some(usd) = data.cost.as_ref().and_then(|c| c.total_cost_usd)
        && usd > 0.001
    {
        let _ = write!(out, " {}|{} ${:.2}", c(&cfg, DIM), rst, usd);
    }

    // ── Residue line: what each of the last N turns added to the context ──
    if !residue.is_empty() {
        out.push_str("\nres:");
        for d in &residue {
            out.push(' ');
            out.push_str(&fmt_delta(*d));
        }
    }

    // ── Git line ──
    let gi = if !current_dir.is_empty() {
        git_info(current_dir)
    } else if !project_dir.is_empty() {
        git_info(project_dir)
    } else {
        None
    };
    if let Some(g) = &gi {
        out.push('\n');
        let _ = write!(out, "git: {}", g.branch);
        if let Some(secs) = g.age_secs {
            let (label, color) = fmt_age_secs(secs);
            let _ = write!(out, " {}({}){}", c(&cfg, color), label, rst);
        }
        if g.dirty {
            let _ = write!(out, " {}*{}", c(&cfg, "\x1b[38;2;251;191;36m"), rst);
        }
        if g.ahead > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2191}", "\x1b[38;2;74;222;128m")
            } else {
                ("ahead:", "\x1b[38;2;74;222;128m")
            };
            let _ = write!(out, " {}{}{}{}", c(&cfg, color), sym, g.ahead, rst);
        }
        if g.behind > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2193}", "\x1b[38;2;251;113;133m")
            } else {
                ("behind:", "\x1b[38;2;251;113;133m")
            };
            let _ = write!(out, " {}{}{}{}", c(&cfg, color), sym, g.behind, rst);
        }
    } else if let Some(br) = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref())
    {
        // Fallback if gix couldn't open (e.g., not a git repo from the hook's view)
        let _ = write!(out, "\ngit: {}", br);
    }

    // ── Rate limit lines ──
    if let Some(five) = data.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref()) {
        let pct = five.used_percentage.unwrap_or(0.0);
        let icon = if pct > 80.0 {
            " !!"
        } else if pct > 50.0 {
            " !"
        } else {
            ""
        };
        let reset = five
            .resets_at
            .map(|ts| format!(", resets {}", fmt_reset(ts)))
            .unwrap_or_default();
        let _ = write!(out, "\n5h window: {:.0}% used{}{}", pct, icon, reset);
    }

    if let Some(seven) = data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()) {
        let pct = seven.used_percentage.unwrap_or(0.0);
        if pct > 0.0 {
            let reset = seven
                .resets_at
                .map(|ts| format!(", resets {}", fmt_reset(ts)))
                .unwrap_or_default();
            let _ = write!(out, "\n7d window: {:.0}% used{}", pct, reset);
        }
    }

    // ── Misc line ──
    let mut misc: Vec<String> = Vec::new();
    let sub_count = data.subagents.as_ref().and_then(|s| s.count).unwrap_or(0);
    if sub_count > 0 {
        misc.push(format!("agents:{}", sub_count));
    }
    if let Some(mode) = data.vim.as_ref().and_then(|v| v.mode.as_deref()) {
        misc.push(format!("[{}]", mode));
    }
    if let Some(name) = data.agent.as_ref().and_then(|a| a.name.as_deref()) {
        misc.push(format!("{{{}}}", name));
    }
    if !misc.is_empty() {
        let _ = write!(out, "\n{}", misc.join(" | "));
    }

    print!("{out}");
}

// ─────────────────────────────────────────────────────────────────────
// Metrics (SQLite)
// ─────────────────────────────────────────────────────────────────────

type DbResult<T> = Result<T, Box<dyn std::error::Error>>;

fn open_metrics_db(cfg: &Config, home: &str) -> DbResult<Connection> {
    if home.is_empty() {
        return Err("HOME unset".into());
    }
    match resolve_metrics_db(cfg.metrics_db.as_deref(), home) {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            open_metrics_at(&path, true)
        }
        None => {
            let _ = std::fs::create_dir_all(format!("{}/.config/dbg", home));
            open_metrics_at(&format!("{home}/.config/dbg/statusline-metrics.db"), false)
        }
    }
}

/// The shared metrics path from config: None for unset or blank; `~`, `~/x`
/// and a relative `x` all land under `home` (`~user` is a literal relative
/// name, not another user's home); an absolute path is itself.
fn resolve_metrics_db(raw: Option<&str>, home: &str) -> Option<String> {
    let p = raw?.trim();
    if p.is_empty() {
        return None;
    }
    Some(if p == "~" {
        home.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if p.starts_with('/') {
        p.to_string()
    } else {
        format!("{home}/{p}")
    })
}

/// `shared`: the file may sit on a mount that rejects fcntl locks (virtiofs,
/// NFS), so lock with a dotfile and keep a rollback journal; WAL needs shared
/// memory and cannot live there. Otherwise WAL on local disk, as before.
/// Nothing here removes a lock: in the dotfile VFS every lock level is the
/// same directory, so an old-looking lock can be a live writer or a slow
/// reader, and deleting it under them corrupts the file.
fn open_metrics_at(path: &str, shared: bool) -> DbResult<Connection> {
    let conn = if shared {
        Connection::open_with_flags_and_vfs(path, rusqlite::OpenFlags::default(), "unix-dotfile")?
    } else {
        Connection::open(path)?
    };
    // Another status line (a second pane) or a reader may hold the lock; wait
    // a little, never long enough to be seen.
    conn.busy_timeout(std::time::Duration::from_millis(if shared {
        150
    } else {
        50
    }))?;
    if shared {
        conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")?;
    } else {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    }
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Create the metrics table, and add the columns newer versions need to a
/// table created by an older one.
fn ensure_schema(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            ts              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S','now')),
            project         TEXT,
            branch          TEXT,
            model           TEXT,
            in_tokens       INTEGER,
            out_tokens      INTEGER,
            context_cap     INTEGER,
            context_pct     REAL,
            cost_usd        REAL,
            rate_5h_pct     REAL,
            rate_5h_resets  INTEGER,
            rate_7d_pct     REAL,
            rate_7d_resets  INTEGER
        );",
    )?;
    let mut have: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA table_info(metrics)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for n in names {
            have.push(n?);
        }
    }
    for (name, ty) in [
        ("session_id", "TEXT"),
        ("prompt_id", "TEXT"),
        ("content", "INTEGER"),
        ("always_on_chars", "INTEGER"),
        ("always_on_files", "TEXT"),
    ] {
        if !have.iter().any(|h| h == name) {
            conn.execute_batch(&format!("ALTER TABLE metrics ADD COLUMN {name} {ty};"))?;
        }
    }
    // The residue query reads one session's rows newest first.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS metrics_session_id ON metrics(session_id, id);",
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn log_metrics(
    conn: &Connection,
    project: &str,
    branch: Option<&str>,
    model: Option<&str>,
    session_id: Option<&str>,
    prompt_id: Option<&str>,
    content: Option<i64>,
    in_tokens: i64,
    out_tokens: i64,
    context_cap: i64,
    context_pct: f64,
    cost_usd: Option<f64>,
    five_hour: Option<&RateWindow>,
    seven_day: Option<&RateWindow>,
    always_on: Option<&AlwaysOn>,
) -> DbResult<()> {
    let last: Option<(i64, i64, f64, f64, i64, i64)> = conn
        .query_row(
            "SELECT in_tokens, out_tokens, COALESCE(rate_5h_pct, -1), COALESCE(rate_7d_pct, -1), COALESCE(content, -1), COALESCE(always_on_chars, -1) FROM metrics ORDER BY id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .ok();
    let cur_on: i64 = always_on.map(|a| a.chars as i64).unwrap_or(-1);
    let on_files: Option<String> = always_on.and_then(|a| serde_json::to_string(&a.files).ok());

    let cur_5h = five_hour.and_then(|w| w.used_percentage).unwrap_or(-1.0);
    let cur_7d = seven_day.and_then(|w| w.used_percentage).unwrap_or(-1.0);
    let cur_content = content.unwrap_or(-1);

    if let Some((last_in, last_out, last_5h, last_7d, last_content, last_on)) = last
        && last_in == in_tokens
        && last_out == out_tokens
        && (last_5h - cur_5h).abs() < 0.01
        && (last_7d - cur_7d).abs() < 0.01
        && last_content == cur_content
        && last_on == cur_on
    {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO metrics (ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets, always_on_chars, always_on_files)
         VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            project,
            branch,
            model,
            session_id,
            prompt_id,
            content,
            in_tokens,
            out_tokens,
            context_cap,
            context_pct,
            cost_usd,
            five_hour.and_then(|w| w.used_percentage),
            five_hour.and_then(|w| w.resets_at),
            seven_day.and_then(|w| w.used_percentage),
            seven_day.and_then(|w| w.resets_at),
            always_on.map(|a| a.chars as i64),
            on_files,
        ],
    )?;

    Ok(())
}

/// Context deltas of the last `n` user turns of `session_id`, oldest first.
/// Rows without a prompt_id (older Claude Code) each count as a turn.
fn residue_deltas(conn: &Connection, session_id: &str, n: usize) -> DbResult<Vec<i64>> {
    // One row per turn: the last row of each prompt_id. Bounded by turns, so
    // a turn with any number of API responses never pushes older turns out.
    let limit = n as i64 + 1;
    let mut stmt = conn.prepare(
        "SELECT id, content FROM metrics
         WHERE id IN (
             SELECT MAX(id) FROM metrics
             WHERE session_id = ?1 AND content IS NOT NULL
             GROUP BY COALESCE(prompt_id, 'row-' || id)
         )
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, limit], |row| {
        let id: i64 = row.get(0)?;
        let content: i64 = row.get(1)?;
        Ok((id.to_string(), content))
    })?;
    let mut newest_first = Vec::new();
    for r in rows {
        newest_first.push(r?);
    }
    Ok(turn_deltas(&newest_first, n))
}

// ─────────────────────────────────────────────────────────────────────
// --flush: copy local metrics into the house recorder
// ─────────────────────────────────────────────────────────────────────
//
// The render writes its rows to the local file (fast, WAL, survives a killed
// render). `claude-statusline-rust --flush` copies the rows newer than the
// last flushed id into a shared recorder database in one transaction, then
// records the new high-water mark locally. It is the only path that writes
// to the mount. It never removes a lock: a recorder it cannot open or lock
// within the busy timeout means one stderr line and exit 0, so a Stop hook
// is never blocked and nothing is forced.
//
// Recorder schema (house ADR 0015, proposed): measures(id, ts, room, source,
// source_id, session_id, prompt_id, kind, key, value, unit, data) with
// UNIQUE(room, source, source_id), written with INSERT OR IGNORE so a flush
// killed between the insert and the mark cannot write a row twice.

const RECORDER_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const FLUSH_BATCH: i64 = 2000;
const FLUSH_MAX_BATCHES: usize = 10;

fn flush_main() {
    let cfg = Config::load();
    let home = home_dir().unwrap_or_default();
    let Some(recorder) = std::env::var("CSR_RECORDER_DB")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        eprintln!("claude-statusline-rust --flush: CSR_RECORDER_DB is not set; nothing to do");
        return;
    };
    let room = std::env::var("CSR_ROOM")
        .ok()
        .or_else(|| std::env::var("SANDBOX_NAME").ok())
        .filter(|v| !v.trim().is_empty());
    let Some(room) = room else {
        eprintln!(
            "claude-statusline-rust --flush: CSR_ROOM (or SANDBOX_NAME) is not set; nothing to do"
        );
        return;
    };
    match run_flush(&cfg, &home, &recorder, &room) {
        Ok(n) => println!("claude-statusline-rust --flush: {n} row(s) to {recorder}"),
        Err(e) => eprintln!("claude-statusline-rust --flush: skipped: {e}"),
    }
}

/// Open the recorder the house way: dotfile lock, rollback journal, a few
/// seconds of patience, no lock removal. Creates the table and indexes.
fn open_recorder(path: &str) -> DbResult<Connection> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn =
        Connection::open_with_flags_and_vfs(path, rusqlite::OpenFlags::default(), "unix-dotfile")?;
    conn.busy_timeout(RECORDER_BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS measures (
            id         INTEGER PRIMARY KEY,
            ts         TEXT NOT NULL,
            room       TEXT NOT NULL,
            source     TEXT NOT NULL,
            source_id  INTEGER NOT NULL,
            session_id TEXT,
            prompt_id  TEXT,
            kind       TEXT NOT NULL,
            key        TEXT NOT NULL,
            value      REAL,
            unit       TEXT,
            data       TEXT,
            UNIQUE(room, source, source_id)
         );
         CREATE INDEX IF NOT EXISTS measures_room_ts ON measures(room, ts);
         CREATE INDEX IF NOT EXISTS measures_kind_key_ts ON measures(kind, key, ts);
         CREATE INDEX IF NOT EXISTS measures_session_prompt ON measures(session_id, prompt_id);",
    )?;
    Ok(conn)
}

/// The local high-water mark per recorder path.
fn ensure_flush_state(local: &Connection) -> DbResult<()> {
    local.execute_batch(
        "CREATE TABLE IF NOT EXISTS flush_state (
            target  TEXT PRIMARY KEY,
            last_id INTEGER NOT NULL,
            ts      TEXT NOT NULL
         );",
    )?;
    Ok(())
}

fn last_flushed_id(local: &Connection, target: &str) -> DbResult<i64> {
    ensure_flush_state(local)?;
    Ok(local
        .query_row(
            "SELECT last_id FROM flush_state WHERE target = ?1",
            [target],
            |r| r.get(0),
        )
        .unwrap_or(0))
}

/// ISO 8601 UTC with milliseconds; rows from before the millisecond format
/// get ".000Z" so every recorder row has one shape.
fn recorder_ts(local_ts: &str) -> String {
    if local_ts.ends_with('Z') {
        local_ts.to_string()
    } else {
        format!("{local_ts}.000Z")
    }
}

/// One local metrics row, as read for the flush.
struct LocalRow {
    id: i64,
    ts: String,
    session_id: Option<String>,
    prompt_id: Option<String>,
    content: Option<i64>,
    always_on_chars: Option<i64>,
    data: serde_json::Value,
}

fn read_local_rows(local: &Connection, after_id: i64) -> DbResult<Vec<LocalRow>> {
    let mut stmt = local.prepare(
        "SELECT id, ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens,
                context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets,
                always_on_chars, always_on_files
         FROM metrics WHERE id > ?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![after_id, FLUSH_BATCH], |r| {
        let files_text: Option<String> = r.get(18)?;
        let files = files_text
            .as_deref()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
            .unwrap_or(serde_json::Value::Null);
        let data = serde_json::json!({
            "project": r.get::<_, Option<String>>(2)?,
            "branch": r.get::<_, Option<String>>(3)?,
            "model": r.get::<_, Option<String>>(4)?,
            "content": r.get::<_, Option<i64>>(7)?,
            "in_tokens": r.get::<_, Option<i64>>(8)?,
            "out_tokens": r.get::<_, Option<i64>>(9)?,
            "context_cap": r.get::<_, Option<i64>>(10)?,
            "context_pct": r.get::<_, Option<f64>>(11)?,
            "cost_usd": r.get::<_, Option<f64>>(12)?,
            "rate_5h_pct": r.get::<_, Option<f64>>(13)?,
            "rate_5h_resets": r.get::<_, Option<i64>>(14)?,
            "rate_7d_pct": r.get::<_, Option<f64>>(15)?,
            "rate_7d_resets": r.get::<_, Option<i64>>(16)?,
            "always_on_chars": r.get::<_, Option<i64>>(17)?,
            "always_on_files": files,
        });
        Ok(LocalRow {
            id: r.get(0)?,
            ts: r.get(1)?,
            session_id: r.get(5)?,
            prompt_id: r.get(6)?,
            content: r.get(7)?,
            always_on_chars: r.get(17)?,
            data,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Copy local rows newer than the last flushed id into the recorder. Returns
/// the number of local rows flushed. One call row per local row
/// (kind=statusline, key=call, value=content tokens, the rest as JSON) and,
/// whenever the always-on count differs from the previous local row, one
/// scalar row (kind=always_on, key=chars) so the change is a plain series.
fn run_flush(cfg: &Config, home: &str, recorder_path: &str, room: &str) -> DbResult<usize> {
    let local = open_metrics_db(cfg, home)?;
    let recorder = open_recorder(recorder_path)?;
    let mut total = 0usize;
    for _ in 0..FLUSH_MAX_BATCHES {
        let last = last_flushed_id(&local, recorder_path)?;
        let rows = read_local_rows(&local, last)?;
        if rows.is_empty() {
            break;
        }
        // The always-on value of the row before this batch, for change detection.
        let mut prev_on: Option<i64> = local
            .query_row(
                "SELECT always_on_chars FROM metrics WHERE id <= ?1 AND always_on_chars IS NOT NULL ORDER BY id DESC LIMIT 1",
                [last],
                |r| r.get(0),
            )
            .ok();
        let tx = recorder.unchecked_transaction()?;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO measures (ts, room, source, source_id, session_id, prompt_id, kind, key, value, unit, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for r in &rows {
                let ts = recorder_ts(&r.ts);
                ins.execute(rusqlite::params![
                    ts,
                    room,
                    "statusline",
                    r.id,
                    r.session_id,
                    r.prompt_id,
                    "statusline",
                    "call",
                    r.content.map(|c| c as f64),
                    "tokens",
                    r.data.to_string(),
                ])?;
                if let Some(on) = r.always_on_chars
                    && prev_on != Some(on)
                {
                    ins.execute(rusqlite::params![
                        ts,
                        room,
                        "statusline.always_on",
                        r.id,
                        r.session_id,
                        r.prompt_id,
                        "always_on",
                        "chars",
                        on as f64,
                        "chars",
                        r.data.get("always_on_files").map(|v| v.to_string()),
                    ])?;
                    prev_on = Some(on);
                }
            }
        }
        tx.commit()?;
        let new_last = rows.last().map(|r| r.id).unwrap_or(last);
        local.execute(
            "INSERT INTO flush_state (target, last_id, ts) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(target) DO UPDATE SET last_id = excluded.last_id, ts = excluded.ts",
            rusqlite::params![recorder_path, new_last],
        )?;
        total += rows.len();
        if (rows.len() as i64) < FLUSH_BATCH {
            break;
        }
    }
    Ok(total)
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_thresholds() {
        assert_eq!(pick_mode(0), Mode::Compact);
        assert_eq!(pick_mode(59), Mode::Compact);
        assert_eq!(pick_mode(60), Mode::Standard);
        assert_eq!(pick_mode(200), Mode::Standard);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(fmt_duration_ms(0), "0s");
        assert_eq!(fmt_duration_ms(5_000), "5s");
        assert_eq!(fmt_duration_ms(65_000), "1m05s");
        assert_eq!(fmt_duration_ms(3_725_000), "1h02m");
        assert_eq!(fmt_duration_ms(7_320_000), "2h02m");
    }

    #[test]
    fn bytes_formatting() {
        assert_eq!(fmt_bytes(0), "-");
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(1536), "2KB");
        assert_eq!(
            fmt_bytes(25_363),
            "25KB",
            "MEMORY.md-sized index reads as KB, not tokens"
        );
        assert_eq!(fmt_bytes(371_005), "362KB");
        assert_eq!(fmt_bytes(1_048_575), "1.0MB", "never 1024KB");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0MB");
    }

    #[test]
    fn age_formatting() {
        let (l, _) = fmt_age_secs(0);
        assert_eq!(l, "now");
        let (l, _) = fmt_age_secs(30);
        assert_eq!(l, "now");
        let (l, _) = fmt_age_secs(300);
        assert_eq!(l, "5m");
        let (l, _) = fmt_age_secs(7200);
        assert_eq!(l, "2h");
        let (l, _) = fmt_age_secs(90000);
        assert_eq!(l, "1d");
    }

    #[test]
    fn memory_slug() {
        assert_eq!(
            path_to_memory_slug("/Users/foo/code/bar"),
            "-Users-foo-code-bar"
        );
        assert_eq!(path_to_memory_slug("/"), "-");
    }

    #[test]
    fn relative_current_semantics() {
        assert_eq!(
            relative_current("/a/b", "/a/b").as_deref(),
            None,
            "same path → no suffix"
        );
        assert_eq!(relative_current("/a/b", "/a/b/c/d").as_deref(), Some("c/d"));
        assert_eq!(
            relative_current("/a/b", "/x/y").as_deref(),
            Some("/x/y"),
            "unrelated → absolute"
        );
    }

    #[test]
    fn computed_ctx_with_baseline() {
        let cu = CurrentUsage {
            input_tokens: Some(1000),
            output_tokens: Some(500),
            cache_read_input_tokens: Some(10_000),
            cache_creation_input_tokens: Some(5_000),
        };
        let pct = computed_ctx_pct(Some(&cu), 200_000).unwrap();
        // (10000+5000+1000+500) + 22600 = 39100 / 200000 = 19.55%
        assert!((pct - 19.55).abs() < 0.01, "got {}", pct);
    }

    #[test]
    fn computed_ctx_none_without_data() {
        assert!(computed_ctx_pct(None, 200_000).is_none());
        let empty = CurrentUsage::default();
        assert!(computed_ctx_pct(Some(&empty), 200_000).is_none());
        assert!(computed_ctx_pct(Some(&empty), 0).is_none());
    }

    #[test]
    fn context_bar_bounds() {
        let b = context_bar(8, -10);
        assert!(b.contains('\u{26C1}'));
        let b = context_bar(8, 999);
        assert!(b.contains('\u{26C1}'));
        // 0% → no filled buckets (only empty color)
        let b = context_bar(4, 0);
        let filled_count = b.matches("\x1b[38;2;74").count();
        assert_eq!(filled_count, 0);
        // 100% → all filled
        let b = context_bar(4, 100);
        assert_eq!(b.matches(EMPTY_BAR).count(), 0);
    }

    #[test]
    fn context_bar_plain_shapes() {
        assert_eq!(context_bar_plain(4, 0), "[....]");
        assert_eq!(context_bar_plain(4, 100), "[####]");
        // 50% of 4 = 2 filled
        assert_eq!(context_bar_plain(4, 50), "[##..]");
        // Out-of-range clamp
        assert_eq!(context_bar_plain(4, 999), "[####]");
        assert_eq!(context_bar_plain(4, -50), "[....]");
    }

    #[test]
    fn config_defaults_are_opt_in() {
        let cfg = Config::default();
        assert!(!cfg.bar, "bar should be opt-in");
        assert!(!cfg.glyphs, "glyphs should be opt-in");
        assert!(cfg.color, "color should be on by default");
        assert_eq!(
            residue_turns(&cfg.residue),
            0,
            "residue line should be opt-in"
        );
    }

    #[test]
    fn residue_config_never_breaks_the_rest() {
        // An out-of-range residue must not fail the whole config and drop bar.
        let cfg: Config = serde_json::from_str(r#"{"bar": true, "residue": 300}"#).unwrap();
        assert!(cfg.bar);
        assert_eq!(residue_turns(&cfg.residue), 10);
        let cfg: Config = serde_json::from_str(r#"{"glyphs": true, "residue": -1}"#).unwrap();
        assert!(cfg.glyphs);
        assert_eq!(residue_turns(&cfg.residue), 0);
        // Not integers either: a float, a string, a huge number, junk, null.
        for (raw, want) in [
            (r#"{"bar": true, "residue": 2.5}"#, 2),
            (r#"{"bar": true, "residue": "4"}"#, 4),
            (r#"{"bar": true, "residue": 99999999999999999999}"#, 10),
            (r#"{"bar": true, "residue": "x"}"#, 0),
            (r#"{"bar": true, "residue": null}"#, 0),
            (r#"{"bar": true, "residue": [6]}"#, 0),
        ] {
            let cfg: Config = serde_json::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert!(cfg.bar, "{raw}: bar must survive");
            assert_eq!(residue_turns(&cfg.residue), want, "{raw}");
        }
        assert_eq!(clamp_residue(6), 6);
        assert_eq!(clamp_residue(i64::MAX), 10);
    }

    #[test]
    fn fmt_delta_shapes() {
        assert_eq!(fmt_delta(0), "+0");
        assert_eq!(fmt_delta(512), "+512");
        assert_eq!(fmt_delta(-900), "-900");
        assert_eq!(fmt_delta(3_140), "+3.1k");
        assert_eq!(fmt_delta(9_940), "+9.9k");
        assert_eq!(fmt_delta(9_999), "+10k", "one shape around 10k");
        assert_eq!(fmt_delta(10_049), "+10k");
        assert_eq!(fmt_delta(84_200), "+84k");
        assert_eq!(fmt_delta(-120_400), "-120k");
    }

    fn rows(v: &[(&str, i64)]) -> Vec<(String, i64)> {
        v.iter().map(|(k, c)| (k.to_string(), *c)).collect()
    }

    #[test]
    fn turn_deltas_groups_rows_by_prompt() {
        // Newest first. Turn c had three API responses; its final state is 100_000.
        let r = rows(&[
            ("c", 100_000),
            ("c", 97_000),
            ("c", 90_000),
            ("b", 88_000),
            ("a", 84_000),
        ]);
        // Window wider than the session: first delta is the launch cost from 0.
        assert_eq!(turn_deltas(&r, 6), vec![84_000, 4_000, 12_000]);
        // Window of 2: oldest turn is the baseline, not shown.
        assert_eq!(turn_deltas(&r, 2), vec![4_000, 12_000]);
        assert_eq!(turn_deltas(&r, 1), vec![12_000]);
        assert!(turn_deltas(&r, 0).is_empty());
        assert!(turn_deltas(&[], 5).is_empty());
    }

    #[test]
    fn turn_deltas_show_compaction_as_negative() {
        let r = rows(&[("c", 30_000), ("b", 150_000), ("a", 84_000)]);
        assert_eq!(turn_deltas(&r, 10), vec![84_000, 66_000, -120_000]);
    }

    #[test]
    fn schema_migrates_old_table_and_residue_reads_back() {
        let conn = Connection::open_in_memory().unwrap();
        // A table as the previous release created it: no session, prompt or content.
        conn.execute_batch(
            "CREATE TABLE metrics (
                id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, project TEXT, branch TEXT,
                model TEXT, in_tokens INTEGER, out_tokens INTEGER, context_cap INTEGER,
                context_pct REAL, cost_usd REAL, rate_5h_pct REAL, rate_5h_resets INTEGER,
                rate_7d_pct REAL, rate_7d_resets INTEGER);
             INSERT INTO metrics (project, in_tokens, out_tokens) VALUES ('old', 1, 1);",
        )
        .unwrap();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap(); // idempotent
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(metrics)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for want in [
            "session_id",
            "prompt_id",
            "content",
            "always_on_chars",
            "always_on_files",
        ] {
            assert!(cols.iter().any(|c| c == want), "missing column {want}");
        }

        let mut turn = 0;
        let mut log = |prompt: &str, content: i64, in_t: i64| {
            turn += 1;
            log_metrics(
                &conn,
                "proj",
                None,
                Some("Fable"),
                Some("s1"),
                Some(prompt),
                Some(content),
                in_t,
                turn,
                200_000,
                10.0,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        };
        log("p1", 84_000, 84_000);
        log("p2", 88_000, 172_000);
        log("p3", 90_000, 262_000);
        log("p3", 100_000, 362_000);
        // Identical update: deduplicated, no new row.
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap();
        log_metrics(
            &conn,
            "proj",
            None,
            Some("Fable"),
            Some("s1"),
            Some("p3"),
            Some(100_000),
            362_000,
            turn,
            200_000,
            10.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "unchanged update must not add a row");

        assert_eq!(
            residue_deltas(&conn, "s1", 10).unwrap(),
            vec![84_000, 4_000, 12_000]
        );
        assert_eq!(residue_deltas(&conn, "s1", 2).unwrap(), vec![4_000, 12_000]);
        assert!(residue_deltas(&conn, "other", 5).unwrap().is_empty());

        // A turn with many API responses must not push older turns out of the
        // window (review finding: the old row-based limit did).
        for i in 1..=200_i64 {
            log_metrics(
                &conn,
                "proj",
                None,
                Some("Fable"),
                Some("s1"),
                Some("p4"),
                Some(100_000 + i * 10),
                362_000 + i * 10,
                1_000 + i,
                200_000,
                10.0,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(residue_deltas(&conn, "s1", 1).unwrap(), vec![2_000]);
        assert_eq!(
            residue_deltas(&conn, "s1", 4).unwrap(),
            vec![84_000, 4_000, 12_000, 2_000]
        );

        // Rows without a prompt_id (older Claude Code) are one turn each.
        log_metrics(
            &conn,
            "proj",
            None,
            None,
            Some("s2"),
            None,
            Some(50_000),
            1,
            1,
            200_000,
            1.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        log_metrics(
            &conn,
            "proj",
            None,
            None,
            Some("s2"),
            None,
            Some(53_000),
            2,
            2,
            200_000,
            1.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(residue_deltas(&conn, "s2", 5).unwrap(), vec![50_000, 3_000]);
    }

    #[test]
    fn memory_bytes_walks_subdirectories() {
        let root = std::env::temp_dir().join(format!(
            "csr-mem-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let rooms = root.join("rooms").join("tts");
        std::fs::create_dir_all(&rooms).unwrap();
        std::fs::write(root.join("MEMORY.md"), vec![b'x'; 100]).unwrap();
        std::fs::write(root.join("a.md"), vec![b'x'; 10]).unwrap();
        std::fs::write(root.join("notes.txt"), vec![b'x'; 1000]).unwrap();
        std::fs::write(rooms.join("b.md"), vec![b'x'; 20]).unwrap();
        // A nested MEMORY.md is an ordinary file, not the index.
        std::fs::write(rooms.join("MEMORY.md"), vec![b'x'; 30]).unwrap();
        let (idx, other) = memory_bytes_in(&root);
        assert_eq!(idx, 100);
        assert_eq!(other, 60, "a.md + rooms/tts/b.md + rooms/tts/MEMORY.md");
        assert_eq!(memory_bytes_in(&root.join("missing")), (0, 0));

        // Symlinks: a linked file counts, a linked directory is walked, and a
        // link back to the root neither loops nor double-counts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = root
                .join("..")
                .join(format!("csr-mem-outside-{}", std::process::id()));
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("far.md"), vec![b'x'; 7]).unwrap();
            symlink(outside.join("far.md"), root.join("link.md")).unwrap();
            symlink(&outside, root.join("linked-dir")).unwrap();
            symlink(&root, rooms.join("loop")).unwrap();
            let (idx, other) = memory_bytes_in(&root);
            assert_eq!(idx, 100);
            assert_eq!(
                other,
                60 + 7,
                "link.md and linked-dir/far.md are one file, counted once"
            );
            // The memory directory itself may be a symlink (the house layout).
            let link_to_root = outside.join("memory");
            symlink(&root, &link_to_root).unwrap();
            assert_eq!(memory_bytes_in(&link_to_root), (100, 67));
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_md_import_syntax() {
        let text = "See @/abs/file.md and @~/home.md here\n@./rel.md\n  @../up.md,\nmail me@example.com or @handle\n```\n@/in/fence.md\n```\n@docs/guide.md.\n@HOUSE.md\n~~~\n@/in/tilde/fence.md\n~~~\nuse `@/in/code.md` not that\n@./é.md @./b.md\n``@/in/double.md``\n````\n```\n@/in/four.md\n````\n    @/indented.md\n`unmatched @./after.md\nsee `code @/in/span.md` here\n";
        assert_eq!(
            claude_md_imports(text),
            vec![
                "/abs/file.md",
                "~/home.md",
                "./rel.md",
                "../up.md",
                "handle",
                "docs/guide.md",
                "HOUSE.md",
                "./é.md",
                "./b.md",
                "./after.md"
            ]
        );
    }

    #[test]
    fn code_span_stripping() {
        assert_eq!(strip_code_spans("a `b` c"), "a   c");
        assert_eq!(strip_code_spans("``x `y` z`` w"), "  w");
        assert_eq!(strip_code_spans("`open @./x.md"), "`open @./x.md");
        assert_eq!(strip_code_spans("no code"), "no code");
    }

    fn fresh_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "csr-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn always_on_counts_the_chain_once() {
        let root = fresh_dir("on");
        let home = root.join("home");
        let proj = root.join("repos").join("app");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(proj.join(".claude")).unwrap();
        let w = |p: &std::path::Path, text: &str| std::fs::write(p, text).unwrap();
        // User file imports an absolute file; that file imports the user file back (a cycle).
        let shared = root.join("shared.md");
        w(
            &home.join(".claude").join("CLAUDE.md"),
            &format!("@{}\n", shared.display()),
        );
        w(&shared, "shared text\n@./home/.claude/CLAUDE.md\n");
        assert!(
            root.join("home").join(".claude").join("CLAUDE.md").exists(),
            "cycle target exists"
        );
        // Parent-directory CLAUDE.md: relative import, bare import, fenced import, inline code.
        w(
            &root.join("repos").join("CLAUDE.md"),
            "parent\n@./inc.md\n@HOUSE.md\n```\n@./ignored.md\n```\nsee `@./ignored.md`\n",
        );
        w(&root.join("repos").join("inc.md"), "12345");
        w(&root.join("repos").join("HOUSE.md"), "house rules");
        w(&root.join("repos").join("ignored.md"), "should not count");
        // Project files; the local one has a non-ASCII name and invalid UTF-8 inside.
        w(&proj.join("CLAUDE.md"), "project\n@./é.md @./b.md\n");
        w(&proj.join("é.md"), "éé"); // 2 chars, 4 bytes
        w(&proj.join("b.md"), "bb");
        w(&proj.join(".claude").join("CLAUDE.md"), "dot");
        std::fs::write(proj.join("CLAUDE.local.md"), b"loc\xffal").unwrap(); // 6 chars after lossy
        // Memory index for this project.
        let mem = home
            .join(".claude")
            .join("projects")
            .join(path_to_memory_slug(&proj.to_string_lossy()))
            .join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        w(&mem.join("MEMORY.md"), "memory index");
        w(&mem.join("other.md"), "not always on");

        let on = always_on(&proj.to_string_lossy(), &home.to_string_lossy());
        let names: Vec<(String, u64)> = on
            .files
            .iter()
            .map(|(p, n)| (p.rsplit('/').next().unwrap().to_string(), *n))
            .collect();
        let _ = std::fs::remove_dir_all(&root);
        let user_len = format!("@{}\n", shared.display()).chars().count() as u64;
        assert_eq!(
            names,
            vec![
                ("CLAUDE.md".to_string(), user_len),
                ("shared.md".to_string(), 38),
                ("CLAUDE.md".to_string(), 69),
                ("inc.md".to_string(), 5),
                ("HOUSE.md".to_string(), 11),
                ("CLAUDE.md".to_string(), 24),
                ("é.md".to_string(), 2),
                ("b.md".to_string(), 2),
                ("CLAUDE.md".to_string(), 3),
                ("CLAUDE.local.md".to_string(), 6),
                ("MEMORY.md".to_string(), 12),
            ]
        );
        assert_eq!(on.chars, names.iter().map(|(_, n)| n).sum::<u64>());
        assert_eq!(always_on("", ""), AlwaysOn::default());
    }

    #[test]
    fn always_on_import_depth_is_capped_at_five() {
        let root = fresh_dir("depth");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        // user CLAUDE.md (depth 0) -> c1 -> c2 -> c3 -> c4 -> c5 -> c6
        std::fs::write(home.join(".claude").join("CLAUDE.md"), "@./c1.md").unwrap();
        for i in 1..=6 {
            std::fs::write(
                home.join(".claude").join(format!("c{i}.md")),
                format!("x @./c{}.md", i + 1),
            )
            .unwrap();
        }
        let on = always_on("", &home.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
        let names: Vec<&str> = on
            .files
            .iter()
            .map(|(p, _)| p.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["CLAUDE.md", "c1.md", "c2.md", "c3.md", "c4.md", "c5.md"]
        );
    }

    #[test]
    fn always_on_skips_an_oversized_import() {
        let root = fresh_dir("big");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join("CLAUDE.md"),
            "@./big.md\n@./ok.md",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude").join("big.md"),
            vec![b'x'; IMPORT_MAX_BYTES as usize + 1],
        )
        .unwrap();
        std::fs::write(home.join(".claude").join("ok.md"), "ok").unwrap();
        let on = always_on("", &home.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
        let names: Vec<&str> = on
            .files
            .iter()
            .map(|(p, _)| p.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["CLAUDE.md", "ok.md"],
            "a file past the size cap is skipped"
        );
        assert_eq!(on.chars, 18 + 2);
    }

    #[cfg(unix)]
    #[test]
    fn always_on_skips_a_fifo_import() {
        let root = fresh_dir("fifo");
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let fifo = home.join(".claude").join("pipe.md");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(made, "mkfifo available");
        std::fs::write(
            home.join(".claude").join("CLAUDE.md"),
            "@./pipe.md\n@./ok.md",
        )
        .unwrap();
        std::fs::write(home.join(".claude").join("ok.md"), "ok").unwrap();
        // Without the regular-file check the read blocks forever; fail on a
        // deadline instead of hanging the suite.
        let (tx, rx) = std::sync::mpsc::channel();
        let h = home.to_string_lossy().to_string();
        std::thread::spawn(move || {
            let _ = tx.send(always_on("", &h));
        });
        let on = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("always_on hung on a FIFO import");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(on.files.len(), 2);
        assert_eq!(on.chars, 19 + 2);
    }

    #[test]
    fn metrics_db_env_override() {
        let mut cfg = Config::default();
        cfg.metrics_db = Some("/from/file.sqlite".into());
        cfg.apply_metrics_env(Some("   ".into()));
        assert_eq!(
            cfg.metrics_db.as_deref(),
            Some("/from/file.sqlite"),
            "blank env is not set"
        );
        cfg.apply_metrics_env(None);
        assert_eq!(cfg.metrics_db.as_deref(), Some("/from/file.sqlite"));
        cfg.apply_metrics_env(Some("/from/env.sqlite".into()));
        assert_eq!(cfg.metrics_db.as_deref(), Some("/from/env.sqlite"));
        let mut blank = Config::default();
        blank.metrics_db = Some("".into());
        blank.apply_metrics_env(None);
        assert_eq!(blank.metrics_db, None, "blank in the file means unset");
    }

    #[test]
    fn metrics_db_path_resolution() {
        assert_eq!(resolve_metrics_db(None, "/h"), None);
        assert_eq!(resolve_metrics_db(Some(""), "/h"), None);
        assert_eq!(resolve_metrics_db(Some("  "), "/h"), None);
        assert_eq!(
            resolve_metrics_db(Some("~/a/b.sqlite"), "/h").as_deref(),
            Some("/h/a/b.sqlite")
        );
        assert_eq!(resolve_metrics_db(Some("~"), "/h").as_deref(), Some("/h"));
        assert_eq!(
            resolve_metrics_db(Some("~bob/x.sqlite"), "/h").as_deref(),
            Some("/h/~bob/x.sqlite"),
            "~user is a literal relative name"
        );
        assert_eq!(
            resolve_metrics_db(Some("rel.sqlite"), "/h").as_deref(),
            Some("/h/rel.sqlite")
        );
        assert_eq!(
            resolve_metrics_db(Some("/abs/x.sqlite"), "/h").as_deref(),
            Some("/abs/x.sqlite")
        );
    }

    #[test]
    fn open_metrics_db_creates_the_parent_directory() {
        let dir = fresh_dir("home");
        let mut cfg = Config::default();
        cfg.metrics_db = Some("state/deep/metrics.sqlite".into());
        let conn = open_metrics_db(&cfg, &dir.to_string_lossy()).unwrap();
        drop(conn);
        assert!(
            dir.join("state")
                .join("deep")
                .join("metrics.sqlite")
                .is_file()
        );
        assert!(open_metrics_db(&cfg, "").is_err(), "no HOME, no metrics");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shared_metrics_db_locks_with_a_dotfile_and_never_removes_one() {
        let dir = fresh_dir("db");
        let path = dir.join("room.sqlite");
        let p = path.to_string_lossy().to_string();
        let lock = format!("{p}.lock");
        let on = AlwaysOn {
            chars: 48_000,
            files: vec![("/x/CLAUDE.md".to_string(), 48_000)],
        };
        let on2 = AlwaysOn {
            chars: 50_000,
            files: vec![("/x/CLAUDE.md".to_string(), 50_000)],
        };
        let log = |conn: &Connection, a: &AlwaysOn| {
            log_metrics(
                conn,
                "proj",
                None,
                None,
                Some("s"),
                Some("p"),
                Some(1000),
                1,
                1,
                200_000,
                1.0,
                None,
                None,
                None,
                Some(a),
            )
        };
        let conn = open_metrics_at(&p, true).unwrap();
        log(&conn, &on).unwrap();

        // Someone else holds the dotfile lock (a slow writer, a reader, a
        // killed process): our write fails, the lock stays, the file is intact.
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::File::open(&lock)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        let t0 = std::time::Instant::now();
        assert!(
            log(&conn, &on2).is_err(),
            "a held lock means no row, not a removal"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "bounded by busy_timeout"
        );
        assert!(
            std::path::Path::new(&lock).is_dir(),
            "an hour-old lock is still not ours to remove"
        );
        assert!(
            open_metrics_at(&p, true).is_err(),
            "the schema check needs the lock, so a held lock fails the open"
        );
        assert!(
            std::path::Path::new(&lock).is_dir(),
            "and the failed open did not touch the lock either"
        );
        std::fs::remove_dir_all(&lock).unwrap();

        // The dotfile VFS is in use: the lock appears during a write transaction and goes after.
        conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO metrics (project) VALUES ('x');")
            .unwrap();
        assert!(
            std::path::Path::new(&lock).exists(),
            "dotfile lock held inside the transaction"
        );
        conn.execute_batch("COMMIT;").unwrap();
        assert!(
            !std::path::Path::new(&lock).exists(),
            "lock released at commit"
        );
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let ok: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");

        // always_on columns round-trip, and a change in them alone writes a new row.
        let count = || -> i64 {
            conn.query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
                .unwrap()
        };
        let before = count();
        for a in [&on, &on, &on2] {
            log(&conn, a).unwrap();
        }
        assert_eq!(
            count(),
            before + 2,
            "same input is one row; a changed always-on is another"
        );
        let (chars, files): (i64, String) = conn
            .query_row(
                "SELECT always_on_chars, always_on_files FROM metrics ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(chars, 50_000);
        assert!(files.contains("/x/CLAUDE.md"));
        drop(conn);
        assert!(
            !dir.join("room.sqlite-wal").exists(),
            "no WAL beside a shared database"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_copies_new_rows_once_and_skips_a_held_lock() {
        let dir = fresh_dir("flush");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cfg = Config::default(); // local file under home/.config/dbg, WAL
        let recorder = dir.join("house").join("recorder.sqlite");
        let rp = recorder.to_string_lossy().to_string();
        let on1 = AlwaysOn {
            chars: 100,
            files: vec![("/a/CLAUDE.md".to_string(), 100)],
        };
        let on2 = AlwaysOn {
            chars: 120,
            files: vec![("/a/CLAUDE.md".to_string(), 120)],
        };
        {
            let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
            let mut n = 0;
            let mut log = |prompt: &str, content: i64, on: &AlwaysOn| {
                n += 1;
                log_metrics(
                    &local,
                    "proj",
                    Some("main"),
                    Some("Fable"),
                    Some("s1"),
                    Some(prompt),
                    Some(content),
                    content,
                    n,
                    200_000,
                    10.0,
                    Some(0.5),
                    None,
                    None,
                    Some(on),
                )
                .unwrap();
            };
            log("p1", 84_000, &on1);
            log("p2", 88_000, &on1);
            log("p3", 90_000, &on2);
        }
        // First flush: three call rows, two always_on rows (100, then 120).
        let n = run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").unwrap();
        assert_eq!(n, 3);
        let rec = open_recorder(&rp).unwrap();
        let count = |sql: &str| -> i64 { rec.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM measures WHERE kind='statusline'"),
            3
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM measures WHERE kind='always_on'"),
            2
        );
        let (ts, room, value, unit, data): (String, String, f64, String, String) = rec
            .query_row(
                "SELECT ts, room, value, unit, data FROM measures WHERE kind='statusline' ORDER BY source_id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert!(
            ts.ends_with('Z') && ts.contains('.'),
            "ISO UTC with milliseconds: {ts}"
        );
        assert_eq!(room, "roomA");
        assert_eq!(value, 90_000.0);
        assert_eq!(unit, "tokens");
        let d: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(d["always_on_chars"], 120);
        assert_eq!(d["always_on_files"][0][1], 120);
        assert_eq!(d["model"], "Fable");

        // Second flush: nothing new.
        assert_eq!(
            run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").unwrap(),
            0
        );
        assert_eq!(count("SELECT COUNT(*) FROM measures"), 5);

        // A flush that lost its mark (killed before the update) writes nothing twice.
        {
            let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
            local
                .execute("UPDATE flush_state SET last_id = 0", [])
                .unwrap();
        }
        assert_eq!(
            run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").unwrap(),
            3
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM measures"),
            5,
            "INSERT OR IGNORE keeps it idempotent"
        );

        // New local rows after the mark flush alone; another room's rows do not collide.
        {
            let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
            log_metrics(
                &local,
                "proj",
                None,
                None,
                Some("s1"),
                Some("p4"),
                Some(95_000),
                95_000,
                9,
                200_000,
                10.0,
                None,
                None,
                None,
                Some(&on2),
            )
            .unwrap();
        }
        assert_eq!(
            run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").unwrap(),
            1
        );
        assert_eq!(count("SELECT COUNT(*) FROM measures"), 6);
        drop(rec);

        // Recorder lock held by someone else: skip, no removal, mark unchanged.
        let lock = format!("{rp}.lock");
        std::fs::create_dir_all(&lock).unwrap();
        {
            let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
            log_metrics(
                &local,
                "proj",
                None,
                None,
                Some("s1"),
                Some("p5"),
                Some(97_000),
                97_000,
                10,
                200_000,
                10.0,
                None,
                None,
                None,
                Some(&on2),
            )
            .unwrap();
        }
        let t0 = std::time::Instant::now();
        assert!(run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").is_err());
        assert!(t0.elapsed() < std::time::Duration::from_secs(10));
        assert!(
            std::path::Path::new(&lock).is_dir(),
            "the lock is not ours to remove"
        );
        {
            let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
            let last: i64 = local
                .query_row("SELECT last_id FROM flush_state", [], |r| r.get(0))
                .unwrap();
            assert_eq!(last, 4, "mark unchanged by a skipped flush");
        }
        std::fs::remove_dir_all(&lock).unwrap();
        assert_eq!(
            run_flush(&cfg, &home.to_string_lossy(), &rp, "roomA").unwrap(),
            1
        );
        assert!(!dir.join("house").join("recorder.sqlite-wal").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorder_ts_shapes() {
        assert_eq!(
            recorder_ts("2026-10-02T18:00:00"),
            "2026-10-02T18:00:00.000Z"
        );
        assert_eq!(
            recorder_ts("2026-10-02T18:00:00.123Z"),
            "2026-10-02T18:00:00.123Z"
        );
    }

    #[test]
    fn fmt_chars_unit() {
        assert_eq!(fmt_chars(512), "512ch");
        assert_eq!(fmt_chars(4_800), "4.8kch");
        assert_eq!(fmt_chars(47_735), "48kch");
    }

    #[test]
    fn truthy_parsing() {
        assert!(truthy("1"));
        assert!(truthy("true"));
        assert!(truthy("TRUE"));
        assert!(truthy("yes"));
        assert!(truthy("on"));
        assert!(!truthy("0"));
        assert!(!truthy("false"));
        assert!(!truthy("no"));
        assert!(!truthy(""));
    }
}
