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
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, Deserialize)]
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
// lock. When that runs out the row is kept in a file of its own under
// `<local metrics dir>/spill/<hash of the shared path>/`, named
// `<microseconds>-<session>-<spill key>.json`: one JSON object with the
// row's columns, its timestamp and a random `spill_key`. The file is
// written under a dot-name and renamed into place, so it appears whole or
// not at all. Keeping a row takes no lock of any kind: nothing in this
// step can be busy.
//
// One file per row, not one append-only file per session: a drain that
// read a session's file and then deleted or rewrote it would lose a row
// appended between the read and the delete, and closing that window would
// need a lock again. A row file is never written after it appears.
//
// The next render that gets the shared lock drains, inside the same
// BEGIN IMMEDIATE as its own row: it reads the oldest row files (names
// sort by time), inserts them, inserts its own row and commits. Only after
// the commit does it delete the files it wrote. A crash between the commit
// and the delete leaves rows that are in both places; the shared file keeps
// `spill_key` under a unique index, so draining them again inserts nothing.
// Drains are serialised by the shared lock itself.
//
// Any render drains every session's rows, oldest first, so a session that
// ended right after a skipped render still has its row delivered. A
// session's own rows keep their order: its next render drains them before
// writing its own row.
//
// A row file that does not parse, or that the shared file refuses for a
// reason other than a lock, is moved to `failed/` beside it with the error,
// named once on stderr, and never retried or deleted.

/// Kept rows one render reads at most.
pub(crate) const DRAIN_BATCH: usize = 100;

/// How long a render keeps copying kept rows once it holds the shared
/// lock. Other renders wait RENDER_PATIENCE for that lock, so a drain that
/// held it for longer would make them give up in turn; well under half of
/// it leaves room for the render's own row and the commit. At least one row
/// is copied whatever the clock says, so a backlog always shrinks.
pub(crate) const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(20);

/// The default local metrics file.
pub(crate) fn local_metrics_path(home: &str) -> String {
    format!("{home}/.config/dbg/statusline-metrics.db")
}

/// FNV-1a, 64 bits: a short, stable name for a shared path.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Where rows kept for `target` wait.
pub(crate) fn spill_dir(home: &str, target: &str) -> std::path::PathBuf {
    std::path::Path::new(home)
        .join(".config/dbg/spill")
        .join(format!("{:016x}", fnv1a(target)))
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for a time since the epoch, the format the
/// shared file stamps its own rows with.
pub(crate) fn utc_stamp(since_epoch: std::time::Duration) -> String {
    let secs = since_epoch.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to a civil date (proleptic Gregorian), after H. Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        since_epoch.subsec_millis()
    )
}

/// One kept row as its file holds it.
#[derive(serde::Serialize, Deserialize, Debug, PartialEq)]
pub(crate) struct KeptRow {
    pub(crate) spill_key: String,
    #[serde(flatten)]
    pub(crate) row: MetricsRow,
}

/// A session id as part of a file name: letters, digits, `-` and `_` kept,
/// anything else `_`, at most 64 characters; `_none` without one. The
/// file's content carries the real id.
fn name_part(session: Option<&str>) -> String {
    match session {
        None => "_none".into(),
        Some(s) => s
            .chars()
            .take(64)
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
    }
}

/// Keep `row` for `target`, stamped now. Returns the file it is in.
pub(crate) fn keep_row(
    home: &str,
    target: &str,
    row: &MetricsRow,
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let dir = spill_dir(home, target);
    std::fs::create_dir_all(&dir)?;
    // Which shared file the directory is for, for whoever looks at it.
    let named = dir.join("target.txt");
    if !named.exists() {
        let _ = std::fs::write(&named, format!("{target}\n"));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let key = mint_id();
    let mut kept = KeptRow {
        spill_key: key.clone(),
        row: row.clone(),
    };
    kept.row.ts.get_or_insert_with(|| utc_stamp(now));
    let body = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".{key}.tmp"));
    let path = dir.join(format!(
        "{:020}-{}-{key}.json",
        now.as_micros(),
        name_part(row.session_id.as_deref())
    ));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&body)?;
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The oldest `limit` row files in `dir`, oldest first; none when there is
/// no directory.
pub(crate) fn pending(dir: &std::path::Path, limit: usize) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.') && n.ends_with(".json"))
        .collect();
    names.sort();
    names.truncate(limit);
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// What a drain did with the kept rows.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Drained {
    /// Files whose row the shared file now holds (written, already there,
    /// or dropped as a repeat): delete them.
    pub(crate) written: Vec<std::path::PathBuf>,
    /// Files that did not parse or whose row the shared file refused for a
    /// reason other than a lock, with the error: set them aside.
    pub(crate) refused: Vec<(std::path::PathBuf, String)>,
}

