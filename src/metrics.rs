use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Metrics (SQLite)
// ─────────────────────────────────────────────────────────────────────

pub(crate) type DbResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Where the local metrics file is and whether it is opened as shared
/// (dotfile lock, no WAL): the configured path, else the default under HOME.
pub(crate) fn metrics_db_path(cfg: &Config, home: &str) -> DbResult<(String, bool)> {
    if home.is_empty() {
        return Err("HOME unset".into());
    }
    Ok(match resolve_metrics_db(cfg.metrics_db.as_deref(), home) {
        Some(path) => (path, true),
        None => (local_metrics_path(home), false),
    })
}

/// How long a render waits for the local file: a keystroke is behind it.
pub(crate) const RENDER_PATIENCE: std::time::Duration = std::time::Duration::from_millis(50);

/// How long an opener of a brand-new file waits for each lock. The first
/// opener creates the schema in one write transaction; on a mount where
/// fsync is slow that, plus the other first renders' rows queued behind it,
/// can take longer than RENDER_PATIENCE, and every first render that gave
/// up would lose its row. Once the file has a header no render waits this
/// long again.
pub(crate) const FIRST_LIFE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1);

/// How long a file without a header still counts as brand new. A first
/// opener killed mid-creation can leave an empty file under a lock that
/// nobody removes; after this window renders on it go back to
/// RENDER_PATIENCE instead of waiting FIRST_LIFE_PATIENCE every time.
pub(crate) const FIRST_LIFE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// A file nobody has created a database in yet: missing, or shorter than
/// SQLite's 100-byte header and modified within FIRST_LIFE_WINDOW. Read
/// without any lock, so it never waits.
pub(crate) fn is_brand_new(path: &str) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    if meta.len() >= 100 {
        return false;
    }
    let now = std::time::SystemTime::now();
    meta.modified()
        .map(|m| {
            let age = now
                .duration_since(m)
                .or_else(|_| m.duration_since(now))
                .unwrap_or_default();
            age < FIRST_LIFE_WINDOW
        })
        .unwrap_or(false)
}

pub(crate) fn open_metrics_db(cfg: &Config, home: &str) -> DbResult<Connection> {
    open_metrics_db_with(cfg, home, RENDER_PATIENCE)
}

pub(crate) fn open_metrics_db_with(
    cfg: &Config,
    home: &str,
    patience: std::time::Duration,
) -> DbResult<Connection> {
    let (path, shared) = metrics_db_path(cfg, home)?;
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    open_metrics_at(&path, shared, patience)
}

