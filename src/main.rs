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

#[derive(Deserialize, Debug, Clone, Copy)]
struct Config {
    #[serde(default)]
    bar: bool,
    #[serde(default)]
    glyphs: bool,
    #[serde(default = "default_true")]
    color: bool,
    /// Residue line: how many recent user turns to show with their context
    /// cost (`res: +84k +3.1k ...`). 0 (the default) hides the line; values
    /// above RESIDUE_MAX are clamped.
    #[serde(default)]
    residue: u8,
}

/// Longest residue window; past ten turns the line stops being readable.
const RESIDUE_MAX: u8 = 10;

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bar: false,
            glyphs: false,
            color: true,
            residue: 0,
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
        if let Ok(v) = std::env::var("CSR_RESIDUE")
            && let Ok(n) = v.trim().parse::<u8>()
        {
            cfg.residue = n;
        }
        cfg.residue = cfg.residue.min(RESIDUE_MAX);
        cfg
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

fn fmt_bytes(b: u64) -> String {
    if b == 0 {
        "-".to_string()
    } else if b < 1024 {
        format!("{}B", b)
    } else if b < 1024 * 1024 {
        format!("{:.1}k", b as f64 / 1024.0)
    } else {
        format!("{:.1}M", b as f64 / (1024.0 * 1024.0))
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
        format!("{:+}", d)
    } else if a < 10_000 {
        format!("{:+.1}k", d as f64 / 1000.0)
    } else {
        format!("{:+}k", (d as f64 / 1000.0).round() as i64)
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
fn memory_bytes(project_dir: &str) -> (u64, u64) {
    let home = match home_dir() {
        Some(h) => h,
        None => return (0, 0),
    };
    let slug = path_to_memory_slug(project_dir);
    let dir = format!("{}/.claude/projects/{}/memory", home, slug);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return (0, 0),
    };
    let mut index = 0u64;
    let mut other = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.ends_with(".md") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if name_str == "MEMORY.md" {
            index += size;
        } else {
            other += size;
        }
    }
    (index, other)
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
    let residue: Vec<i64> = open_metrics_db()
        .ok()
        .map(|conn| {
            let _ = log_metrics(
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
            );
            match (cfg.residue, data.session_id.as_deref()) {
                (n, Some(sid)) if n > 0 => {
                    residue_deltas(&conn, sid, n as usize).unwrap_or_default()
                }
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

fn open_metrics_db() -> DbResult<Connection> {
    let home = std::env::var("HOME")?;
    let db_path = format!("{}/.config/dbg/statusline-metrics.db", home);
    let _ = std::fs::create_dir_all(format!("{}/.config/dbg", home));
    let conn = Connection::open(&db_path)?;
    // Another status line (a second pane) may hold the write lock; wait a
    // little, never long enough to be seen.
    conn.busy_timeout(std::time::Duration::from_millis(50))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
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
    ] {
        if !have.iter().any(|h| h == name) {
            conn.execute_batch(&format!("ALTER TABLE metrics ADD COLUMN {name} {ty};"))?;
        }
    }
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
) -> DbResult<()> {
    let last: Option<(i64, i64, f64, f64, i64)> = conn
        .query_row(
            "SELECT in_tokens, out_tokens, COALESCE(rate_5h_pct, -1), COALESCE(rate_7d_pct, -1), COALESCE(content, -1) FROM metrics ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .ok();

    let cur_5h = five_hour.and_then(|w| w.used_percentage).unwrap_or(-1.0);
    let cur_7d = seven_day.and_then(|w| w.used_percentage).unwrap_or(-1.0);
    let cur_content = content.unwrap_or(-1);

    if let Some((last_in, last_out, last_5h, last_7d, last_content)) = last
        && last_in == in_tokens
        && last_out == out_tokens
        && (last_5h - cur_5h).abs() < 0.01
        && (last_7d - cur_7d).abs() < 0.01
        && last_content == cur_content
    {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO metrics (project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
        ],
    )?;

    Ok(())
}

/// Context deltas of the last `n` user turns of `session_id`, oldest first.
/// Rows without a prompt_id (older Claude Code) each count as a turn.
fn residue_deltas(conn: &Connection, session_id: &str, n: usize) -> DbResult<Vec<i64>> {
    // A turn produces one row per API response; 40 rows per turn is a loose
    // upper bound for a turn full of tool calls.
    let limit = (n as i64 + 1) * 40;
    let mut stmt = conn.prepare(
        "SELECT id, prompt_id, content FROM metrics
         WHERE session_id = ?1 AND content IS NOT NULL
         ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, limit], |row| {
        let id: i64 = row.get(0)?;
        let prompt: Option<String> = row.get(1)?;
        let content: i64 = row.get(2)?;
        Ok((prompt.unwrap_or_else(|| format!("row-{id}")), content))
    })?;
    let mut newest_first = Vec::new();
    for r in rows {
        newest_first.push(r?);
    }
    Ok(turn_deltas(&newest_first, n))
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
        assert_eq!(fmt_bytes(1536), "1.5k");
        assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0M");
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
        assert_eq!(cfg.residue, 0, "residue line should be opt-in");
    }

    #[test]
    fn fmt_delta_shapes() {
        assert_eq!(fmt_delta(0), "+0");
        assert_eq!(fmt_delta(512), "+512");
        assert_eq!(fmt_delta(-900), "-900");
        assert_eq!(fmt_delta(3_140), "+3.1k");
        assert_eq!(fmt_delta(9_999), "+10.0k");
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
