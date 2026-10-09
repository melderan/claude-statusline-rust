use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Lines: each row of the status line is built as its own value, then
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

/// Rank of a piece that is never dropped.
pub(crate) const KEEP: u8 = u8::MAX;

/// Drop order for one-line mode: a piece with a lower rank goes first.
/// Tails of every row go before any head, so a narrow line keeps the
/// project, the context size and the branch the longest.
pub(crate) mod rank {
    pub(crate) const RESIDUE: u8 = 1;
    pub(crate) const SEVEN_DAY: u8 = 2;
    pub(crate) const FIVE_HOUR: u8 = 3;
    pub(crate) const MISC: u8 = 4;
    /// ctx tail: the cache segment, then last in/out, then the cost.
    pub(crate) const CTX_CACHE: u8 = 5;
    pub(crate) const CTX_LAST: u8 = 6;
    pub(crate) const CTX_COST: u8 = 7;
    /// project tail: CC version, memory sizes, always-on size, duration.
    pub(crate) const PROJECT_CC: u8 = 8;
    pub(crate) const PROJECT_MEM: u8 = 9;
    pub(crate) const PROJECT_ON: u8 = 10;
    pub(crate) const PROJECT_DUR: u8 = 11;
    /// git tail: the age, then the PR tag.
    pub(crate) const GIT_AGE: u8 = 12;
    pub(crate) const GIT_PR: u8 = 13;
    /// Heads, last to go: git, then ctx. The project head is KEEP.
    pub(crate) const GIT_HEAD: u8 = 14;
    pub(crate) const CTX_HEAD: u8 = 15;
}

/// A run of text inside a row, with the place it takes in the drop order.
/// A piece carries its own leading separator, so any subset of a row's
/// pieces, in order, reads as a whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) text: String,
    pub(crate) rank: u8,
    /// Part of the row's head (what the row is about) rather than its tail.
    pub(crate) head: bool,
}

/// One row of the status line: its pieces, in print order.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) pieces: Vec<Piece>,
}

impl Row {
    pub(crate) fn push_head(&mut self, text: String, rank: u8) {
        self.push(text, rank, true);
    }

    pub(crate) fn push_tail(&mut self, text: String, rank: u8) {
        self.push(text, rank, false);
    }

    fn push(&mut self, text: String, rank: u8, head: bool) {
        if !text.is_empty() {
            self.pieces.push(Piece { text, rank, head });
        }
    }

    /// A row that is one piece, dropped whole.
    pub(crate) fn whole(text: String, rank: u8) -> Self {
        let mut row = Self::default();
        row.push_tail(text, rank);
        row
    }

    /// The row as printed in multi-line mode: every piece, in order.
    pub(crate) fn text(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
}

/// The rows, in render order. An empty row is absent.
#[derive(Default, Debug, PartialEq, Eq, Clone)]
pub(crate) struct Lines {
    /// Project, model, then version, duration, memory sizes, always-on size.
    pub(crate) project: Row,
    pub(crate) ctx: Row,
    pub(crate) residue: Row,
    pub(crate) git: Row,
    pub(crate) five_hour: Row,
    pub(crate) seven_day: Row,
    pub(crate) misc: Row,
}

impl Lines {
    pub(crate) fn in_order(&self) -> [&Row; 7] {
        [
            &self.project,
            &self.ctx,
            &self.residue,
            &self.git,
            &self.five_hour,
            &self.seven_day,
            &self.misc,
        ]
    }
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
        project: project_row(data, cfg, env, project_dir, current_dir),
        ..Lines::default()
    };

    // Cost and cache ride on the end of the ctx row. With no context window
    // size there is no ctx row, and they land on the project row instead, as
    // they always have.
    let mut tail: Vec<Piece> = Vec::new();
    if let Some(usd) = data.cost.as_ref().and_then(|c| c.total_cost_usd)
        && usd > 0.001
    {
        tail.push(Piece {
            text: format!(" {}|{} ${:.2}", c(cfg, DIM), rst, usd),
            rank: rank::CTX_COST,
            head: false,
        });
    }
    if cfg.cache
        && let Some(pc) = data.prompt_cache.as_ref()
        && let Some(seg) = cache_segment(pc, env.now, cfg)
    {
        tail.push(Piece {
            text: format!(" {}|{} {}", c(cfg, DIM), rst, seg),
            rank: rank::CTX_CACHE,
            head: false,
        });
    }
    let target = if num.cap > 0 {
        lines.ctx = ctx_row(data, cfg, env, &num);
        &mut lines.ctx
    } else {
        &mut lines.project
    };
    target.pieces.extend(tail);

    if !env.residue.is_empty() {
        let mut text = String::from("res:");
        for d in &env.residue {
            text.push(' ');
            text.push_str(&fmt_delta(*d));
        }
        lines.residue = Row::whole(text, rank::RESIDUE);
    }

    lines.git = git_row(data, cfg, env);
    lines.five_hour = five_hour_row(data, cfg, env.now);
    lines.seven_day = seven_day_row(data, cfg, env.now);
    lines.misc = Row::whole(misc_text(data, cfg, env), rank::MISC);
    lines
}

