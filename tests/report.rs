//! `--report`: the usage report read from a metrics file that real renders
//! wrote. Drives the built binary; rows are only moved in time afterwards,
//! because a render always stamps the present.
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-report-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn db_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config/dbg/statusline-metrics.db")
}

/// One hook payload: a session, a prompt, a project and the numbers that vary.
struct Hook<'a> {
    session: &'a str,
    prompt: &'a str,
    project: &'a str,
    model: &'a str,
    /// total_input_tokens, which is also what makes a render write a row.
    input: i64,
    output: i64,
    pct: f64,
    cost: f64,
    /// (five-hour %, resets_at, seven-day %, resets_at)
    rate: Option<(f64, i64, f64, i64)>,
}

impl Hook<'_> {
    fn json(&self) -> String {
        let rate = match self.rate {
            Some((a, ar, b, br)) => format!(
                r#","rate_limits":{{"five_hour":{{"used_percentage":{a},"resets_at":{ar}}},"seven_day":{{"used_percentage":{b},"resets_at":{br}}}}}"#
            ),
            None => String::new(),
        };
        format!(
            r#"{{"session_id":"{}","prompt_id":"{}","model":{{"display_name":"{}"}},"workspace":{{"project_dir":"{}"}},"cost":{{"total_cost_usd":{}}},"context_window":{{"total_input_tokens":{},"total_output_tokens":{},"context_window_size":200000,"used_percentage":{}}}{rate}}}"#,
            self.session,
            self.prompt,
            self.model,
            self.project,
            self.cost,
            self.input,
            self.output,
            self.pct
        )
    }
}

