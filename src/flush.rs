use crate::*;

// ─────────────────────────────────────────────────────────────────────
// --flush: copy local metrics into the house recorder
// ─────────────────────────────────────────────────────────────────────
//
// The render writes its rows to the local file (fast, WAL, survives a killed
// render). `claude-statusline-rust --flush` copies the rows newer than the
// last flushed id into a shared recorder database in one transaction, then
// records the new high-water mark locally. It is the only path that writes
// to the mount. It never removes a lock: a recorder it cannot open or lock
// within the busy timeout means one stderr line and exit 0, so a Stop hook
// is never blocked and nothing is forced.
//
// Recorder schema (house ADR 0015, proposed): measures(id, ts, room, source,
// buffer, source_id, session_id, prompt_id, kind, key, value, unit, data) with
// UNIQUE(room, source, buffer, source_id), written with INSERT OR IGNORE so a
// flush killed between the insert and the mark cannot write a row twice, and
// a rebuilt room (local ids back at 1, a new buffer id) collides with nothing.

pub(crate) const RECORDER_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub(crate) const FLUSH_BATCH: i64 = 2000;
pub(crate) const FLUSH_MAX_BATCHES: usize = 10;

pub(crate) fn flush_main() {
    let cfg = Config::load();
    let home = home_dir().unwrap_or_default();
    let Some(recorder) = env_nonblank("CSR_RECORDER_DB") else {
        eprintln!("claude-statusline-rust --flush: CSR_RECORDER_DB is not set; nothing to do");
        return;
    };
    // The room is the full sandbox name, never a trimmed tag: two houses can
    // share one repo, and a tag would fold them into one room.
    let Some(room) = env_nonblank("CSR_ROOM").or_else(|| env_nonblank("SANDBOX_NAME")) else {
        eprintln!(
            "claude-statusline-rust --flush: CSR_ROOM (or SANDBOX_NAME) is not set; nothing to do"
        );
        return;
    };
    // Every refusal below exits 0: the flush is a Stop hook's errand and a
    // failed errand must never fail the session. The message is the signal.
    match run_flush(&cfg, &home, &recorder, &room) {
        Ok(r) => {
            println!(
                "claude-statusline-rust --flush: {} new row(s) in {recorder}",
                r.inserted
            );
            if r.left_behind > 0 {
                eprintln!(
                    "claude-statusline-rust --flush: batch cap reached, {} row(s) left for the next flush",
                    r.left_behind
                );
            }
        }
        Err(e) => eprintln!("claude-statusline-rust --flush: skipped: {e}"),
    }
}

/// An environment variable, trimmed, None when unset or blank.
pub(crate) fn env_nonblank(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(crate) fn ctx<T>(r: DbResult<T>, what: &str) -> DbResult<T> {
    r.map_err(|e| -> Box<dyn std::error::Error> { format!("{what}: {e}").into() })
}

/// Columns of the recorder's `measures` table written by the flush, in the
/// order the INSERT binds them.
pub(crate) const MEASURES_COLUMNS: &str =
    "ts, room, source, buffer, source_id, session_id, prompt_id, kind, key, value, unit, data";

/// The recorder's idempotence key, in order. A recorder whose unique index
/// is anything else is refused before a byte is written: with the old
/// three-part key a rebuilt room's rows would be silently dropped again.
pub(crate) const RECORDER_KEY: [&str; 4] = ["room", "source", "buffer", "source_id"];

/// Open the recorder the house way: dotfile lock, rollback journal, a few
/// seconds of patience, no lock removal. An existing `measures` table must
/// carry a unique index on exactly RECORDER_KEY; a missing table is created
/// from the house DDL. Nothing is written to a file that fails the check.
pub(crate) fn open_recorder(path: &str) -> DbResult<Connection> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn =
        Connection::open_with_flags_and_vfs(path, rusqlite::OpenFlags::default(), "unix-dotfile")?;
    conn.busy_timeout(RECORDER_BUSY_TIMEOUT)?;
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'measures'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| -> Box<dyn std::error::Error> {
            let cant_open = matches!(&e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::CannotOpen);
            if cant_open || std::path::Path::new(&format!("{path}-wal")).exists() {
                format!("{e} (a recorder left in WAL mode cannot be read through the dotfile VFS; run PRAGMA journal_mode=DELETE on it)").into()
            } else {
                e.into()
            }
        })?;
    if has_table > 0 {
        check_recorder_key(&conn)?;
    }
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=NORMAL;")?;
    // The house recorder DDL, applied verbatim by every writer (house ADR 0015
    // point 5; the same text lives in the house's commons/schema/recorder.sql).
    let create = "CREATE TABLE IF NOT EXISTS measures (
    id         INTEGER PRIMARY KEY,
    ts         TEXT    NOT NULL,
    room       TEXT    NOT NULL,
    source     TEXT    NOT NULL,
    buffer     TEXT    NOT NULL,
    source_id  INTEGER NOT NULL,
    session_id TEXT,
    prompt_id  TEXT,
    kind       TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    value      REAL,
    unit       TEXT,
    data       TEXT,
    UNIQUE(room, source, buffer, source_id)
);
CREATE INDEX IF NOT EXISTS measures_room_ts        ON measures(room, ts);
CREATE INDEX IF NOT EXISTS measures_kind_key_ts    ON measures(kind, key, ts);
CREATE INDEX IF NOT EXISTS measures_session_prompt ON measures(session_id, prompt_id);";
    conn.execute_batch(create)?;
    Ok(conn)
}