/// Head: project path (tilde form, with `cd` suffix) and model. Tail: CC
/// version, duration, memory sizes, always-on size.
fn project_row(data: &Input, cfg: &Config, env: &Env, project_dir: &str, current_dir: &str) -> Row {
    let rst = reset(cfg);
    let mut row = Row::default();
    let rel_cur = if !project_dir.is_empty() && !current_dir.is_empty() {
        relative_current(project_dir, current_dir)
    } else {
        None
    };
    let mut path = String::new();
    if !project_dir.is_empty() {
        path.push_str(&tilde(project_dir));
        if let Some(suffix) = &rel_cur {
            if cfg.glyphs {
                let _ = write!(path, " {}\u{2192}{} {}", c(cfg, DIM), rst, suffix);
            } else {
                let _ = write!(path, " {}|{} cd:{}", c(cfg, DIM), rst, suffix);
            }
        }
    }
    let path_empty = path.is_empty();
    row.push_head(path, KEEP);

    if let Some(name) = data.model.as_ref().and_then(|m| m.display_name.as_deref()) {
        let short = name.strip_prefix("Claude ").unwrap_or(name);
        let sep = if path_empty { "" } else { " | " };
        row.push_head(format!("{sep}{short}"), KEEP);
    }

    if let Some(cc) = data.version.as_deref() {
        row.push_tail(
            format!(" {}|{} CC:{}", c(cfg, DIM), rst, cc),
            rank::PROJECT_CC,
        );
    }

    if let Some(ms) = data.cost.as_ref().and_then(|c| c.total_duration_ms) {
        let label = if cfg.glyphs { "\u{23F1}" } else { "dur:" };
        row.push_tail(
            format!(" {}|{} {}{}", c(cfg, DIM), rst, label, fmt_duration_ms(ms)),
            rank::PROJECT_DUR,
        );
    }

    let (idx, other) = env.memory;
    if !project_dir.is_empty() && (idx > 0 || other > 0) {
        row.push_tail(
            format!(
                " {}|{} mem:{}+{}",
                c(cfg, DIM),
                rst,
                fmt_bytes(idx),
                fmt_bytes(other)
            ),
            rank::PROJECT_MEM,
        );
    }

    if env.on_chars > 0 {
        row.push_tail(
            format!(" {}|{} on:{}", c(cfg, DIM), rst, fmt_chars(env.on_chars)),
            rank::PROJECT_ON,
        );
    }
    row
}

/// Head: `ctx [bar] pct (used/cap)` with the compact and 200k+ markers.
/// Tail: `last in/out`, then cost and cache (added by build_lines). Empty
/// when the window size is unknown.
fn ctx_row(data: &Input, cfg: &Config, env: &Env, num: &CtxNumbers) -> Row {
    let rst = reset(cfg);
    let cap = num.cap;
    let mut row = Row::default();
    if cap <= 0 {
        return row;
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
    if cfg.extras
        && let Some(marker) = compact_marker(cap, current_tok, cfg.compact_reserve(), cfg)
    {
        let _ = write!(out, " {}", marker);
    }
    if cfg.extras && data.exceeds_200k_tokens == Some(true) {
        let _ = write!(out, " {}200k+{}", c(cfg, AMBER), rst);
    }
    row.push_head(out, rank::CTX_HEAD);
    if env.mode == Mode::Standard {
        row.push_tail(
            format!(
                " {}|{} last in:{} out:{}",
                c(cfg, DIM),
                rst,
                num.in_tok,
                num.out_tok
            ),
            rank::CTX_LAST,
        );
    }
    row
}

/// Head: `git: branch`, dirty star, ahead and behind. Tail: the age and
/// the PR tag. Or the bare worktree name when the repository could not be
/// opened. Empty when neither is known.
fn git_row(data: &Input, cfg: &Config, env: &Env) -> Row {
    let rst = reset(cfg);
    let mut row = Row::default();
    if let Some(g) = &env.git {
        row.push_head(format!("git: {}", g.branch), rank::GIT_HEAD);
        if let Some(secs) = g.age_secs {
            let (label, color) = fmt_age_secs(secs);
            row.push_tail(
                format!(" {}({}){}", c(cfg, color), label, rst),
                rank::GIT_AGE,
            );
        }
        if g.dirty {
            row.push_head(
                format!(" {}*{}", c(cfg, "\x1b[38;2;251;191;36m"), rst),
                rank::GIT_HEAD,
            );
        }
        if g.ahead > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2191}", "\x1b[38;2;74;222;128m")
            } else {
                ("ahead:", "\x1b[38;2;74;222;128m")
            };
            row.push_head(
                format!(" {}{}{}{}", c(cfg, color), sym, g.ahead, rst),
                rank::GIT_HEAD,
            );
        }
        if g.behind > 0 {
            let (sym, color) = if cfg.glyphs {
                ("\u{2193}", "\x1b[38;2;251;113;133m")
            } else {
                ("behind:", "\x1b[38;2;251;113;133m")
            };
            row.push_head(
                format!(" {}{}{}{}", c(cfg, color), sym, g.behind, rst),
                rank::GIT_HEAD,
            );
        }
        if cfg.extras
            && let Some(tag) = data.pr.as_ref().and_then(pr_tag)
        {
            row.push_tail(format!(" {}|{} {}", c(cfg, DIM), rst, tag), rank::GIT_PR);
        }
    } else if let Some(br) = data
        .workspace
        .as_ref()
        .and_then(|w| w.git_worktree.as_deref())
    {
        // Fallback if gix couldn't open (e.g., not a git repo from the hook's view)
        row.push_head(format!("git: {}", br), rank::GIT_HEAD);
    }
    row
}