/// The shared metrics path from config: None for unset or blank; `~`, `~/x`
/// and a relative `x` all land under `home` (`~user` is a literal relative
/// name, not another user's home); an absolute path is itself.
pub(crate) fn resolve_metrics_db(raw: Option<&str>, home: &str) -> Option<String> {
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
pub(crate) fn open_metrics_at(
    path: &str,
    shared: bool,
    patience: std::time::Duration,
) -> DbResult<Connection> {
    // Checked before the open, which creates the file: the openers of a
    // brand-new file wait up to FIRST_LIFE_PATIENCE per lock, for this one
    // connection only, so the schema step and the first rows queued behind
    // it all land. A file that already has a header gets `patience`.
    let patience = if is_brand_new(path) {
        patience.max(FIRST_LIFE_PATIENCE)
    } else {
        patience
    };
    let conn = if shared {
        Connection::open_with_flags_and_vfs(path, rusqlite::OpenFlags::default(), "unix-dotfile")?
    } else {
        Connection::open(path)?
    };
    // The same patience on either kind of file: a render waits
    // RENDER_PATIENCE, the flush passes its own, longer one.
    conn.busy_timeout(patience)?;
    // Switching a brand-new file to WAL needs an exclusive lock, and SQLite
    // answers BUSY at once without consulting the busy handler, so several
    // first openers of a new file would all skip. Retry within patience.
    let mode = if shared { "DELETE" } else { "WAL" };
    let deadline = std::time::Instant::now() + patience;
    loop {
        match conn.query_row(&format!("PRAGMA journal_mode={mode}"), [], |r| {
            r.get::<_, String>(0)
        }) {
            Ok(_) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
    }
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    ensure_schema(&conn)?;
    Ok(conn)
}

/// Create the metrics table, and add the columns newer versions need to a
/// table created by an older one.
pub(crate) fn ensure_schema(conn: &Connection) -> DbResult<()> {
    // A complete schema costs one read and no write lock per render. Only
    // when something is missing do we take BEGIN IMMEDIATE and re-check
    // inside it, so two openers racing cannot hit "duplicate column name".
    if schema_complete(conn)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    match ensure_schema_inner(conn) {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

pub(crate) const METRICS_ADDED_COLUMNS: [(&str, &str); 6] = [
    ("session_id", "TEXT"),
    ("prompt_id", "TEXT"),
    ("content", "INTEGER"),
    ("always_on_chars", "INTEGER"),
    ("always_on_files", "TEXT"),
    // Set only on a row drained from the spill; see `log_row`.
    ("spill_key", "TEXT"),
];

pub(crate) fn schema_complete(conn: &Connection) -> DbResult<bool> {
    let cols: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('metrics') WHERE name IN ('session_id','prompt_id','content','always_on_chars','always_on_files','spill_key')",
        [],
        |r| r.get(0),
    )?;
    let idx: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN ('metrics_session_id', 'metrics_spill_key')",
        [],
        |r| r.get(0),
    )?;
    Ok(cols == METRICS_ADDED_COLUMNS.len() as i64 && idx == 2)
}

pub(crate) fn ensure_schema_inner(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            ts              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S','now')),
            project         TEXT,
            branch          TEXT,
            model           TEXT,
            in_tokens       INTEGER, -- context_window.total_input_tokens: the most recent API response, cache reads included
            out_tokens      INTEGER, -- context_window.total_output_tokens: that response's output, not a session total
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
    for (name, ty) in METRICS_ADDED_COLUMNS {
        if !have.iter().any(|h| h == name) {
            conn.execute_batch(&format!("ALTER TABLE metrics ADD COLUMN {name} {ty};"))?;
        }
    }
    // The residue query reads one session's rows newest first.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS metrics_session_id ON metrics(session_id, id);",
    )?;
    // A drained row lands once even when the drain runs twice.
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS metrics_spill_key ON metrics(spill_key) WHERE spill_key IS NOT NULL;",
    )?;
    Ok(())
}

/// One metrics row, as a render produces it and as the spill keeps it.
/// `ts` is None for a row written as it happens (SQLite stamps it); a spilled
/// row carries the time of the render that produced it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MetricsRow {
    pub(crate) ts: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) content: Option<i64>,
    pub(crate) in_tokens: i64,
    pub(crate) out_tokens: i64,
    pub(crate) context_cap: i64,
    pub(crate) context_pct: f64,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) rate_5h_pct: Option<f64>,
    pub(crate) rate_5h_resets: Option<i64>,
    pub(crate) rate_7d_pct: Option<f64>,
    pub(crate) rate_7d_resets: Option<i64>,
    pub(crate) always_on_chars: Option<i64>,
    pub(crate) always_on_files: Option<String>,
}

/// The numbers the duplicate check compares, with -1 for an absent value.
type RowNumbers = (i64, i64, f64, f64, i64, i64);

impl MetricsRow {
    fn numbers(&self) -> RowNumbers {
        (
            self.in_tokens,
            self.out_tokens,
            self.rate_5h_pct.unwrap_or(-1.0),
            self.rate_7d_pct.unwrap_or(-1.0),
            self.content.unwrap_or(-1),
            self.always_on_chars.unwrap_or(-1),
        )
    }
}

fn same_numbers(a: RowNumbers, b: RowNumbers) -> bool {
    a.0 == b.0
        && a.1 == b.1
        && (a.2 - b.2).abs() < 0.01
        && (a.3 - b.3).abs() < 0.01
        && a.4 == b.4
        && a.5 == b.5
}

const NUMBERS_SELECT: &str = "SELECT in_tokens, out_tokens, COALESCE(rate_5h_pct, -1), COALESCE(rate_7d_pct, -1), COALESCE(content, -1), COALESCE(always_on_chars, -1)";

