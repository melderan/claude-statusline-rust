use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Lines: each row of the status line is built as its own String, then
// assembled either one per row (the default) or into a single row.
// Building takes everything that touches the machine (git, the memory
// directory, the metrics database, the voice card, the clock) as an `Env`
// the caller fills in, so the whole layout tests without a filesystem.
// ─────────────────────────────────────────────────────────────────────

/// How the rows are laid out. `multi` is one row per kind of information;
/// `one` joins them into a single row for terminals with room for one.
#[derive(Deserialize, Copy, Clone, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LineMode {
    #[default]
    Multi,
    One,
}

impl LineMode {
    /// `one` or `multi`, any case, surrounding space ignored; else None.
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "one" => Some(Self::One),
            "multi" => Some(Self::Multi),
            _ => None,
        }
    }
}

/// The rows, in render order. An empty String means the row is absent.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct Lines {
    /// Project, model, version, duration, memory sizes, always-on size.
    pub(crate) project: String,
    pub(crate) ctx: String,
    pub(crate) residue: String,
    pub(crate) git: String,
    pub(crate) five_hour: String,
    pub(crate) seven_day: String,
    pub(crate) misc: String,
}

/// Everything build_lines() needs that is not in the hook payload or the
/// config. Gathered once by main(); a test fills it with fixed values.
pub(crate) struct Env {
    pub(crate) mode: Mode,
    pub(crate) bar_width: usize,
    /// Bytes of the project's memory index and of its other memory files.
    pub(crate) memory: (u64, u64),
    /// Characters always loaded into the context (CLAUDE.md chain, imports).
    pub(crate) on_chars: u64,
    /// Per-turn context deltas for the residue row, oldest first.
    pub(crate) residue: Vec<i64>,
    pub(crate) git: Option<GitInfo>,
    /// The voice segment, already read from the voice card.
    pub(crate) voice: Option<String>,
    pub(crate) now: i64,
}

/// The context window numbers the ctx row and the metrics log both use.
pub(crate) struct CtxNumbers {
    pub(crate) cap: i64,
    pub(crate) raw_pct: f64,
    /// Baseline-corrected percent when usage is known, else `raw_pct`.
    pub(crate) computed_pct: f64,
    pub(crate) in_tok: i64,
    pub(crate) out_tok: i64,
}

pub(crate) fn ctx_numbers(data: &Input) -> CtxNumbers {
    let cw = data.context_window.as_ref();
    let cap = cw.and_then(|c| c.context_window_size).unwrap_or(0);
    let raw_pct = cw.and_then(|c| c.used_percentage).unwrap_or(0.0);
    let cu_ref = cw.and_then(|c| c.current_usage.as_ref());
    CtxNumbers {
        cap,
        raw_pct,
        computed_pct: computed_ctx_pct(cu_ref, cap).unwrap_or(raw_pct),
        in_tok: cw.and_then(|c| c.total_input_tokens).unwrap_or(0),
        out_tok: cw.and_then(|c| c.total_output_tokens).unwrap_or(0),
    }
}

/// (project_dir, current_dir) from the payload; empty when absent.
pub(crate) fn dirs(data: &Input) -> (&str, &str) {
    let ws = data.workspace.as_ref();
    (
        ws.and_then(|w| w.project_dir.as_deref()).unwrap_or(""),
        ws.and_then(|w| w.current_dir.as_deref()).unwrap_or(""),
    )
}

/// The dim `|` with a space either side, as the segments inside a row use it.
fn dim_bar(cfg: &Config) -> String {
    format!(" {}|{} ", c(cfg, DIM), reset(cfg))
}

pub(crate) fn build_lines(data: &Input, cfg: &Config, env: &Env) -> Lines {
    let rst = reset(cfg);
    let (project_dir, current_dir) = dirs(data);
    let num = ctx_numbers(data);

    let mut lines = Lines {
        project: project_line(data, cfg, env, project_dir, current_dir),
        ..Lines::default()
    };

    // Cost and cache ride on the end of the ctx row. With no context window
    // size there is no ctx row, and they land on the project row instead, as
    // they always have.
    let mut tail = String::new();
    if let Some(usd) = data.cost.as_ref().and_then(|c| c.total_cost_usd)
        && usd > 0.001
    {
        let _ = write!(tail, " {}|{} ${:.2}", c(cfg, DIM), rst, usd);
    }
    if cfg.cache
        && let Some(pc) = data.prompt_cache.as_ref()
        && let Some(seg) = cache_segment(pc, env.now, cfg)
    {
        let _ = write!(tail, " {}|{} {}", c(cfg, DIM), rst, seg);
    }
    if num.cap > 0 {
        lines.ctx = ctx_line(data, cfg, env, &num);
        lines.ctx.push_str(&tail);
    } else {
        lines.project.push_str(&tail);
    }

    if !env.residue.is_empty() {
        lines.residue.push_str("res:");
        for d in &env.residue {
            lines.residue.push(' ');
            lines.residue.push_str(&fmt_delta(*d));
        }
    }

    lines.git = git_line(data, cfg, env);
    lines.five_hour = five_hour_line(data, cfg, env.now);
    lines.seven_day = seven_day_line(data, cfg, env.now);
    lines.misc = misc_line(data, cfg, env);
    lines
}

