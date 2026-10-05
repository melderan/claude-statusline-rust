use crate::*;

// System prompt + tools + MCP tokens not surfaced to the hook JSON.
// See github.com/anthropics/claude-code/issues/13783. PAI's documented baseline.
pub(crate) const CONTEXT_BASELINE: i64 = 22_600;

// ─────────────────────────────────────────────────────────────────────
// Display mode
// ─────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Compact,
    Standard,
}

pub(crate) fn pick_mode(cols: usize) -> Mode {
    if cols < 60 {
        Mode::Compact
    } else {
        Mode::Standard
    }
}

pub(crate) fn detect_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80)
}

// ─────────────────────────────────────────────────────────────────────
// Formatters (pure, testable)
// ─────────────────────────────────────────────────────────────────────

pub(crate) const RESET: &str = "\x1b[0m";
pub(crate) const DIM: &str = "\x1b[38;2;100;116;139m";
pub(crate) const EMPTY_BAR: &str = "\x1b[38;2;75;82;95m";

pub(crate) fn fmt_duration_ms(ms: i64) -> String {
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
pub(crate) fn fmt_bytes(b: u64) -> String {
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
pub(crate) fn fmt_chars(n: u64) -> String {
    if n < 1000 {
        format!("{n}ch")
    } else if n < 10_000 {
        format!("{:.1}kch", n as f64 / 1000.0)
    } else {
        format!("{}kch", (n as f64 / 1000.0).round() as u64)
    }
}

/// Returns (label, 24-bit ANSI color).
pub(crate) fn fmt_age_secs(secs: i64) -> (String, &'static str) {
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
pub(crate) fn fmt_reset(resets_at: i64) -> String {
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
pub(crate) const GRADIENT_STOPS: [(i32, i32, i32); 4] = [
    (74, 222, 128), // 0%   green
    (250, 204, 21), // 33%  yellow
    (251, 146, 60), // 66%  orange
    (239, 68, 68),  // 100% red
];

pub(crate) fn bucket_color(pos: usize, max: usize) -> String {
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

pub(crate) fn context_bar(width: usize, pct: i32) -> String {
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
pub(crate) fn context_bar_plain(width: usize, pct: i32) -> String {
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
pub(crate) fn content_tokens(cu: Option<&CurrentUsage>) -> Option<i64> {
    let cu = cu?;
    let cache_read = cu.cache_read_input_tokens.unwrap_or(0);
    let cache_creation = cu.cache_creation_input_tokens.unwrap_or(0);
    let input = cu.input_tokens.unwrap_or(0);
    let output = cu.output_tokens.unwrap_or(0);
    let content = cache_read + cache_creation + input + output;
    if content <= 0 { None } else { Some(content) }
}

/// Baseline-corrected context percent (matches /context more closely than raw used_percentage).
pub(crate) fn computed_ctx_pct(cu: Option<&CurrentUsage>, cap: i64) -> Option<f64> {
    if cap <= 0 {
        return None;
    }
    let used = content_tokens(cu)? + CONTEXT_BASELINE;
    Some((used as f64) * 100.0 / (cap as f64))
}

/// Signed token delta for the residue line: `+512`, `+3.1k`, `+84k`, `-120k`.
pub(crate) fn fmt_delta(d: i64) -> String {
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
pub(crate) fn turn_deltas(rows_newest_first: &[(String, i64)], n: usize) -> Vec<i64> {
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
// Render
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn pct_color(pct: f64) -> &'static str {
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
pub(crate) fn c<'a>(cfg: &Config, color_esc: &'a str) -> &'a str {
    if cfg.color { color_esc } else { "" }
}

pub(crate) fn reset(cfg: &Config) -> &'static str {
    if cfg.color { RESET } else { "" }
}