fn read_numbers(r: &rusqlite::Row<'_>) -> rusqlite::Result<RowNumbers> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn metrics_row(
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
) -> MetricsRow {
    MetricsRow {
        ts: None,
        project: Some(project.to_string()),
        branch: branch.map(str::to_string),
        model: model.map(str::to_string),
        session_id: session_id.map(str::to_string),
        prompt_id: prompt_id.map(str::to_string),
        content,
        in_tokens,
        out_tokens,
        context_cap,
        context_pct,
        cost_usd,
        rate_5h_pct: five_hour.and_then(|w| w.used_percentage),
        rate_5h_resets: five_hour.and_then(|w| w.resets_at),
        rate_7d_pct: seven_day.and_then(|w| w.used_percentage),
        rate_7d_resets: seven_day.and_then(|w| w.resets_at),
        always_on_chars: always_on.map(|a| a.chars as i64),
        always_on_files: always_on.and_then(|a| serde_json::to_string(&a.files).ok()),
    }
}

/// `metrics_row` and `log_row` in one call, for tests.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn log_metrics(
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
    let row = metrics_row(
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
        five_hour,
        seven_day,
        always_on,
    );
    log_row(conn, &row, None)
}

/// Insert `row` unless it repeats this session's last row. `spill_key` is
/// set for a row drained from the spill: the shared file keeps it under a
/// unique index, so draining the same row twice writes it once.
pub(crate) fn log_row(
    conn: &Connection,
    row: &MetricsRow,
    spill_key: Option<&str>,
) -> DbResult<()> {
    let last: Option<RowNumbers> = conn
        .query_row(
            // This session's last row, not the last row of any session: two
            // sessions reporting the same numbers back to back are two rows.
            &format!(
                "{NUMBERS_SELECT} FROM metrics WHERE session_id IS ?1 ORDER BY id DESC LIMIT 1"
            ),
            [&row.session_id],
            read_numbers,
        )
        .ok();
    if last.is_some_and(|l| same_numbers(l, row.numbers())) {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO metrics (ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets, always_on_chars, always_on_files, spill_key)
         VALUES (COALESCE(?1, strftime('%Y-%m-%dT%H:%M:%fZ','now')), ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        rusqlite::params![
            row.ts,
            row.project,
            row.branch,
            row.model,
            row.session_id,
            row.prompt_id,
            row.content,
            row.in_tokens,
            row.out_tokens,
            row.context_cap,
            row.context_pct,
            row.cost_usd,
            row.rate_5h_pct,
            row.rate_5h_resets,
            row.rate_7d_pct,
            row.rate_7d_resets,
            row.always_on_chars,
            row.always_on_files,
            spill_key,
        ],
    )?;
    Ok(())
}

/// Context deltas of the last `n` user turns of `session_id`, oldest first.
/// Rows without a prompt_id (older Claude Code) each count as a turn.
pub(crate) fn residue_deltas(conn: &Connection, session_id: &str, n: usize) -> DbResult<Vec<i64>> {
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
// Spill: rows a render could not write to the shared file
// ─────────────────────────────────────────────────────────────────────
//
// A render on a shared file (`metrics_db`) waits RENDER_PATIENCE for its
// lock. When that runs out the row goes to the `metrics_spill` table of the
// default local file under HOME instead of being dropped. That file is
// already there for the default setup, is on local disk, uses WAL, and is
// only ever opened by this machine, so writing to it does not meet the lock
// that just timed out; a table in it needs no new file format.
//
// The next render that gets the shared lock drains the spill: in one
// transaction it copies the oldest spilled rows for that shared file, then
// writes its own row, and commits. Only after the commit does it delete
// the copied rows from the spill. A crash between the commit and the delete
// leaves rows that are in both files; each spilled row carries a random
// `spill_key` that the shared file keeps under a unique index, so draining
// them again inserts nothing. Deleting first would lose them instead.
//
// The spill holds every session's rows and any render drains them, oldest
// first, so a session that ends right after a skipped render still has its
// row delivered by the next render on the machine. A session's own rows
// keep their order: its next render drains them before writing its own row.

/// Spilled rows one render copies at most.
pub(crate) const DRAIN_BATCH: i64 = 100;

/// How long a render keeps copying spilled rows once it holds the shared
/// lock. Other renders wait RENDER_PATIENCE for that lock, so a drain that
/// held it for longer would make them spill in turn; well under half of it
/// leaves room for the render's own row and the commit. At least one row is
/// copied whatever the clock says, so a backlog always shrinks.
pub(crate) const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(20);

/// The default local metrics file, where the spill lives.
pub(crate) fn local_metrics_path(home: &str) -> String {
    format!("{home}/.config/dbg/statusline-metrics.db")
}

pub(crate) fn open_local_metrics(home: &str) -> DbResult<Connection> {
    let path = local_metrics_path(home);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = open_metrics_at(&path, false, RENDER_PATIENCE)?;
    // The last connection to close a WAL file checkpoints it, holding the
    // file exclusively through an fsync. On a busy disk that fsync outlasts
    // RENDER_PATIENCE, and the renders spilling at the same moment, all of
    // which just gave up on the shared file, failed to keep their rows. The
    // spill's connections skip that checkpoint; SQLite's automatic one, which
    // runs after a commit without blocking readers or writers, still keeps
    // the WAL to about a thousand pages.
    conn.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )?;
    Ok(conn)
}

pub(crate) fn ensure_spill(local: &Connection) -> DbResult<()> {
    local.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics_spill (
            spill_id        INTEGER PRIMARY KEY AUTOINCREMENT,
            target          TEXT NOT NULL, -- the shared file the row is for
            spill_key       TEXT NOT NULL UNIQUE,
            ts              TEXT NOT NULL,
            project         TEXT,
            branch          TEXT,
            model           TEXT,
            session_id      TEXT,
            prompt_id       TEXT,
            content         INTEGER,
            in_tokens       INTEGER NOT NULL,
            out_tokens      INTEGER NOT NULL,
            context_cap     INTEGER NOT NULL,
            context_pct     REAL NOT NULL,
            cost_usd        REAL,
            rate_5h_pct     REAL,
            rate_5h_resets  INTEGER,
            rate_7d_pct     REAL,
            rate_7d_resets  INTEGER,
            always_on_chars INTEGER,
            always_on_files TEXT,
            failed          TEXT -- why the shared file refused the row; never drained again
        );
        CREATE INDEX IF NOT EXISTS metrics_spill_target ON metrics_spill(target, spill_id);",
    )?;
    Ok(())
}

