use rusqlite::Connection;
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Read;

mod config;
mod extras;
mod flush;
mod git;
mod input;
mod memory;
mod metrics;
mod render;
#[cfg(test)]
mod tests;

use config::*;
use extras::*;
use flush::*;
use git::*;
use input::*;
use memory::*;
use metrics::*;
use render::*;

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
        if cfg.extras && data.exceeds_200k_tokens == Some(true) {
            let _ = write!(out, " {}200k+{}", c(&cfg, AMBER), rst);
        }
        if mode == Mode::Standard {
            let _ = write!(
                out,
                " {}|{} last in:{} out:{}",
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
    if cfg.cache
        && let Some(pc) = data.prompt_cache.as_ref()
        && let Some(seg) = cache_segment(pc, now_epoch(), &cfg)
    {
        let _ = write!(out, " {}|{} {}", c(&cfg, DIM), rst, seg);
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
        if cfg.extras
            && let Some(tag) = data.pr.as_ref().and_then(pr_tag)
        {
            let _ = write!(out, " {}|{} {}", c(&cfg, DIM), rst, tag);
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
        if cfg.extras
            && let Some(ts) = five.resets_at
            && let Some(p) = pace(pct, ts, FIVE_HOURS, now_epoch())
        {
            out.push_str(&fmt_pace(p, &cfg));
        }
    }

    if let Some(seven) = data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()) {
        let pct = seven.used_percentage.unwrap_or(0.0);
        if pct > 0.0 {
            let reset = seven
                .resets_at
                .map(|ts| format!(", resets {}", fmt_reset(ts)))
                .unwrap_or_default();
            let _ = write!(out, "\n7d window: {:.0}% used{}", pct, reset);
            if cfg.extras
                && let Some(ts) = seven.resets_at
                && let Some(p) = pace(pct, ts, SEVEN_DAYS, now_epoch())
            {
                out.push_str(&fmt_pace(p, &cfg));
            }
        }
    }

    // ── Misc line ──
    let mut misc: Vec<String> = Vec::new();
    if cfg.extras {
        misc.extend(mode_tags(&data));
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