/// Refuse a `measures` table unless it has a unique index on exactly
/// RECORDER_KEY, in order, whole (no WHERE clause), with binary collation,
/// and no other unique index that could make INSERT OR IGNORE drop a row the
/// key would accept: a unique index that neither contains `id` nor covers
/// all four key columns is narrower than the key, and SQLite cannot drop a
/// table constraint, so a hand-migrated old recorder keeps its three-part
/// autoindex beside the new one.
pub(crate) fn check_recorder_key(conn: &Connection) -> DbResult<()> {
    let refuse = |why: &str| -> DbResult<()> {
        Err(format!(
            "measures table needs a whole, binary-collated UNIQUE({}) index and no narrower unique index; {why}; this writer refuses it, move the file aside",
            RECORDER_KEY.join(", ")
        )
        .into())
    };
    // (name, partial) for every unique index, table constraint or named.
    let mut uniques: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare("PRAGMA index_list(measures)")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (name, unique, partial) = row?;
            if unique == 1 {
                uniques.push((name, partial));
            }
        }
    }
    let mut key_found = false;
    for (name, partial) in &uniques {
        let quoted = name.replace('"', "\"\"");
        // index_xinfo: seqno, cid, name (NULL for an expression), desc, coll, key.
        let mut stmt = conn.prepare(&format!("PRAGMA index_xinfo(\"{quoted}\")"))?;
        let mut cols: Vec<(i64, Option<String>, String)> = Vec::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })? {
            let (seq, col, coll, is_key) = row?;
            if is_key == 1 {
                cols.push((seq, col, coll));
            }
        }
        cols.sort_by_key(|c| c.0);
        let names: Vec<Option<&str>> = cols.iter().map(|(_, c, _)| c.as_deref()).collect();
        let exact = names.len() == RECORDER_KEY.len()
            && names
                .iter()
                .zip(RECORDER_KEY.iter())
                .all(|(a, b)| *a == Some(*b));
        if exact {
            if *partial != 0 {
                return refuse(&format!("index {name} is partial"));
            }
            if let Some((_, _, coll)) = cols
                .iter()
                .find(|(_, _, c)| !c.eq_ignore_ascii_case("BINARY"))
            {
                return refuse(&format!("index {name} uses collation {coll}"));
            }
            key_found = true;
            continue;
        }
        let has_id = names.contains(&Some("id"));
        let covers_key = RECORDER_KEY.iter().all(|k| names.contains(&Some(*k)));
        if !has_id && !covers_key {
            return refuse(&format!("unique index {name} is narrower than the key"));
        }
        // A covering index folds rows the key would keep apart if a key
        // column in it compares loosely (room COLLATE NOCASE).
        if covers_key
            && let Some((_, Some(col), coll)) = cols.iter().find(|(_, c, coll)| {
                c.as_deref().is_some_and(|c| RECORDER_KEY.contains(&c))
                    && !coll.eq_ignore_ascii_case("BINARY")
            })
        {
            return refuse(&format!("index {name} uses collation {coll} on {col}"));
        }
    }
    if key_found {
        Ok(())
    } else {
        refuse("no such index")
    }
}