/// Keep `row` for `target`, stamped now. A row that repeats this session's
/// last spilled row is dropped, as the shared file would drop it.
pub(crate) fn spill_row(local: &Connection, target: &str, row: &MetricsRow) -> DbResult<()> {
    ensure_spill(local)?;
    let last: Option<RowNumbers> = local
        .query_row(
            &format!(
                "{NUMBERS_SELECT} FROM metrics_spill WHERE target = ?1 AND session_id IS ?2 ORDER BY spill_id DESC LIMIT 1"
            ),
            rusqlite::params![target, row.session_id],
            read_numbers,
        )
        .ok();
    if last.is_some_and(|l| same_numbers(l, row.numbers())) {
        return Ok(());
    }
    local.execute(
        "INSERT INTO metrics_spill (target, spill_key, ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets, always_on_chars, always_on_files)
         VALUES (?1, ?2, COALESCE(?3, strftime('%Y-%m-%dT%H:%M:%fZ','now')), ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        rusqlite::params![
            target,
            mint_id(),
            row.ts,
            row.project,
            row.branch,
            row.model,
            row.session_id,
            row.prompt_id,
            row.content,
            row.in_tokens,
            row.out_tokens,
            row.context_cap,
            row.context_pct,
            row.cost_usd,
            row.rate_5h_pct,
            row.rate_5h_resets,
            row.rate_7d_pct,
            row.rate_7d_resets,
            row.always_on_chars,
            row.always_on_files,
        ],
    )?;
    Ok(())
}

/// A row waiting in the spill.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spilled {
    pub(crate) spill_id: i64,
    pub(crate) spill_key: String,
    pub(crate) row: MetricsRow,
}

