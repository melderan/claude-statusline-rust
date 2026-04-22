use rusqlite::Connection;
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Read;

#[derive(Deserialize)]
struct Input {
    model: Option<Model>,
    workspace: Option<Workspace>,
    context_window: Option<ContextWindow>,
    cost: Option<Cost>,
    vim: Option<Vim>,
    agent: Option<Agent>,
    rate_limits: Option<RateLimits>,
    subagents: Option<Subagents>,
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
    git_worktree: Option<String>,
}

#[derive(Deserialize)]
struct ContextWindow {
    total_input_tokens: Option<i64>,
    total_output_tokens: Option<i64>,
    context_window_size: Option<i64>,
    used_percentage: Option<f64>,
}

#[derive(Deserialize)]
struct Cost {
    total_cost_usd: Option<f64>,
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

/// Format a unix timestamp as "in Xh Ym @ Mon Apr 14 18:30 UTC"
fn fmt_reset(resets_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let remaining = resets_at - now;

    // Time remaining
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

    // Absolute UTC time from epoch
    // Manual UTC date formatting (no chrono dependency needed)
    let ts = resets_at;
    let secs_per_day: i64 = 86400;
    let days_since_epoch = ts / secs_per_day;
    let time_of_day = ts % secs_per_day;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;

    // Calculate year/month/day from days since 1970-01-01
    // Using a civil-from-days algorithm
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

    let weekday = ((days_since_epoch % 7) + 4) % 7; // 0=Sun
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

fn main() {
    let mut buf = String::with_capacity(4096);
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return;
    }

    let data: Input = match serde_json::from_str(&buf) {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut out = String::with_capacity(128);

    // Project dir: prefer $PROJECT_ROOT, fall back to workspace.project_dir
    let project_env = std::env::var("PROJECT_ROOT").ok();
    // Git branch from workspace
    let branch = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref());

    // Full project path + branch
    let full_path = project_env
        .as_deref()
        .or_else(|| {
            data.workspace
                .as_ref()
                .and_then(|w| w.project_dir.as_deref())
        })
        .unwrap_or("");
    if !full_path.is_empty() {
        out.push_str(full_path);
        if let Some(b) = branch {
            out.push(':');
            out.push_str(b);
        }
        out.push_str(" | ");
    }

    // Model (strip "Claude " prefix)
    if let Some(name) = data.model.as_ref().and_then(|m| m.display_name.as_deref()) {
        let short = name.strip_prefix("Claude ").unwrap_or(name);
        out.push_str(short);
        out.push_str(" | ");
    }

    // Context usage -- current window size + cumulative session totals.
    // current_ctx_tok is derived from used_percentage so it tracks the live
    // window (and drops after compaction), while in/out remain session lifetime.
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
    let cap = data
        .context_window
        .as_ref()
        .and_then(|c| c.context_window_size)
        .unwrap_or(0);
    let pct = data
        .context_window
        .as_ref()
        .and_then(|c| c.used_percentage)
        .unwrap_or(0.0);
    let current_ctx_tok = if cap > 0 && pct > 0.0 {
        ((pct / 100.0) * cap as f64) as i64
    } else {
        in_tok + out_tok
    };
    if cap > 0 {
        let _ = write!(
            out,
            "ctx:{}/{} ({:.0}%) | session in:{} out:{}",
            current_ctx_tok, cap, pct, in_tok, out_tok
        );
    } else {
        let _ = write!(out, "in:{} out:{}", in_tok, out_tok);
    }

    // Cost (session lifetime)
    if let Some(usd) = data.cost.as_ref().and_then(|c| c.total_cost_usd)
        && usd > 0.001
    {
        let _ = write!(out, " | ${:.2}", usd);
    }

    // Line 2: 5h rate limit
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

    // Line 3: 7d rate limit
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

    // Line 4: misc (agents, vim, agent name)
    let mut misc_parts: Vec<String> = Vec::new();

    let sub_count = data.subagents.as_ref().and_then(|s| s.count).unwrap_or(0);
    if sub_count > 0 {
        misc_parts.push(format!("agents:{}", sub_count));
    }

    if let Some(mode) = data.vim.as_ref().and_then(|v| v.mode.as_deref()) {
        misc_parts.push(format!("[{}]", mode));
    }

    if let Some(name) = data.agent.as_ref().and_then(|a| a.name.as_deref()) {
        misc_parts.push(format!("{{{}}}", name));
    }

    if !misc_parts.is_empty() {
        let _ = write!(out, "\n{}", misc_parts.join(" | "));
    }

    print!("{out}");

    // Log metrics to SQLite (best-effort, never block display)
    let _ = log_metrics(
        full_path,
        branch,
        data.model.as_ref().and_then(|m| m.display_name.as_deref()),
        in_tok,
        out_tok,
        cap,
        pct,
        data.cost.as_ref().and_then(|c| c.total_cost_usd),
        data.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref()),
        data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()),
    );
}

#[allow(clippy::too_many_arguments)]
fn log_metrics(
    project: &str,
    branch: Option<&str>,
    model: Option<&str>,
    in_tokens: i64,
    out_tokens: i64,
    context_cap: i64,
    context_pct: f64,
    cost_usd: Option<f64>,
    five_hour: Option<&RateWindow>,
    seven_day: Option<&RateWindow>,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let db_path = format!("{}/.config/dbg/statusline-metrics.db", home);

    // Ensure parent dir exists
    let _ = std::fs::create_dir_all(format!("{}/.config/dbg", home));

    let conn = Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

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

    // Deduplicate: skip if the last row has identical token counts and rate %
    let last: Option<(i64, i64, f64, f64)> = conn
        .query_row(
            "SELECT in_tokens, out_tokens, COALESCE(rate_5h_pct, -1), COALESCE(rate_7d_pct, -1) FROM metrics ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .ok();

    let cur_5h = five_hour.and_then(|w| w.used_percentage).unwrap_or(-1.0);
    let cur_7d = seven_day.and_then(|w| w.used_percentage).unwrap_or(-1.0);

    if let Some((last_in, last_out, last_5h, last_7d)) = last
        && last_in == in_tokens
        && last_out == out_tokens
        && (last_5h - cur_5h).abs() < 0.01
        && (last_7d - cur_7d).abs() < 0.01
    {
        return Ok(()); // Nothing changed, skip
    }

    conn.execute(
        "INSERT INTO metrics (project, branch, model, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            project,
            branch,
            model,
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