/// The local file's identity: a random id minted the first time the file is
/// used for a flush and kept in it. A rebuilt room starts its local ids at 1
/// again, so (room, source, source_id) alone would collide with the old
/// life's rows; the buffer id tells the lives apart. The guarantee against two
/// first flushes at once (a doubled Stop hook) minting two ids is the
/// singleton primary key: INSERT OR IGNORE on `k = 1` lets exactly one
/// candidate in, and every caller reads that one back. The transaction only
/// keeps the insert and the read together.
pub(crate) fn buffer_id(local: &Connection) -> DbResult<String> {
    local.execute_batch(
        "CREATE TABLE IF NOT EXISTS buffer_identity (
            k       INTEGER PRIMARY KEY CHECK (k = 1),
            id      TEXT NOT NULL,
            created TEXT NOT NULL
         );",
    )?;
    let candidate = mint_id();
    local.execute_batch("BEGIN IMMEDIATE;")?;
    let inner = || -> DbResult<String> {
        local.execute(
            "INSERT OR IGNORE INTO buffer_identity (k, id, created) VALUES (1, ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            [&candidate],
        )?;
        Ok(
            local.query_row("SELECT id FROM buffer_identity WHERE k = 1", [], |r| {
                r.get(0)
            })?,
        )
    };
    match inner() {
        Ok(id) => {
            local.execute_batch("COMMIT;")?;
            Ok(id)
        }
        Err(e) => {
            let _ = local.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// 32 hex characters from the OS random source, or from time and pid when
/// that is unavailable.
pub(crate) fn mint_id() -> String {
    let mut bytes = [0u8; 16];
    let got = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !got {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mix = t ^ ((std::process::id() as u128) << 64);
        bytes = mix.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The local high-water mark per recorder path.
pub(crate) fn ensure_flush_state(local: &Connection) -> DbResult<()> {
    local.execute_batch(
        "CREATE TABLE IF NOT EXISTS flush_state (
            target  TEXT PRIMARY KEY,
            last_id INTEGER NOT NULL,
            ts      TEXT NOT NULL
         );",
    )?;
    Ok(())
}

/// No mark yet is 0; a read error is an error, not a flush-everything-again.
pub(crate) fn last_flushed_id(local: &Connection, target: &str) -> DbResult<i64> {
    ensure_flush_state(local)?;
    match local.query_row(
        "SELECT last_id FROM flush_state WHERE target = ?1",
        [target],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Record the high-water mark. Two flushes at once may finish out of order;
/// the mark only moves forward. Returns the mark now stored.
pub(crate) fn advance_mark(local: &Connection, target: &str, new_last: i64) -> DbResult<i64> {
    ensure_flush_state(local)?;
    local.execute(
        "INSERT INTO flush_state (target, last_id, ts) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(target) DO UPDATE SET
           last_id = MAX(flush_state.last_id, excluded.last_id), ts = excluded.ts",
        rusqlite::params![target, new_last],
    )?;
    last_flushed_id(local, target)
}

/// ISO 8601 UTC with milliseconds. Local rows from before the millisecond
/// format get ".000Z"; a space-separated stamp gets its T; a fraction with
/// no zone gets its Z.
pub(crate) fn recorder_ts(local_ts: &str) -> String {
    let t = local_ts.trim().replacen(' ', "T", 1);
    if t.ends_with('Z') {
        t
    } else if t.len() == 19 {
        format!("{t}.000Z")
    } else {
        format!("{t}Z")
    }
}

/// One local metrics row, as read for the flush.
pub(crate) struct LocalRow {
    pub(crate) id: i64,
    pub(crate) ts: String,
    pub(crate) session_id: Option<String>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) content: Option<i64>,
    pub(crate) always_on_chars: Option<i64>,
    /// The call's numbers; the always-on file list stays out of it and rides
    /// on the always_on change row instead, which keeps the call row small.
    pub(crate) data: serde_json::Value,
    pub(crate) files: serde_json::Value,
}

pub(crate) fn read_local_rows(local: &Connection, after_id: i64) -> DbResult<Vec<LocalRow>> {
    let mut stmt = local.prepare(
        "SELECT id, ts, project, branch, model, session_id, prompt_id, content, in_tokens, out_tokens,
                context_cap, context_pct, cost_usd, rate_5h_pct, rate_5h_resets, rate_7d_pct, rate_7d_resets,
                always_on_chars, always_on_files
         FROM metrics WHERE id > ?1 ORDER BY id LIMIT ?2",
    )?;
    const NAMES: [&str; 19] = [
        "id",
        "ts",
        "project",
        "branch",
        "model",
        "session_id",
        "prompt_id",
        "content",
        "in_tokens",
        "out_tokens",
        "context_cap",
        "context_pct",
        "cost_usd",
        "rate_5h_pct",
        "rate_5h_resets",
        "rate_7d_pct",
        "rate_7d_resets",
        "always_on_chars",
        "always_on_files",
    ];
    let rows = stmt.query_map(rusqlite::params![after_id, FLUSH_BATCH], |r| {
        let id: i64 = r.get(0)?;
        // A hand-edited value of the wrong type names its row, so the flush
        // that it stops can be fixed rather than puzzled over.
        fn col<T: rusqlite::types::FromSql>(
            r: &rusqlite::Row<'_>,
            idx: usize,
            id: i64,
        ) -> rusqlite::Result<T> {
            r.get::<_, T>(idx).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    idx,
                    rusqlite::types::Type::Null,
                    format!("row {id}: {}: {e}", NAMES[idx]).into(),
                )
            })
        }
        let files_text: Option<String> = col(r, 18, id)?;
        let files = files_text
            .as_deref()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
            .unwrap_or(serde_json::Value::Null);
        let data = serde_json::json!({
            "project": col::<Option<String>>(r, 2, id)?,
            "branch": col::<Option<String>>(r, 3, id)?,
            "model": col::<Option<String>>(r, 4, id)?,
            "content": col::<Option<i64>>(r, 7, id)?,
            "in_tokens": col::<Option<i64>>(r, 8, id)?,
            "out_tokens": col::<Option<i64>>(r, 9, id)?,
            "context_cap": col::<Option<i64>>(r, 10, id)?,
            "context_pct": col::<Option<f64>>(r, 11, id)?,
            "cost_usd": col::<Option<f64>>(r, 12, id)?,
            "rate_5h_pct": col::<Option<f64>>(r, 13, id)?,
            "rate_5h_resets": col::<Option<i64>>(r, 14, id)?,
            "rate_7d_pct": col::<Option<f64>>(r, 15, id)?,
            "rate_7d_resets": col::<Option<i64>>(r, 16, id)?,
            "always_on_chars": col::<Option<i64>>(r, 17, id)?,
        });
        Ok(LocalRow {
            id,
            ts: col(r, 1, id)?,
            session_id: col(r, 5, id)?,
            prompt_id: col(r, 6, id)?,
            content: col(r, 7, id)?,
            always_on_chars: col(r, 17, id)?,
            data,
            files,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Copy local rows newer than the last flushed id into the recorder. Returns
/// the number of recorder rows actually inserted (a re-flush after a lost
/// mark inserts none). One call row per local row (kind=statusline,
/// key=call, value=content tokens, the numbers as JSON) and, whenever the
/// always-on count differs from the previous local row, one scalar row
/// (kind=always_on, key=chars, the file list as JSON) so the change is a
/// plain series.
/// What a flush did: recorder rows inserted, and whether the batch cap left
/// local rows for the next run.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FlushReport {
    pub(crate) inserted: usize,
    /// Local rows past the batch cap, for the next flush.
    pub(crate) left_behind: i64,
}

pub(crate) fn run_flush(
    cfg: &Config,
    home: &str,
    recorder_path: &str,
    room: &str,
) -> DbResult<FlushReport> {
    let (local_path, _) = metrics_db_path(cfg, home)?;
    let local_what = format!("local metrics file {local_path}");
    // The render waits 50 ms for the local file; the flush is not on a
    // keystroke and opens with the recorder's patience.
    let local = ctx(
        open_metrics_db_with(cfg, home, RECORDER_BUSY_TIMEOUT),
        &local_what,
    )?;
    let buffer = ctx(buffer_id(&local), &local_what)?;
    let recorder = ctx(
        open_recorder(recorder_path),
        &format!("recorder {recorder_path}"),
    )?;
    // The mark is keyed by the recorder's real path, so two spellings of one
    // file share one mark.
    let target = std::fs::canonicalize(recorder_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| recorder_path.to_string());
    let mut report = FlushReport::default();
    for batch in 0..=FLUSH_MAX_BATCHES {
        let last = ctx(last_flushed_id(&local, &target), &local_what)?;
        let rows = ctx(read_local_rows(&local, last), &local_what)?;
        if rows.is_empty() {
            break;
        }
        if batch == FLUSH_MAX_BATCHES {
            report.left_behind = ctx(
                local
                    .query_row("SELECT COUNT(*) FROM metrics WHERE id > ?1", [last], |r| {
                        r.get::<_, i64>(0)
                    })
                    .map_err(Into::into),
                &local_what,
            )?;
            break;
        }
        // The always-on value of the row before this batch, for change
        // detection; no row is None, a read error is an error.
        let mut prev_on: Option<i64> = match local.query_row(
            "SELECT id, always_on_chars FROM metrics WHERE id <= ?1 AND always_on_chars IS NOT NULL ORDER BY id DESC LIMIT 1",
            [last],
            |r| {
                let id: i64 = r.get(0)?;
                Ok((id, r.get::<_, i64>(1)))
            },
        ) {
            Ok((_, Ok(v))) => Some(v),
            Ok((id, Err(e))) => {
                return Err(format!("{local_what}: row {id}: always_on_chars: {e}").into());
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(format!("{local_what}: {e}").into()),
        };
        let tx = ctx(
            recorder.unchecked_transaction().map_err(Into::into),
            &format!("recorder {recorder_path}"),
        )?;
        {
            let mut ins = ctx(
                tx.prepare(&format!(
                    "INSERT OR IGNORE INTO measures ({MEASURES_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
                ))
                .map_err(Into::into),
                &format!("recorder {recorder_path}"),
            )?;
            for r in &rows {
                let ts = recorder_ts(&r.ts);
                report.inserted += ctx(
                    ins.execute(rusqlite::params![
                        ts,
                        room,
                        "statusline",
                        buffer,
                        r.id,
                        r.session_id,
                        r.prompt_id,
                        "statusline",
                        "call",
                        r.content.map(|c| c as f64),
                        "tokens",
                        r.data.to_string(),
                    ])
                    .map_err(Into::into),
                    &format!("recorder {recorder_path}"),
                )?;
                if let Some(on) = r.always_on_chars
                    && prev_on != Some(on)
                {
                    report.inserted += ctx(
                        ins.execute(rusqlite::params![
                            ts,
                            room,
                            "statusline.always_on",
                            buffer,
                            r.id,
                            r.session_id,
                            r.prompt_id,
                            "always_on",
                            "chars",
                            on as f64,
                            "chars",
                            r.files.to_string(),
                        ])
                        .map_err(Into::into),
                        &format!("recorder {recorder_path}"),
                    )?;
                    prev_on = Some(on);
                }
            }
        }
        ctx(
            tx.commit().map_err(Into::into),
            &format!("recorder {recorder_path}"),
        )?;
        let new_last = rows.last().map(|r| r.id).unwrap_or(last);
        ctx(advance_mark(&local, &target, new_last), &local_what)?;
        if (rows.len() as i64) < FLUSH_BATCH {
            break;
        }
    }
    Ok(report)
}