/// In one transaction on the shared file: the kept rows in `dir`, oldest
/// first, as many as DRAIN_BATCH and `budget` allow (at least one), then
/// `own`. Each kept row has its own savepoint, so a refused one is undone
/// alone and the drain goes on with the next: one bad row never stops the
/// rows behind it, nor costs a render its own row. A lock (not expected
/// while the transaction holds the file) ends the drain with the row left
/// for later. Nothing is deleted here.
pub(crate) fn write_with_drain(
    shared: &Connection,
    dir: Option<&std::path::Path>,
    own: &MetricsRow,
    budget: std::time::Duration,
) -> DbResult<Drained> {
    shared.execute_batch("BEGIN IMMEDIATE;")?;
    let start = std::time::Instant::now();
    let inner = || -> DbResult<Drained> {
        let mut out = Drained::default();
        let files = dir.map(|d| pending(d, DRAIN_BATCH)).unwrap_or_default();
        for (i, path) in files.into_iter().enumerate() {
            if i > 0 && start.elapsed() >= budget {
                break;
            }
            // A file that cannot be read now (removed by hand, say) is left.
            let Ok(text) = std::fs::read(&path) else {
                continue;
            };
            let kept: KeptRow = match serde_json::from_slice(&text) {
                Ok(k) => k,
                Err(e) => {
                    out.refused.push((path, format!("does not parse: {e}")));
                    continue;
                }
            };
            shared.execute_batch("SAVEPOINT kept_row;")?;
            match log_row(shared, &kept.row, Some(&kept.spill_key)) {
                Ok(()) => out.written.push(path),
                Err(e) => {
                    shared.execute_batch("ROLLBACK TO kept_row;")?;
                    if is_busy(e.as_ref()) {
                        shared.execute_batch("RELEASE kept_row;")?;
                        break;
                    }
                    out.refused.push((path, e.to_string()));
                }
            }
            shared.execute_batch("RELEASE kept_row;")?;
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

/// Move a refused row file to `failed/` beside it, wrapped with the error:
/// never retried, never deleted. The copy is written before the original
/// goes, so a crash in between leaves the row in both places, not neither.
pub(crate) fn set_aside(
    path: &std::path::Path,
    error: &str,
) -> std::io::Result<std::path::PathBuf> {
    let dir = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("failed");
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().unwrap_or_default();
    let raw = std::fs::read(path)?;
    let body = serde_json::json!({
        "error": error,
        "kept": String::from_utf8_lossy(&raw),
    });
    let dest = dir.join(name);
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    std::fs::write(&tmp, body.to_string())?;
    std::fs::rename(&tmp, &dest)?;
    std::fs::remove_file(path)?;
    Ok(dest)
}

/// Write this render's row to `shared` (a `metrics_db` file), draining the
/// rows kept for it first. Kept files are deleted only after the shared
/// file has committed their rows.
pub(crate) fn write_shared(
    shared: &Connection,
    home: &str,
    target: &str,
    own: &MetricsRow,
) -> DbResult<()> {
    let dir = spill_dir(home, target);
    // No directory means nothing was ever kept: one stat, no listing.
    let dir = dir.is_dir().then_some(dir);
    let drained = write_with_drain(shared, dir.as_deref(), own, DRAIN_BUDGET)?;
    for path in &drained.written {
        // A failure here leaves a row the next drain finds already written.
        let _ = std::fs::remove_file(path);
    }
    for (path, error) in &drained.refused {
        // Said once per row: a row set aside is never drained again. If
        // moving it fails the next drain meets it and says it again.
        if let Ok(dest) = set_aside(path, error) {
            eprintln!(
                "claude-statusline-rust: kept metrics row set aside in {}: {error}",
                dest.display()
            );
        }
    }
    Ok(())
}

/// Record this render's row. Returns the metrics connection, for the
/// residue line, and whether the database was busy or locked.
///
/// On the default local file a busy file skips the row, as it always did.
/// On a shared file a busy file at open or at insert keeps the row in a
/// file of its own under the local metrics directory; the render still shows `db:locked` and prints one stderr line,
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
            Some(t) if busy => match keep_row(home, t, row) {
                Ok(_) => eprintln!(
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