/// project [→ cur] | model | CC:version | duration | memory | always-on
fn project_line(
    data: &Input,
    cfg: &Config,
    env: &Env,
    project_dir: &str,
    current_dir: &str,
) -> String {
    let rst = reset(cfg);
    let mut out = String::new();
    let rel_cur = if !project_dir.is_empty() && !current_dir.is_empty() {
        relative_current(project_dir, current_dir)
    } else {
        None
    };
    if !project_dir.is_empty() {
        out.push_str(&tilde(project_dir));
        if let Some(suffix) = &rel_cur {
            if cfg.glyphs {
                let _ = write!(out, " {}\u{2192}{} {}", c(cfg, DIM), rst, suffix);
            } else {
                let _ = write!(out, " {}|{} cd:{}", c(cfg, DIM), rst, suffix);
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
        let _ = write!(out, " {}|{} CC:{}", c(cfg, DIM), rst, cc);
    }

    if let Some(ms) = data.cost.as_ref().and_then(|c| c.total_duration_ms) {
        let label = if cfg.glyphs { "\u{23F1}" } else { "dur:" };
        let _ = write!(
            out,
            " {}|{} {}{}",
            c(cfg, DIM),
            rst,
            label,
            fmt_duration_ms(ms)
        );
    }

    let (idx, other) = env.memory;
    if !project_dir.is_empty() && (idx > 0 || other > 0) {
        let _ = write!(
            out,
            " {}|{} mem:{}+{}",
            c(cfg, DIM),
            rst,
            fmt_bytes(idx),
            fmt_bytes(other)
        );
    }

    if env.on_chars > 0 {
        let _ = write!(
            out,
            " {}|{} on:{}",
            c(cfg, DIM),
            rst,
            fmt_chars(env.on_chars)
        );
    }
    out
}

/// ctx [bar] pct (used/cap) [200k+] [| last in/out].
/// Empty when the window size is unknown.
fn ctx_line(data: &Input, cfg: &Config, env: &Env, num: &CtxNumbers) -> String {
    let rst = reset(cfg);
    let cap = num.cap;
    if cap <= 0 {
        return String::new();
    }
    let current_tok = ((num.computed_pct / 100.0) * cap as f64) as i64;
    let pct_int = num.computed_pct.round() as i32;
    let mut out = String::from("ctx ");
    if cfg.bar {
        // Bar respects cfg.color through its ANSI codes; if color off, emit
        // plain ASCII hashes/dots instead.
        if cfg.color {
            out.push_str(&context_bar(env.bar_width, pct_int));
        } else {
            out.push_str(&context_bar_plain(env.bar_width, pct_int));
        }
        out.push(' ');
    }
    let _ = write!(
        out,
        "{}{}%{} ({}k/{}k)",
        c(cfg, pct_color(num.computed_pct)),
        pct_int,
        rst,
        current_tok / 1000,
        cap / 1000
    );
    if cfg.extras && data.exceeds_200k_tokens == Some(true) {
        let _ = write!(out, " {}200k+{}", c(cfg, AMBER), rst);
    }
    if env.mode == Mode::Standard {
        let _ = write!(
            out,
            " {}|{} last in:{} out:{}",
            c(cfg, DIM),
            rst,
            num.in_tok,
            num.out_tok
        );
    }
    out
}

/// git: branch (age) * ahead behind | PR; or the bare worktree name when
/// the repository could not be opened. Empty when neither is known.
fn git_line(data: &Input, cfg: &Config, env: &Env) -> String {
    let rst = reset(cfg);
    let mut out = String::new();
    if let Some(g) = &env.git {
        let _ = write!(out, "git: {}", g.branch);
        if let Some(secs) = g.age_secs {
            let (label, color) = fmt_age_secs(secs);
            let _ = write!(out, " {}({}){}", c(cfg, color), label, rst);
        }
        if g.dirty {
            let _ = write!(out, " {}*{}", c(cfg, "\x1b[38;2;251;191;36m"), rst);
        }
        if g.ahead > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2191}", "\x1b[38;2;74;222;128m")
            } else {
                ("ahead:", "\x1b[38;2;74;222;128m")
            };
            let _ = write!(out, " {}{}{}{}", c(cfg, color), sym, g.ahead, rst);
        }
        if g.behind > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2193}", "\x1b[38;2;251;113;133m")
            } else {
                ("behind:", "\x1b[38;2;251;113;133m")
            };
            let _ = write!(out, " {}{}{}{}", c(cfg, color), sym, g.behind, rst);
        }
        if cfg.extras
            && let Some(tag) = data.pr.as_ref().and_then(pr_tag)
        {
            let _ = write!(out, " {}|{} {}", c(cfg, DIM), rst, tag);
        }
    } else if let Some(br) = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref())
    {
        // Fallback if gix couldn't open (e.g., not a git repo from the hook's view)
        let _ = write!(out, "git: {}", br);
    }
    out
}

