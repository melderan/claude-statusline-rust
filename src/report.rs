use crate::*;
use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────────────
// --report: a plain-text usage report from the metrics file
// ─────────────────────────────────────────────────────────────────────
//
// `claude-statusline-rust --report [24h|7d|30d]` reads the rows of the local
// metrics file inside the window and prints one line per session, a totals
// line and, when the rows carry them, the rate-limit windows. It only reads
// (it never writes a row), prints no colour, and keeps to REPORT_WIDTH
// columns by shortening the project name, never a number.
//
// A session's cost is the `cost_usd` of its last row: the hook reports the
// session's running total, so summing rows would count it once per render.
// A session that began before the window is reported from its first row
// inside the window, but its cost is still the running total, so the totals
// line can include spend from before the window.

/// The widest line the report prints.
const REPORT_WIDTH: usize = 100;
/// The least a shortened column keeps.
const MIN_TEXT_WIDTH: usize = 8;
const USAGE: &str = "usage: claude-statusline-rust --report [24h|7d|30d]";

/// The windows `--report` accepts: the name and its length in seconds.
const WINDOWS: [(&str, i64); 3] = [("24h", 86_400), ("7d", 7 * 86_400), ("30d", 30 * 86_400)];

/// One metrics row inside the window, as the report reads it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReportRow {
    pub(crate) ts: i64,
    pub(crate) project: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) in_tokens: Option<i64>,
    pub(crate) out_tokens: Option<i64>,
    pub(crate) context_cap: Option<i64>,
    pub(crate) context_pct: Option<f64>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) rate_5h_pct: Option<f64>,
    pub(crate) rate_5h_resets: Option<i64>,
    pub(crate) rate_7d_pct: Option<f64>,
    pub(crate) rate_7d_resets: Option<i64>,
}

pub(crate) fn report_main(args: &[String]) {
    let after: Vec<&String> = args
        .iter()
        .skip_while(|a| *a != "--report")
        .skip(1)
        .collect();
    let window = match after.as_slice() {
        [] => Some(WINDOWS[0]),
        [w] => WINDOWS.iter().copied().find(|(name, _)| name == w),
        _ => None,
    };
    let Some((label, secs)) = window else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let cfg = Config::load();
    let home = home_dir().unwrap_or_default();
    let (path, _) = match metrics_db_path(&cfg, &home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("claude-statusline-rust --report: {e}");
            std::process::exit(1);
        }
    };
    // A fresh install has no file yet, and opening one would create it.
    if !std::path::Path::new(&path).exists() {
        out(&format!("No metrics file at {path}; nothing to report.\n"));
        return;
    }
    let now = now_epoch();
    // Not on a keystroke: wait as long as the flush does for a busy file.
    let rows = open_metrics_db_with(&cfg, &home, RECORDER_BUSY_TIMEOUT)
        .and_then(|conn| load_rows(&conn, now - secs));
    match rows {
        Ok(rows) => out(&render_report(&rows, label, &path, now)),
        // A busy file clears on its own and a report is never worth failing
        // a script over; any other failure is a real error.
        Err(e) if is_busy(e.as_ref()) => {
            eprintln!("claude-statusline-rust --report: skipped: {e}");
        }
        Err(e) => {
            eprintln!("claude-statusline-rust --report: {path}: {e}");
            std::process::exit(1);
        }
    }
}

/// Write to stdout without panicking on a closed pipe (`| head`).
fn out(text: &str) {
    let _ = std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes());
}

