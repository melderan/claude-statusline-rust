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
        None => (format!("{home}/.config/dbg/statusline-metrics.db"), false),
    })
}

/// How long a render waits for the local file: a keystroke is behind it.
pub(crate) const RENDER_PATIENCE: std::time::Duration = std::time::Duration::from_millis(50);

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

pub(crate) const METRICS_ADDED_COLUMNS: [(&str, &str); 5] = [
    ("session_id", "TEXT"),
    ("prompt_id", "TEXT"),
    ("content", "INTEGER"),
    ("always_on_chars", "INTEGER"),
    ("always_on_files", "TEXT"),
];

pub(crate) fn schema_complete(conn: &Connection) -> DbResult<bool> {
    let cols: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('metrics') WHERE name IN ('session_id','prompt_id','content','always_on_chars','always_on_files')",
        [],
        |r| r.get(0),
    )?;
    let idx: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'metrics_session_id'",
        [],
        |r| r.get(0),
    )?;
    Ok(cols == METRICS_ADDED_COLUMNS.len() as i64 && idx == 1)
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
    Ok(())
}

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
    let last: Option<(i64, i64, f64, f64, i64, i64)> = conn
        .query_row(
            // This session's last row, not the last row of any session: two
            // sessions reporting the same numbers back to back are two rows.
            "SELECT in_tokens, out_tokens, COALESCE(rate_5h_pct, -1), COALESCE(rate_7d_pct, -1), COALESCE(content, -1), COALESCE(always_on_chars, -1) FROM metrics WHERE session_id IS ?1 ORDER BY id DESC LIMIT 1",
            [session_id],
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