fn run(home: &std::path::Path, envs: &[(&str, &str)], args: &[&str], stdin: &str) -> Out {
    use std::io::Write;
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("HOME", home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Err(e) = child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
    let out = child.wait_with_output().unwrap();
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn render(home: &std::path::Path, envs: &[(&str, &str)], hook: &Hook) {
    let out = run(home, envs, &[], &hook.json());
    assert_eq!(out.code, 0, "{}", out.stderr);
}

fn report(home: &std::path::Path, args: &[&str]) -> Out {
    let mut all = vec!["--report"];
    all.extend_from_slice(args);
    run(home, &[], &all, "")
}

/// Move the rows of `session` (oldest row first) to `ages` seconds before
/// `now` (epoch seconds, read once per test so spans come out exact).
fn age_rows(db: &std::path::Path, now: i64, session: &str, ages: &[i64]) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM metrics WHERE session_id = ?1 ORDER BY id")
        .unwrap()
        .query_map([session], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(ids.len(), ages.len(), "rows of {session}");
    for (id, age) in ids.iter().zip(ages) {
        conn.execute(
            "UPDATE metrics SET ts = strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch') WHERE id = ?2",
            rusqlite::params![now - age, id],
        )
        .unwrap();
    }
}

/// SQLite's own `HH:MM` or `MM-DD HH:MM` of the first or last row of a session.
fn stamp(db: &std::path::Path, session: &str, last: bool, with_date: bool) -> String {
    let conn = rusqlite::Connection::open(db).unwrap();
    let fmt = if with_date { "%m-%d %H:%M" } else { "%H:%M" };
    let order = if last { "DESC" } else { "ASC" };
    conn.query_row(
        &format!("SELECT strftime('{fmt}', ts) FROM metrics WHERE session_id = ?1 ORDER BY id {order} LIMIT 1"),
        [session],
        |r| r.get(0),
    )
    .unwrap()
}

fn cells(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// Session "alpha-s" (three renders, two prompts, peak 30%) 3 hours ago,
/// session "beta-s" (one render) 20 minutes ago, "gamma-s" 3 days ago and
/// "delta-s" 10 days ago.
fn seed(home: &std::path::Path) {
    let a = |prompt, input, output, pct, cost, rate| Hook {
        session: "alpha-s",
        prompt,
        project: "/work/alpha",
        model: "ModelA",
        input,
        output,
        pct,
        cost,
        rate,
    };
    let reset5 = 4_102_444_800; // 2100-01-01: far enough that the reset carries its date
    render(
        home,
        &[],
        &a(
            "p1",
            20_000,
            10,
            10.0,
            0.5,
            Some((61.0, reset5, 9.0, reset5)),
        ),
    );
    render(
        home,
        &[],
        &a(
            "p1",
            60_000,
            20,
            30.0,
            1.0,
            Some((42.0, reset5, 18.0, reset5)),
        ),
    );
    render(
        home,
        &[],
        &a(
            "p2",
            40_000,
            30,
            20.0,
            1.5,
            Some((42.4, reset5, 18.0, reset5)),
        ),
    );
    let b = Hook {
        session: "beta-s",
        prompt: "q1",
        project: "/work/beta",
        model: "ModelB",
        input: 10_000,
        output: 5,
        pct: 5.0,
        cost: 2.0,
        rate: None,
    };
    render(home, &[], &b);
    for (session, project) in [("gamma-s", "/work/gamma"), ("delta-s", "/work/delta")] {
        render(
            home,
            &[],
            &Hook {
                session,
                prompt: "r1",
                project,
                model: "ModelC",
                input: 1_500,
                output: 7,
                pct: 1.0,
                cost: 4.0,
                rate: None,
            },
        );
    }
    let db = db_path(home);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    age_rows(&db, now, "alpha-s", &[3 * 3600, 3 * 3600 - 1800, 2 * 3600]);
    age_rows(&db, now, "beta-s", &[20 * 60]);
    age_rows(&db, now, "gamma-s", &[3 * 86_400]);
    age_rows(&db, now, "delta-s", &[10 * 86_400]);
}

#[test]
fn the_default_window_lists_its_sessions_and_the_totals() {
    let home = fresh_dir("default");
    seed(&home);
    let db = db_path(&home);
    let out = report(&home, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stderr, "");
    assert!(!out.stdout.contains('\x1b'), "no colour: {:?}", out.stdout);
    let lines: Vec<&str> = out.stdout.lines().collect();

    assert_eq!(lines[0], "Usage, last 24h (times UTC)");
    assert_eq!(
        cells(lines[1]),
        [
            "start", "end", "dur", "project", "model", "turns", "peak", "ctx", "cost", "last",
            "in/out"
        ]
    );
    let a_start = stamp(&db, "alpha-s", false, false);
    let a_end = stamp(&db, "alpha-s", true, false);
    let b_start = stamp(&db, "beta-s", false, false);
    // Oldest start first, so the newest session is last.
    assert_eq!(
        cells(lines[2]),
        [
            &a_start[..],
            &a_end,
            "1h00m",
            "alpha",
            "ModelA",
            "2",
            "30%",
            "60k/200k",
            "$1.50",
            "40k/30"
        ]
    );
    assert_eq!(
        cells(lines[3]),
        [
            &b_start[..],
            &b_start,
            "0m",
            "beta",
            "ModelB",
            "1",
            "5%",
            "10k/200k",
            "$2.00",
            "10k/5"
        ]
    );
    // Cost is each session's last value, 1.50 + 2.00, not the sum of rows.
    assert_eq!(
        lines[4],
        format!("Total: 2 sessions, 3 turns, $3.50, {a_start} to {b_start} (2h40m)")
    );
    // The 5h reading is the latest row's, the peak is the window's highest;
    // the reset is far away, so it carries its date, month and day only.
    assert_eq!(
        lines[5],
        "Rate limits: 5h 42% (resets 01-01 00:00), 7d 18% (resets 01-01 00:00); 5h peak in window 61%"
    );
    assert_eq!(lines.len(), 6, "{}", out.stdout);
    // The explicit spelling is the same report.
    assert_eq!(report(&home, &["24h"]).stdout, out.stdout);
}

#[test]
fn longer_windows_take_older_sessions_and_show_their_dates() {
    let home = fresh_dir("windows");
    seed(&home);
    let db = db_path(&home);

    let week = report(&home, &["7d"]);
    assert_eq!(week.code, 0, "{}", week.stderr);
    let lines: Vec<&str> = week.stdout.lines().collect();
    assert_eq!(lines[0], "Usage, last 7d (times UTC)");
    let g_start = stamp(&db, "gamma-s", false, true);
    let (g_day, g_time) = g_start.split_once(' ').unwrap();
    assert_eq!(
        cells(lines[2])[..5],
        [g_day, g_time, g_day, g_time, "0m"],
        "{}",
        lines[2]
    );
    assert!(lines[2].contains("gamma"), "{}", lines[2]);
    assert!(
        lines[3].contains("alpha") && lines[4].contains("beta"),
        "{}",
        week.stdout
    );
    assert!(!week.stdout.contains("delta"), "{}", week.stdout);
    assert!(
        lines[5].starts_with("Total: 3 sessions, 4 turns, $7.50, "),
        "{}",
        lines[5]
    );

    let month = report(&home, &["30d"]);
    assert!(month.stdout.contains("delta"), "{}", month.stdout);
    assert!(
        month
            .stdout
            .contains("Total: 4 sessions, 5 turns, $11.50, "),
        "{}",
        month.stdout
    );
}

#[test]
fn a_session_without_prompt_ids_counts_a_turn_per_row() {
    let home = fresh_dir("noprompt");
    for input in [1_000, 2_000, 3_000] {
        // No prompt_id key at all, as an older Claude Code sends.
        let payload = format!(
            r#"{{"session_id":"old","model":{{"display_name":"M"}},"workspace":{{"project_dir":"/w/p"}},"context_window":{{"total_input_tokens":{input},"total_output_tokens":1,"context_window_size":200000,"used_percentage":1}}}}"#
        );
        assert_eq!(run(&home, &[], &[], &payload).code, 0);
    }
    let out = report(&home, &[]);
    assert!(
        out.stdout.contains("Total: 1 session, 3 turns, -, "),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("Rate limits"), "{}", out.stdout);
}

#[test]
fn a_long_project_name_gives_way_and_the_numbers_do_not() {
    let home = fresh_dir("wide");
    let long = "p".repeat(150);
    render(
        &home,
        &[],
        &Hook {
            session: "s",
            prompt: "p",
            project: &format!("/work/{long}"),
            model: "ModelA",
            input: 123_456,
            output: 789,
            pct: 62.0,
            cost: 12.34,
            rate: None,
        },
    );
    let out = report(&home, &[]);
    for line in out.stdout.lines() {
        assert!(
            line.chars().count() <= 100,
            "{} chars: {line}",
            line.chars().count()
        );
    }
    let row = out.stdout.lines().nth(2).unwrap();
    assert!(row.contains('…'), "{row}");
    assert!(row.contains(&"p".repeat(7)), "{row}");
    let tail = cells(row);
    assert_eq!(
        tail[tail.len() - 6..],
        ["ModelA", "1", "62%", "123k/200k", "$12.34", "123k/789"],
        "{row}"
    );
}

#[test]
fn an_empty_window_says_so_and_exits_zero() {
    let home = fresh_dir("empty");
    seed(&home);
    // Everything is older than a day.
    for s in ["alpha-s", "beta-s"] {
        let conn = rusqlite::Connection::open(db_path(&home)).unwrap();
        conn.execute(
            "UPDATE metrics SET ts = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 days') WHERE session_id = ?1",
            [s],
        )
        .unwrap();
    }
    let out = report(&home, &["24h"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stderr, "");
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(
        out.stdout.starts_with("No sessions in the last 24h"),
        "{}",
        out.stdout
    );
}

#[test]
fn a_missing_file_is_one_line_naming_the_path_and_is_not_created() {
    let home = fresh_dir("missing");
    let out = report(&home, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stderr, "");
    assert_eq!(out.stdout.lines().count(), 1, "{}", out.stdout);
    assert!(
        out.stdout
            .contains(&db_path(&home).to_string_lossy().to_string()),
        "{}",
        out.stdout
    );
    assert!(
        !db_path(&home).exists(),
        "a report must not create the file"
    );

    let shared = home
        .join("elsewhere/shared.db")
        .to_string_lossy()
        .to_string();
    let out = run(
        &home,
        &[("CSR_METRICS_DB", &shared)],
        &["--report", "7d"],
        "",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains(&shared), "{}", out.stdout);
    assert!(!std::path::Path::new(&shared).exists());
}

#[test]
fn a_bad_window_prints_the_usage_and_exits_two() {
    let home = fresh_dir("usage");
    seed(&home);
    for args in [&["12h"][..], &["7D"], &[""], &["7d", "30d"], &["--report"]] {
        let out = report(&home, args);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert_eq!(out.stdout, "");
        assert_eq!(
            out.stderr, "usage: claude-statusline-rust --report [24h|7d|30d]\n",
            "{args:?}"
        );
    }
}

#[test]
fn a_busy_file_is_one_stderr_line_and_exit_zero() {
    let home = fresh_dir("busy");
    let db = home.join("shared.db").to_string_lossy().to_string();
    let envs = [("CSR_METRICS_DB", db.as_str())];
    render(
        &home,
        &envs,
        &Hook {
            session: "s",
            prompt: "p",
            project: "/w/x",
            model: "M",
            input: 1_000,
            output: 1,
            pct: 1.0,
            cost: 1.0,
            rate: None,
        },
    );
    // A shared file is locked by a `<file>.lock` directory; the report waits
    // its few seconds for it and never removes it.
    std::fs::create_dir_all(format!("{db}.lock")).unwrap();
    let out = run(&home, &envs, &["--report"], "");
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr.lines().count(), 1, "{}", out.stderr);
    assert!(
        out.stderr
            .starts_with("claude-statusline-rust --report: skipped: "),
        "{}",
        out.stderr
    );
    assert!(
        std::path::Path::new(&format!("{db}.lock")).exists(),
        "the lock stays"
    );

    std::fs::remove_dir_all(format!("{db}.lock")).unwrap();
    let out = run(&home, &envs, &["--report"], "");
    assert!(
        out.stdout.contains("Total: 1 session, 1 turn, $1.00, "),
        "{}",
        out.stdout
    );
}

#[test]
fn the_report_writes_nothing() {
    let home = fresh_dir("readonly");
    seed(&home);
    let count = || -> i64 {
        rusqlite::Connection::open(db_path(&home))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap()
    };
    let before = count();
    assert_eq!(report(&home, &["30d"]).code, 0);
    assert_eq!(count(), before);
}
