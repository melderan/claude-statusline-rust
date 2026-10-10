//! Kept rows: rows a render could not write to a busy shared file wait in
//! files of their own and are written, once, by a later render.
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
fn setup(tag: &str) -> (std::path::PathBuf, String, Config, String) {
    let dir = fresh_dir(tag);
    let home = dir.to_string_lossy().to_string();
    let target = format!("{home}/shared.db");
    let cfg = Config {
        metrics_db: Some(target.clone()),
        ..Config::default()
    };
    (dir, home, cfg, target)
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

fn shared(target: &str) -> Connection {
    open_metrics_at(target, true, RENDER_PATIENCE).unwrap()
}

/// (session, prompt, ts) of every shared row, in id order.
fn shared_rows(target: &str) -> Vec<(String, String, String)> {
    let c = shared(target);
    let mut stmt = c
        .prepare("SELECT session_id, prompt_id, ts FROM metrics ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get(1)?,
            r.get(2)?,
        ))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

fn kept(home: &str, target: &str) -> usize {
    pending(&spill_dir(home, target), usize::MAX).len()
}

fn failed(home: &str, target: &str) -> Vec<serde_json::Value> {
    let dir = spill_dir(home, target).join("failed");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    names
        .iter()
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}

fn prompts(rows: &[(String, String, String)]) -> Vec<&str> {
    rows.iter().map(|r| r.1.as_str()).collect()
}

#[test]
fn the_stamp_matches_sqlite() {
    let c = Connection::open_in_memory().unwrap();
    for secs in [
        0u64,
        951_782_400,
        1_709_164_799,
        1_791_590_400,
        4_102_444_800,
    ] {
        let d = std::time::Duration::from_millis(secs * 1000 + 7);
        let want: String = c
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')",
                [secs as f64 + 0.007],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(utc_stamp(d), want, "{secs}");
    }
}

#[test]
fn rows_kept_under_a_held_lock_are_written_by_the_next_render_in_order() {
    let (dir, home, cfg, target) = setup("held");
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
    assert_eq!(kept(&home, &target), 3);
    let kept_ts: Vec<String> = pending(&spill_dir(&home, &target), 10)
        .iter()
        .map(|p| {
            let k: KeptRow = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            k.row.ts.unwrap()
        })
        .collect();
    std::fs::remove_dir(&lock).unwrap();

    // A render of another session drains everything, oldest first, then
    // writes its own row.
    let (_, busy) = record_render(&cfg, &home, &row("c", "p9", 30));
    assert!(!busy);
    let rows = shared_rows(&target);
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
    assert_eq!(drained_ts, kept_ts, "rows keep the time they were made");
    assert!(rows[3].2 <= rows[4].2);
    assert_eq!(kept(&home, &target), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_between_the_shared_commit_and_the_delete_writes_no_duplicate() {
    let (dir, home, cfg, target) = setup("crash");
    for i in 0..3 {
        keep_row(&home, &target, &row("a", &format!("p{i}"), 10 + i)).unwrap();
    }
    // Step one of the drain commits to the shared file; the process then
    // dies before step two, deleting the files.
    let sd = spill_dir(&home, &target);
    let d = write_with_drain(
        &shared(&target),
        Some(&sd),
        &row("a", "p3", 13),
        DRAIN_BUDGET,
    )
    .unwrap();
    assert_eq!(d.written.len(), 3);
    assert_eq!(kept(&home, &target), 3, "the delete never ran");
    assert_eq!(shared_rows(&target).len(), 4);

    // The next render drains the same three rows again: none lands twice.
    let (_, busy) = record_render(&cfg, &home, &row("a", "p4", 14));
    assert!(!busy);
    assert_eq!(
        prompts(&shared_rows(&target)),
        ["p0", "p1", "p2", "p3", "p4"]
    );
    assert_eq!(kept(&home, &target), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_before_the_shared_commit_keeps_the_rows() {
    let (dir, home, cfg, target) = setup("rollback");
    keep_row(&home, &target, &row("a", "p0", 10)).unwrap();
    // The drain's transaction copies the row and the process dies before
    // COMMIT: SQLite rolls the copy back, and the file is still there.
    {
        let s = shared(&target);
        s.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let p = &pending(&spill_dir(&home, &target), 1)[0];
        let k: KeptRow = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        log_row(&s, &k.row, Some(&k.spill_key)).unwrap();
    }
    assert_eq!(shared_rows(&target).len(), 0);
    assert_eq!(kept(&home, &target), 1, "nothing lost");
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(prompts(&shared_rows(&target)), ["p0", "p1"]);
    assert_eq!(kept(&home, &target), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_drain_stops_at_its_budget_but_always_moves_one_row() {
    let (dir, home, _, target) = setup("budget");
    for i in 0..5 {
        keep_row(&home, &target, &row("a", &format!("p{i}"), 10 + i)).unwrap();
    }
    let sd = spill_dir(&home, &target);
    let d = write_with_drain(
        &shared(&target),
        Some(&sd),
        &row("b", "own", 1),
        std::time::Duration::ZERO,
    )
    .unwrap();
    assert_eq!(
        d.written.len(),
        1,
        "an exhausted budget still copies one row"
    );
    assert_eq!(prompts(&shared_rows(&target)), ["p0", "own"]);
    assert_eq!(pending(&sd, 2).len(), 2, "the batch cap bounds a listing");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_or_unreadable_row_is_set_aside_and_the_rows_behind_it_drain() {
    let (dir, home, cfg, target) = setup("refused");
    // The shared file refuses rows of session "bad" for a reason that is
    // not a lock.
    shared(&target)
        .execute_batch(
            "CREATE TRIGGER no_bad BEFORE INSERT ON metrics WHEN NEW.session_id = 'bad'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    keep_row(&home, &target, &row("a", "p0", 10)).unwrap();
    keep_row(&home, &target, &row("bad", "p0", 10)).unwrap();
    let garbled = spill_dir(&home, &target).join("00000000000000000001-x-garbled.json");
    std::fs::write(&garbled, b"{not json").unwrap();
    keep_row(&home, &target, &row("a", "p1", 11)).unwrap();
    let (_, busy) = record_render(&cfg, &home, &row("c", "own", 1));
    assert!(!busy);
    assert_eq!(
        prompts(&shared_rows(&target)),
        ["p0", "p1", "own"],
        "the rows around the bad ones drain in the same pass"
    );
    let aside = failed(&home, &target);
    assert_eq!(aside.len(), 2, "{aside:?}");
    assert!(
        aside[0]["error"]
            .as_str()
            .unwrap()
            .contains("does not parse")
    );
    assert_eq!(aside[0]["kept"], "{not json");
    assert!(aside[1]["error"].as_str().unwrap().contains("refused"));
    assert!(aside[1]["kept"].as_str().unwrap().contains("\"bad\""));
    assert_eq!(kept(&home, &target), 0, "set aside, not pending");
    // Later drains neither retry them nor stall.
    keep_row(&home, &target, &row("a", "p2", 12)).unwrap();
    record_render(&cfg, &home, &row("c", "own2", 2));
    assert_eq!(
        prompts(&shared_rows(&target)),
        ["p0", "p1", "own", "p2", "own2"]
    );
    assert_eq!(failed(&home, &target).len(), 2, "never deleted");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_row_kept_by_a_session_that_ended_is_drained_by_another() {
    let (dir, home, cfg, target) = setup("ended");
    keep_row(&home, &target, &row("gone", "last", 10)).unwrap();
    let mut none = row("x", "p", 5);
    none.session_id = None;
    let p = keep_row(&home, &target, &none).unwrap();
    assert!(p.to_string_lossy().contains("-_none-"), "{}", p.display());
    record_render(&cfg, &home, &row("new", "p0", 1));
    let rows = shared_rows(&target);
    assert_eq!(prompts(&rows), ["last", "p", "p0"]);
    assert_eq!(rows[0].0, "gone");
    assert_eq!(kept(&home, &target), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_repeated_row_is_dropped_against_the_shared_rows() {
    let (dir, home, cfg, target) = setup("repeat");
    record_render(&cfg, &home, &row("a", "p0", 10));
    // The same numbers as the shared file's last row of session a.
    keep_row(&home, &target, &row("a", "p0", 10)).unwrap();
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(
        prompts(&shared_rows(&target)),
        ["p0", "p1"],
        "a kept row that repeats the shared file's last row is dropped, as a direct write would be"
    );
    assert_eq!(kept(&home, &target), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_kept_for_another_shared_file_stay_put() {
    let (dir, home, cfg, target) = setup("other");
    let elsewhere = format!("{home}/elsewhere.db");
    keep_row(&home, &elsewhere, &row("a", "p0", 10)).unwrap();
    record_render(&cfg, &home, &row("a", "p1", 11));
    assert_eq!(prompts(&shared_rows(&target)), ["p1"]);
    assert_eq!(kept(&home, &elsewhere), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_default_local_file_skips_a_busy_row_and_keeps_nothing() {
    let dir = fresh_dir("local");
    let home = dir.to_string_lossy().to_string();
    let cfg = Config::default();
    record_render(&cfg, &home, &row("a", "p0", 10));
    let holder = Connection::open(local_metrics_path(&home)).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let (_, busy) = record_render(&cfg, &home, &row("a", "p1", 11));
    assert!(busy);
    holder.execute_batch("ROLLBACK;").unwrap();
    assert!(
        !dir.join(".config/dbg/spill").exists(),
        "the local file has no other place to go"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