fn five_hour_line(data: &Input, cfg: &Config, now: i64) -> String {
    let Some(five) = data.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref()) else {
        return String::new();
    };
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
    let mut out = format!("5h window: {:.0}% used{}{}", pct, icon, reset);
    if cfg.extras
        && let Some(ts) = five.resets_at
        && let Some(p) = pace(pct, ts, FIVE_HOURS, now)
    {
        out.push_str(&fmt_pace(p, cfg));
    }
    out
}

fn seven_day_line(data: &Input, cfg: &Config, now: i64) -> String {
    let Some(seven) = data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()) else {
        return String::new();
    };
    let pct = seven.used_percentage.unwrap_or(0.0);
    if pct <= 0.0 {
        return String::new();
    }
    let reset = seven
        .resets_at
        .map(|ts| format!(", resets {}", fmt_reset(ts)))
        .unwrap_or_default();
    let mut out = format!("7d window: {:.0}% used{}", pct, reset);
    if cfg.extras
        && let Some(ts) = seven.resets_at
        && let Some(p) = pace(pct, ts, SEVEN_DAYS, now)
    {
        out.push_str(&fmt_pace(p, cfg));
    }
    out
}

fn misc_line(data: &Input, cfg: &Config, env: &Env) -> String {
    let mut misc: Vec<String> = Vec::new();
    if cfg.extras {
        misc.extend(mode_tags(data));
    }
    if cfg.voice
        && let Some(seg) = &env.voice
    {
        misc.push(seg.clone());
    }
    if let Some(mode) = data.vim.as_ref().and_then(|v| v.mode.as_deref()) {
        misc.push(format!("[{}]", mode));
    }
    if let Some(name) = data.agent.as_ref().and_then(|a| a.name.as_deref()) {
        misc.push(format!("{{{}}}", name));
    }
    misc.join(" | ")
}

// ─────────────────────────────────────────────────────────────────────
// Assembly
// ─────────────────────────────────────────────────────────────────────

/// `s` without ANSI escape sequences (colour and cursor codes), for
/// measuring how wide a row prints. Handles CSI (`ESC [ ... final`); a bare
/// ESC followed by anything else drops the ESC alone.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, then one final byte 0x40..=0x7E.
            for next in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

/// Printed width in characters, escapes excluded.
pub(crate) fn visible_len(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

/// `multi`: one row each, in render order. `one`: the non-empty rows joined
/// by a dim ` | `. When that is wider than `width`, whole rows are dropped,
/// lowest priority first (residue, seven-day, five-hour, misc, git, ctx),
/// until it fits. The project row is never dropped, so the line is never
/// blank; if it alone is too wide the terminal clips it.
///
/// Interaction with Compact display mode (`pick_mode`, under 60 columns):
/// the two are independent. Compact shortens the bar and drops `last in/out`
/// from the ctx row; `one` then drops whole rows to fit the same width.
pub(crate) fn assemble(lines: &Lines, cfg: &Config, width: usize) -> String {
    match cfg.line_mode() {
        LineMode::Multi => assemble_multi(lines),
        LineMode::One => assemble_one(lines, cfg, width),
    }
}

fn assemble_multi(lines: &Lines) -> String {
    let mut out = String::with_capacity(512);
    // The first row is written even when empty; the rest bring their own
    // newline, so an absent project row leaves a leading blank line as it
    // always did.
    out.push_str(&lines.project);
    for row in [
        &lines.ctx,
        &lines.residue,
        &lines.git,
        &lines.five_hour,
        &lines.seven_day,
        &lines.misc,
    ] {
        if !row.is_empty() {
            out.push('\n');
            out.push_str(row);
        }
    }
    out
}

fn assemble_one(lines: &Lines, cfg: &Config, width: usize) -> String {
    let sep = dim_bar(cfg);
    // Display order is render order; drop order is the reverse of priority.
    let rows: [(&String, u8); 7] = [
        (&lines.project, 0),
        (&lines.ctx, 1),
        (&lines.residue, 6),
        (&lines.git, 2),
        (&lines.five_hour, 4),
        (&lines.seven_day, 5),
        (&lines.misc, 3),
    ];
    let mut kept: Vec<(&String, u8)> = rows.into_iter().filter(|(s, _)| !s.is_empty()).collect();
    let joined_len = |kept: &[(&String, u8)]| -> usize {
        let text: usize = kept.iter().map(|(s, _)| visible_len(s)).sum();
        text + 3 * kept.len().saturating_sub(1)
    };
    while kept.len() > 1 && joined_len(&kept) > width {
        // Lowest priority = highest number; the project row (0) stays.
        let worst = kept
            .iter()
            .enumerate()
            .max_by_key(|(_, (_, p))| *p)
            .map(|(i, _)| i)
            .unwrap_or(0);
        kept.remove(worst);
    }
    kept.iter()
        .map(|(s, _)| s.as_str())
        .collect::<Vec<_>>()
        .join(&sep)
}