/// The oldest `limit` spilled rows for `target` not set aside as refused;
/// none when there is no spill.
pub(crate) fn read_spill(local: &Connection, target: &str, limit: i64) -> DbResult<Vec<Spilled>> {
    let has: i64 = local.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'metrics_spill'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = local.prepare(
        "SELECT spill_id, spill_key, ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens, context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets, always_on_chars, always_on_files
         FROM metrics_spill WHERE target = ?1 AND failed IS NULL ORDER BY spill_id LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![target, limit], |r| {
        Ok(Spilled {
            spill_id: r.get(0)?,
            spill_key: r.get(1)?,
            row: MetricsRow {
                ts: r.get(2)?,
                project: r.get(3)?,
                branch: r.get(4)?,
                model: r.get(5)?,
                session_id: r.get(6)?,
                prompt_id: r.get(7)?,
                content: r.get(8)?,
                in_tokens: r.get(9)?,
                out_tokens: r.get(10)?,
                context_cap: r.get(11)?,
                context_pct: r.get(12)?,
                cost_usd: r.get(13)?,
                rate_5h_pct: r.get(14)?,
                rate_5h_resets: r.get(15)?,
                rate_7d_pct: r.get(16)?,
                rate_7d_resets: r.get(17)?,
                always_on_chars: r.get(18)?,
                always_on_files: r.get(19)?,
            },
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Remove rows the shared file now holds.
pub(crate) fn delete_spilled(local: &Connection, done: &[Spilled]) -> DbResult<()> {
    local.execute_batch("BEGIN IMMEDIATE;")?;
    let inner = || -> DbResult<()> {
        let mut del = local.prepare("DELETE FROM metrics_spill WHERE spill_id = ?1")?;
        for s in done {
            del.execute([s.spill_id])?;
        }
        Ok(())
    };
    match inner().and_then(|()| Ok(local.execute_batch("COMMIT;")?)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = local.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Spill `row` for `target`, retrying a busy local file within
/// RENDER_PATIENCE. Several renders that all just timed out on the shared
/// file spill at once, and a WAL file can answer BUSY without waiting (a
/// connection opening it while another one closes it, say), so the busy
/// timeout alone does not cover every case.
pub(crate) fn keep_locally(home: &str, target: &str, row: &MetricsRow) -> DbResult<()> {
    let deadline = std::time::Instant::now() + RENDER_PATIENCE;
    loop {
        match open_local_metrics(home).and_then(|l| spill_row(&l, target, row)) {
            Err(e) if is_busy(e.as_ref()) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            r => return r,
        }
    }
}

/// What a drain did with the spilled rows it was handed, by index.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Drained {
    /// Rows the shared file now holds (written, already there, or dropped
    /// as a repeat): delete them from the spill.
    pub(crate) written: Vec<usize>,
    /// Rows the shared file refused for a reason other than a lock, with
    /// the error: mark them in the spill so no later drain retries them.
    pub(crate) refused: Vec<(usize, String)>,
}

/// In one transaction on the shared file: the spilled rows in order, as many
/// as `budget` allows (at least one), then `own`. Each spilled row has its
/// own savepoint, so a row the shared file refuses for any reason other than
/// a lock is undone alone, reported in `refused`, and the drain goes on with
/// the next one: one bad row never stops the rows behind it, nor costs a
/// render its own row. A lock (not expected while the transaction holds the
/// file) ends the drain with the row left for later.
pub(crate) fn write_with_drain(
    shared: &Connection,
    spilled: &[Spilled],
    own: &MetricsRow,
    budget: std::time::Duration,
) -> DbResult<Drained> {
    shared.execute_batch("BEGIN IMMEDIATE;")?;
    let start = std::time::Instant::now();
    let inner = || -> DbResult<Drained> {
        let mut out = Drained::default();
        for (i, s) in spilled.iter().enumerate() {
            if i > 0 && start.elapsed() >= budget {
                break;
            }
            shared.execute_batch("SAVEPOINT spilled_row;")?;
            match log_row(shared, &s.row, Some(&s.spill_key)) {
                Ok(()) => out.written.push(i),
                Err(e) => {
                    shared.execute_batch("ROLLBACK TO spilled_row;")?;
                    if is_busy(e.as_ref()) {
                        shared.execute_batch("RELEASE spilled_row;")?;
                        break;
                    }
                    out.refused.push((i, e.to_string()));
                }
            }
            shared.execute_batch("RELEASE spilled_row;")?;
        }
        log_row(shared, own, None)?;
        shared.execute_batch("COMMIT;")?;
        Ok(out)
    };
    match inner() {
        Ok(d) => Ok(d),
        Err(e) => {
            let _ = shared.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Mark a spilled row the shared file refused: later drains skip it, and it
/// stays in the spill, with the error, for whoever repairs it.
pub(crate) fn mark_refused(local: &Connection, spill_id: i64, error: &str) -> DbResult<()> {
    local.execute(
        "UPDATE metrics_spill SET failed = ?2 WHERE spill_id = ?1",
        rusqlite::params![spill_id, error],
    )?;
    Ok(())
}

/// Write this render's row to `shared` (a `metrics_db` file), draining the
/// spill for it first. Rows are deleted from the spill only after the
/// shared file has committed them.
pub(crate) fn write_shared(
    shared: &Connection,
    home: &str,
    target: &str,
    own: &MetricsRow,
) -> DbResult<()> {
    // No local file yet means nothing was ever spilled: skip opening it.
    let local = if std::path::Path::new(&local_metrics_path(home)).exists() {
        open_local_metrics(home).ok()
    } else {
        None
    };
    let spilled = local
        .as_ref()
        .and_then(|l| read_spill(l, target, DRAIN_BATCH).ok())
        .unwrap_or_default();
    let drained = write_with_drain(shared, &spilled, own, DRAIN_BUDGET)?;
    if let Some(l) = &local {
        let done: Vec<Spilled> = drained
            .written
            .iter()
            .map(|&i| spilled[i].clone())
            .collect();
        if !done.is_empty() {
            // A failure here leaves rows the next drain finds already written.
            let _ = delete_spilled(l, &done);
        }
        for (i, error) in &drained.refused {
            let id = spilled[*i].spill_id;
            // Said once per row: a marked row is never drained again. If the
            // mark fails the next drain meets the row and says it again.
            if mark_refused(l, id, error).is_ok() {
                eprintln!(
                    "claude-statusline-rust: kept metrics row {id} refused by the shared file and set aside: {error}"
                );
            }
        }
    }
    Ok(())
}

/// Record this render's row. Returns the metrics connection, for the
/// residue line, and whether the database was busy or locked.
///
/// On the default local file a busy file skips the row, as it always did.
/// On a shared file a busy file at open or at insert sends the row to the
/// spill; the render still shows `db:locked` and prints one stderr line,
/// because a lock that stays stuck must stay visible.
pub(crate) fn record_render(
    cfg: &Config,
    home: &str,
    row: &MetricsRow,
) -> (Option<Connection>, bool) {
    // A shared path that is the local file itself has nowhere else to go.
    let target = metrics_db_path(cfg, home)
        .ok()
        .filter(|(p, shared)| *shared && *p != local_metrics_path(home))
        .map(|(p, _)| p);
    let report = |e: &(dyn std::error::Error + 'static), what: &str| -> bool {
        let busy = is_busy(e);
        match &target {
            Some(t) if busy => match keep_locally(home, t, row) {
                Ok(()) => eprintln!(
                    "claude-statusline-rust: metrics row kept locally until the shared file is free: {e}"
                ),
                Err(e2) => eprintln!(
                    "claude-statusline-rust: {what}: {e}; keeping it locally failed: {e2}"
                ),
            },
            // The shared file is unreachable for another reason: this row
            // is skipped, never forced. One line, so a hook or a log shows it.
            _ if cfg.metrics_db.is_some() => eprintln!("claude-statusline-rust: {what}: {e}"),
            _ => {}
        }
        busy
    };
    let conn = match open_metrics_db(cfg, home) {
        Ok(c) => c,
        Err(e) => return (None, report(e.as_ref(), "metrics skipped")),
    };
    let written = match &target {
        Some(t) => write_shared(&conn, home, t, row),
        None => log_row(&conn, row, None),
    };
    let busy = match &written {
        Ok(()) => false,
        Err(e) => report(e.as_ref(), "metrics row skipped"),
    };
    (Some(conn), busy)
}

#[cfg(test)]
#[path = "spill_tests.rs"]
mod spill_tests;