/// Rows with a timestamp at or after `since` (epoch seconds), oldest first.
/// A row whose `ts` SQLite cannot read is left out.
pub(crate) fn load_rows(conn: &Connection, since: i64) -> DbResult<Vec<ReportRow>> {
    let mut stmt = conn.prepare(
        "SELECT ts_epoch, project, model, session_id, prompt_id, in_tokens, out_tokens, context_cap,
                context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets
         FROM (SELECT CAST(strftime('%s', ts) AS INTEGER) AS ts_epoch, * FROM metrics)
         WHERE ts_epoch >= ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([since], |r| {
        Ok(ReportRow {
            ts: r.get(0)?,
            project: r.get(1)?,
            model: r.get(2)?,
            session_id: r.get(3)?,
            prompt_id: r.get(4)?,
            in_tokens: r.get(5)?,
            out_tokens: r.get(6)?,
            context_cap: r.get(7)?,
            context_pct: r.get(8)?,
            cost_usd: r.get(9)?,
            rate_5h_pct: r.get(10)?,
            rate_5h_resets: r.get(11)?,
            rate_7d_pct: r.get(12)?,
            rate_7d_resets: r.get(13)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// One session's rows folded into what the report prints.
#[derive(Debug)]
struct Session {
    first_ts: i64,
    last_ts: i64,
    project: Option<String>,
    model: Option<String>,
    prompts: HashSet<String>,
    rows_without_prompt: usize,
    /// (context_pct, input tokens of that row, window size).
    peak: Option<(f64, Option<i64>, Option<i64>)>,
    cost: Option<f64>,
    last_in: Option<i64>,
    last_out: Option<i64>,
}

impl Session {
    fn turns(&self) -> usize {
        self.prompts.len() + self.rows_without_prompt
    }
}

/// Fold rows (oldest first) into sessions, the one that started last at the
/// end. Rows without a session id are one session.
fn sessions(rows: &[ReportRow]) -> Vec<Session> {
    let mut index: HashMap<Option<&str>, usize> = HashMap::new();
    let mut out: Vec<Session> = Vec::new();
    for r in rows {
        let at = *index.entry(r.session_id.as_deref()).or_insert_with(|| {
            out.push(Session {
                first_ts: r.ts,
                last_ts: r.ts,
                project: None,
                model: None,
                prompts: HashSet::new(),
                rows_without_prompt: 0,
                peak: None,
                cost: None,
                last_in: None,
                last_out: None,
            });
            out.len() - 1
        });
        let s = &mut out[at];
        s.last_ts = r.ts;
        if r.project.is_some() {
            s.project = r.project.clone();
        }
        if r.model.is_some() {
            s.model = r.model.clone();
        }
        match &r.prompt_id {
            Some(p) => {
                s.prompts.insert(p.clone());
            }
            None => s.rows_without_prompt += 1,
        }
        if let Some(pct) = r.context_pct
            && s.peak.is_none_or(|(best, _, _)| pct >= best)
        {
            s.peak = Some((pct, r.in_tokens, r.context_cap));
        }
        if r.cost_usd.is_some() {
            s.cost = r.cost_usd;
        }
        s.last_in = r.in_tokens;
        s.last_out = r.out_tokens;
    }
    // Stable: sessions that start in the same second keep their row order.
    out.sort_by_key(|s| s.first_ts);
    out
}

/// The report for `rows` (the window's rows, oldest first) at time `now`.
pub(crate) fn render_report(rows: &[ReportRow], label: &str, path: &str, now: i64) -> String {
    let sess = sessions(rows);
    if sess.is_empty() {
        return format!("No sessions in the last {label} in {path}.\n");
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Usage, last {label} (times UTC)"));

    const HEAD: [&str; 9] = [
        "start",
        "end",
        "dur",
        "project",
        "model",
        "turns",
        "peak ctx",
        "cost",
        "last in/out",
    ];
    // Left-aligned text columns; the rest are numbers, right-aligned.
    const LEFT: [bool; 9] = [true, true, false, true, true, false, false, false, false];
    let mut table: Vec<[String; 9]> = Vec::new();
    for s in &sess {
        table.push([
            fmt_time(s.first_ts, now),
            fmt_time(s.last_ts, now),
            fmt_duration(s.last_ts - s.first_ts),
            project_name(s.project.as_deref()),
            s.model.clone().unwrap_or_else(|| "-".to_string()),
            s.turns().to_string(),
            fmt_peak(s.peak),
            s.cost.map_or("-".to_string(), |c| format!("${c:.2}")),
            format!("{}/{}", fmt_tokens(s.last_in), fmt_tokens(s.last_out)),
        ]);
    }
    let mut widths: [usize; 9] = std::array::from_fn(|i| {
        table
            .iter()
            .map(|row| row[i].chars().count())
            .chain([HEAD[i].chars().count()])
            .max()
            .unwrap_or(0)
    });
    // Over the budget: the project name gives way first, then the model.
    // Numbers and times never do.
    let gaps = 2 * (widths.len() - 1);
    let mut over = (widths.iter().sum::<usize>() + gaps).saturating_sub(REPORT_WIDTH);
    for col in [3, 4] {
        let give = over.min(widths[col].saturating_sub(MIN_TEXT_WIDTH));
        widths[col] -= give;
        over -= give;
    }
    let row_line = |cells: &[String; 9]| -> String {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let cell = shorten(cell, widths[i]);
                if LEFT[i] {
                    format!("{cell:<w$}", w = widths[i])
                } else {
                    format!("{cell:>w$}", w = widths[i])
                }
            })
            .collect();
        parts.join("  ").trim_end().to_string()
    };
    lines.push(row_line(&HEAD.map(String::from)));
    for row in &table {
        lines.push(row_line(row));
    }

    let turns: usize = sess.iter().map(Session::turns).sum();
    let costs: Vec<f64> = sess.iter().filter_map(|s| s.cost).collect();
    let cost = if costs.is_empty() {
        "-".to_string()
    } else {
        format!("${:.2}", costs.iter().sum::<f64>())
    };
    let first = sess.iter().map(|s| s.first_ts).min().unwrap_or(0);
    let last = sess.iter().map(|s| s.last_ts).max().unwrap_or(0);
    lines.push(format!(
        "Total: {} {}, {} {}, {cost}, {} to {} ({})",
        sess.len(),
        plural(sess.len(), "session"),
        turns,
        plural(turns, "turn"),
        fmt_time(first, now),
        fmt_time(last, now),
        fmt_duration(last - first),
    ));

    if let Some(line) = rate_line(rows, now) {
        lines.push(line);
    }
    lines.push(String::new());
    lines.join("\n")
}

/// The rate-limit line: the latest 5h and 7d readings and the 5h peak. None
/// when no row in the window carries a 5h percentage.
fn rate_line(rows: &[ReportRow], now: i64) -> Option<String> {
    let latest_5h = rows.iter().rev().find(|r| r.rate_5h_pct.is_some())?;
    let peak = rows
        .iter()
        .filter_map(|r| r.rate_5h_pct)
        .fold(f64::MIN, f64::max);
    let reading = |name: &str, pct: Option<f64>, resets: Option<i64>| -> String {
        match (pct, resets) {
            (Some(p), Some(t)) => format!("{name} {p:.0}% (resets {})", fmt_time(t, now)),
            (Some(p), None) => format!("{name} {p:.0}%"),
            (None, _) => format!("{name} -"),
        }
    };
    let latest_7d = rows.iter().rev().find(|r| r.rate_7d_pct.is_some());
    Some(format!(
        "Rate limits: {}, {}; 5h peak in window {peak:.0}%",
        reading("5h", latest_5h.rate_5h_pct, latest_5h.rate_5h_resets),
        reading(
            "7d",
            latest_7d.and_then(|r| r.rate_7d_pct),
            latest_7d.and_then(|r| r.rate_7d_resets)
        ),
    ))
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// The last path component of a project directory; `-` when there is none.
fn project_name(project: Option<&str>) -> String {
    project
        .map(|p| p.trim_end_matches('/'))
        .and_then(|p| p.rsplit('/').next())
        .filter(|p| !p.is_empty())
        .unwrap_or("-")
        .to_string()
}

/// `s` cut to `width` characters, ending in `…` when it was cut.
fn shorten(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// `24% 237k/1000k`: the highest context percentage, the input tokens of that
/// row and the window size.
fn fmt_peak(peak: Option<(f64, Option<i64>, Option<i64>)>) -> String {
    let Some((pct, used, cap)) = peak else {
        return "-".to_string();
    };
    let used = used.or_else(|| cap.map(|c| (pct / 100.0 * c as f64).round() as i64));
    match (used, cap) {
        (Some(u), Some(c)) if c > 0 => format!("{pct:.0}% {}/{}", fmt_k(u), fmt_k(c)),
        _ => format!("{pct:.0}%"),
    }
}

/// Thousands, rounded, with a `k`: `237k`.
fn fmt_k(n: i64) -> String {
    format!("{}k", (n + 500).div_euclid(1000))
}

/// A token count: exact under a thousand, `237k` above; `-` when unknown.
fn fmt_tokens(n: Option<i64>) -> String {
    match n {
        None => "-".to_string(),
        Some(n) if n < 1000 => n.to_string(),
        Some(n) => fmt_k(n),
    }
}

/// `45m`, `2h05m`, `1d03h`; a span under a minute is `0m`.
fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d{h:02}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}

/// UTC time of `t`: `HH:MM` within 24 hours of `now`, else `MM-DD HH:MM`.
fn fmt_time(t: i64, now: i64) -> String {
    let (_, month, day) = civil_from_days(t.div_euclid(86_400));
    let secs = t.rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    if (t - now).abs() < 86_400 {
        hm
    } else {
        format!("{month:02}-{day:02} {hm}")
    }
}

/// (year, month, day) of a day count since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
