//! The spill: rows a render could not write to a busy shared file are kept
//! in the local file and written, once, by a later render.
use super::*;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "csr-spill-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A config whose shared file is `<dir>/shared.db`, with HOME = `dir`.
fn setup(tag: &str) -> (std::path::PathBuf, String, Config) {
    let dir = fresh_dir(tag);
    let home = dir.to_string_lossy().to_string();
    let cfg = Config {
        metrics_db: Some(format!("{home}/shared.db")),
        ..Config::default()
    };
    (dir, home, cfg)
}

fn row(session: &str, prompt: &str, tokens: i64) -> MetricsRow {
    MetricsRow {
        project: Some("/x".into()),
        session_id: Some(session.into()),
        prompt_id: Some(prompt.into()),
        in_tokens: tokens,
        out_tokens: 1,
        context_cap: 200_000,
        context_pct: 1.0,
        ..MetricsRow::default()
    }
}

fn shared(home: &str) -> Connection {
    open_metrics_at(&format!("{home}/shared.db"), true, RENDER_PATIENCE).unwrap()
}

/// (session, prompt, ts) of every shared row, in id order.
fn shared_rows(home: &str) -> Vec<(String, String, String)> {
    let c = shared(home);
    let mut stmt = c
        .prepare("SELECT session_id, prompt_id, ts FROM metrics ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn spill_count(home: &str) -> i64 {
    open_local_metrics(home)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM metrics_spill", [], |r| r.get(0))
        .unwrap()
}

fn prompts(rows: &[(String, String, String)]) -> Vec<&str> {
    rows.iter().map(|r| r.1.as_str()).collect()
}

#[test]
fn rows_skipped_under_a_held_lock_are_written_by_the_next_render_in_order() {
    let (dir, home, cfg) = setup("held");
    let target = format!("{home}/shared.db");
    let (conn, busy) = record_render(&cfg, &home, &row("a", "p0", 10));
    assert!(conn.is_some() && !busy);
    drop(conn);

    let lock = format!("{target}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    for (s, p, t) in [("a", "p1", 11), ("b", "p1", 21), ("a", "p2", 12)] {
        let (conn, busy) = record_render(&cfg, &home, &row(s, p, t));
        assert!(conn.is_none(), "the shared file is locked");
        assert!(busy, "the render that could not write says so");
    }
    assert_eq!(spill_count(&home), 3);
    let spilled_ts: Vec<String> = read_spill(&open_local_metrics(&home).unwrap(), &target, 10)
        .unwrap()
        .into_iter()
        .map(|s| s.row.ts.unwrap())
        .collect();
    std::fs::remove_dir(&lock).unwrap();

    // A render of another session drains everything, oldest first, then
    // writes its own row.
    let (_, busy) = record_render(&cfg, &home, &row("c", "p9", 30));
    assert!(!busy);
    let rows = shared_rows(&home);
    let order: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
    assert_eq!(
        order,
        [
            ("a", "p0"),
            ("a", "p1"),
            ("b", "p1"),
            ("a", "p2"),
            ("c", "p9")
        ]
    );
    let drained_ts: Vec<String> = rows[1..4].iter().map(|r| r.2.clone()).collect();
    assert_eq!(drained_ts, spilled_ts, "rows keep the time they were made");
    assert!(rows[3].2 <= rows[4].2);
    assert_eq!(spill_count(&home), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_between_the_shared_commit_and_the_spill_delete_writes_no_duplicate() {
    let (dir, home, cfg) = setup("crash");
    let target = format!("{home}/shared.db");
    let local = open_local_metrics(&home).unwrap();
    for i in 0..3 {
        spill_row(&local, &target, &row("a", &format!("p{i}"), 10 + i)).unwrap();
    }
    // Step one of the drain commits to the shared file; the process then
    // dies before step two, the delete from the spill.
    let spilled = read_spill(&local, &target, DRAIN_BATCH).unwrap();
    let n = write_with_drain(&shared(&home), &spilled, &row("a", "p3", 13), DRAIN_BUDGET).unwrap();
    assert_eq!(n, 3);
    assert_eq!(spill_count(&home), 3, "the delete never ran");
    assert_eq!(shared_rows(&home).len(), 4);

    // The next render drains the same three rows again: none lands twice.
    let (_, busy) = record_render(&cfg, &home, &row("a", "p4", 14));
    assert!(!busy);
    assert_eq!(prompts(&shared_rows(&home)), ["p0", "p1", "p2", "p3", "p4"]);
    assert_eq!(spill_count(&home), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_before_the_shared_commit_keeps_the_rows_in_the_spill() {
    let (dir, home, cfg) = setup("rollback");
    let target = format!("{home}/shared.db");
    let local = open_local_metrics(&home).unwrap();
    spill_row(&local, &target, &row("a", "p0", 10)).unwrap();
    // The drain's transaction is opened and copies the row, and the process
    // dies before COMMIT: SQLite rolls the copy back.
    {
        let s = shared(&home);
        s.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let spilled = read_spill(&local, &target, DRAIN_BATCH).unwrap();
        log_row(&s, &spilled[0].row, Some(&spilled[0].spill_key)).unwrap();
    }
    assert_eq!(shared_rows(&home).len(), 0);
    assert_eq!(spill_count(&home), 1, "nothing lost");
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(prompts(&shared_rows(&home)), ["p0", "p1"]);
    assert_eq!(spill_count(&home), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_drain_stops_at_its_budget_but_always_moves_one_row() {
    let (dir, home, _) = setup("budget");
    let target = format!("{home}/shared.db");
    let local = open_local_metrics(&home).unwrap();
    for i in 0..5 {
        spill_row(&local, &target, &row("a", &format!("p{i}"), 10 + i)).unwrap();
    }
    let spilled = read_spill(&local, &target, DRAIN_BATCH).unwrap();
    let n = write_with_drain(
        &shared(&home),
        &spilled,
        &row("b", "own", 1),
        std::time::Duration::ZERO,
    )
    .unwrap();
    assert_eq!(n, 1, "an exhausted budget still copies one row");
    assert_eq!(prompts(&shared_rows(&home)), ["p0", "own"]);
    // The batch cap bounds what one render reads.
    assert_eq!(read_spill(&local, &target, 2).unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_spilled_row_the_shared_file_refuses_never_costs_the_render_its_row() {
    let (dir, home, cfg) = setup("refused");
    let target = format!("{home}/shared.db");
    // The shared file refuses rows of session "bad" for a reason that is
    // not a lock.
    shared(&home)
        .execute_batch(
            "CREATE TRIGGER no_bad BEFORE INSERT ON metrics WHEN NEW.session_id = 'bad'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    let local = open_local_metrics(&home).unwrap();
    spill_row(&local, &target, &row("a", "p0", 10)).unwrap();
    spill_row(&local, &target, &row("bad", "p0", 10)).unwrap();
    let (_, busy) = record_render(&cfg, &home, &row("c", "own", 1));
    assert!(!busy);
    assert_eq!(
        prompts(&shared_rows(&home)),
        ["own"],
        "the drain is undone, the row is not"
    );
    assert_eq!(
        spill_count(&home),
        2,
        "nothing deleted that was not written"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_repeated_row_is_spilled_once_and_drained_against_the_shared_rows() {
    let (dir, home, cfg) = setup("repeat");
    let target = format!("{home}/shared.db");
    record_render(&cfg, &home, &row("a", "p0", 10));
    let local = open_local_metrics(&home).unwrap();
    // The same numbers as the shared file's last row of session a, twice.
    spill_row(&local, &target, &row("a", "p0", 10)).unwrap();
    spill_row(&local, &target, &row("a", "p0", 10)).unwrap();
    assert_eq!(
        spill_count(&home),
        1,
        "a repeat of the last spilled row is dropped"
    );
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(
        prompts(&shared_rows(&home)),
        ["p0", "p1"],
        "a spilled row that repeats the shared file's last row is dropped, as a direct write would be"
    );
    assert_eq!(spill_count(&home), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_spilled_for_another_shared_file_stay_put() {
    let (dir, home, cfg) = setup("other");
    let local = open_local_metrics(&home).unwrap();
    spill_row(&local, &format!("{home}/elsewhere.db"), &row("a", "p0", 10)).unwrap();
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(prompts(&shared_rows(&home)), ["p1"]);
    assert_eq!(spill_count(&home), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_local_file_skips_a_busy_row_and_keeps_no_spill() {
    let dir = fresh_dir("local");
    let home = dir.to_string_lossy().to_string();
    let cfg = Config::default();
    record_render(&cfg, &home, &row("a", "p0", 10));
    let holder = Connection::open(local_metrics_path(&home)).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let (_, busy) = record_render(&cfg, &home, &row("a", "p1", 11));
    assert!(busy);
    holder.execute_batch("ROLLBACK;").unwrap();
    let has: i64 = holder
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'metrics_spill'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has, 0, "the local file has no other place to go");
    let _ = std::fs::remove_dir_all(&dir);
}
