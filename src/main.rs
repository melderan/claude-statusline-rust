use rusqlite::Connection;
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Read;

mod config;
mod extras;
mod flush;
mod git;
mod input;
mod lines;
mod memory;
mod metrics;
mod render;
#[cfg(test)]
mod tests;
mod voice;

use config::*;
use extras::*;
use flush::*;
use git::*;
use input::*;
use lines::*;
use memory::*;
use metrics::*;
use render::*;
use voice::*;

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

    let width = detect_width();
    let mode = pick_mode(width);
    let bar_width = if mode == Mode::Compact { 8 } else { 16 };

    let (project_dir, current_dir) = dirs(&data);

    // Memory sizes (only if project_dir known).
    let memory = if project_dir.is_empty() {
        (0, 0)
    } else {
        memory_bytes(project_dir)
    };
    // Always-on characters: CLAUDE.md chain, imports and the memory index.
    let home = home_dir().unwrap_or_default();
    let on = always_on(project_dir, &home);

    let num = ctx_numbers(&data);
    let in_tok = num.in_tok;
    let out_tok = num.out_tok;
    let cap = num.cap;
    let raw_pct = num.raw_pct;
    let cu_ref = data
        .context_window
        .as_ref()
        .and_then(|c| c.current_usage.as_ref());

    // Metrics first, so the residue line can read the row this update wrote.
    // Best-effort: any SQLite failure leaves the display untouched.
    let content = content_tokens(cu_ref);
    let branch = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref());
    let opened = open_metrics_db(&cfg, &home);
    if let Err(e) = &opened
        && cfg.metrics_db.is_some()
    {
        // The shared file is locked or unreachable: this row is skipped,
        // never forced. One line, so a hook or a log shows it.
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

    let git = if !current_dir.is_empty() {
        git_info(current_dir)
    } else if !project_dir.is_empty() {
        git_info(project_dir)
    } else {
        None
    };

    let voice = if cfg.voice {
        voice_session(
            std::env::var("CLAUDE_TTS_SESSION").ok().as_deref(),
            project_dir,
        )
        .and_then(|session| read_voice_card(&voice_card_path(&home, &session)))
        .and_then(|card| voice_segment(&card))
    } else {
        None
    };

    let env = Env {
        mode,
        bar_width,
        memory,
        on_chars: on.chars,
        residue,
        git,
        voice,
        now: now_epoch(),
    };
    let lines = build_lines(&data, &cfg, &env);
    print!("{}", assemble(&lines, &cfg, width));
}