fn five_hour_row(data: &Input, cfg: &Config, now: i64) -> Row {
    let Some(five) = data.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref()) else {
        return Row::default();
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
    Row::whole(out, rank::FIVE_HOUR)
}

fn seven_day_row(data: &Input, cfg: &Config, now: i64) -> Row {
    let Some(seven) = data.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref()) else {
        return Row::default();
    };
    let pct = seven.used_percentage.unwrap_or(0.0);
    if pct <= 0.0 {
        return Row::default();
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
    Row::whole(out, rank::SEVEN_DAY)
}

fn misc_text(data: &Input, cfg: &Config, env: &Env) -> String {
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
/// by a dim ` | `, in render order. When that is wider than `width`, pieces
/// are dropped in rank order (see `rank`) until it fits: the residue row,
/// the rate-limit rows and the misc row, then the tail of each of ctx,
/// project and git, then the git head and the ctx head. The project head
/// (path and model) is never dropped, and neither is the last piece left,
/// so the line is never blank; if what remains is still too wide the
/// terminal clips it.
///
/// Interaction with Compact display mode (`pick_mode`, under 60 columns):
/// the two are independent. Compact shortens the bar and leaves `last in/out`
/// off the ctx row; `one` then drops pieces to fit the same width.
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
    let rows = lines.in_order();
    out.push_str(&rows[0].text());
    for row in &rows[1..] {
        if !row.is_empty() {
            out.push('\n');
            out.push_str(&row.text());
        }
    }
    out
}

/// Width of the rows joined by a 3-column separator.
fn joined_width(rows: &[Row]) -> usize {
    let widths: Vec<usize> = rows
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| visible_len(&r.text()))
        .collect();
    widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1)
}

fn assemble_one(lines: &Lines, cfg: &Config, width: usize) -> String {
    let mut rows: Vec<Row> = lines.in_order().into_iter().cloned().collect();
    while joined_width(&rows) > width {
        let next = rows
            .iter()
            .flat_map(|r| r.pieces.iter())
            .map(|p| p.rank)
            .filter(|r| *r != KEEP)
            .min();
        let Some(next) = next else { break };
        let left: usize = rows
            .iter()
            .flat_map(|r| r.pieces.iter())
            .filter(|p| p.rank != next)
            .count();
        if left == 0 {
            break;
        }
        for row in &mut rows {
            row.pieces.retain(|p| p.rank != next);
        }
    }
    rows.iter()
        .filter(|r| !r.is_empty())
        .map(|r| r.text())
        .collect::<Vec<_>>()
        .join(&dim_bar(cfg))
}

// ─────────────────────────────────────────────────────────────────────
// Auto-compact marker
// ─────────────────────────────────────────────────────────────────────

/// Tokens Claude Code keeps free when it compacts on its own. Default for
/// `compact_reserve`, following claude-powerline's compaction buffer.
pub(crate) const COMPACT_RESERVE_DEFAULT: i64 = 33_000;

/// `compact in 12k` once the context is within 20% of the window of the
/// point where Claude Code compacts by itself; `compact!` at or past it.
/// None further out, with no usage, with an unknown window, when the
/// reserve is negative (the way to switch it off) or is not smaller than
/// the window.
///
/// The compaction point is `cap - reserve`. As of 2026-10-08 the statusline
/// payload carries no field for it: the documented `context_window` fields
/// are the sizes, the percentages and `current_usage`, and 20 payloads
/// captured from a live session had none either (no key containing
/// "compact" anywhere). So the point comes from `compact_reserve`. If a
/// threshold field ever appears, replace `reserve` with it here.
pub(crate) fn compact_marker(
    cap: i64,
    current_tok: i64,
    reserve: i64,
    cfg: &Config,
) -> Option<String> {
    if cap <= 0 || current_tok <= 0 || reserve < 0 || reserve >= cap {
        return None;
    }
    let left = (cap - reserve) - current_tok;
    let rst = reset(cfg);
    if left <= 0 {
        return Some(format!("{}compact!{}", c(cfg, AMBER), rst));
    }
    if left >= cap / 5 {
        return None;
    }
    // Round up so the last few hundred tokens read 1k, never 0k.
    Some(format!(
        "{}compact in {}k{}",
        c(cfg, AMBER),
        (left + 999) / 1000,
        rst
    ))
}
