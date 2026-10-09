//! The `db:locked` marker: a render whose metrics row is skipped because the
//! database is busy says so on the status line. Drives the built binary.
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

const HOOK: &str = r#"{"session_id":"s","prompt_id":"p","model":{"display_name":"Fable"},"workspace":{"project_dir":"/x"},"effort":{"level":"high"},"context_window":{"total_input_tokens":84000,"total_output_tokens":1,"context_window_size":200000,"used_percentage":1,"current_usage":{"cache_read_input_tokens":84000}}}"#;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-locked-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run the binary with `HOOK` on stdin; returns (stdout, stderr).
fn render(home: &std::path::Path, envs: &[(&str, &str)]) -> (String, String) {
    use std::io::Write;
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "120")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(HOOK.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "a render always exits 0");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn last_line(out: &str) -> &str {
    out.lines().last().unwrap_or("")
}

#[test]
fn shared_file_held_by_a_lock_shows_the_marker_until_it_clears() {
    let dir = fresh_dir("shared");
    let db = dir.join("shared.sqlite").to_string_lossy().to_string();
    let envs = [("CSR_METRICS_DB", db.as_str())];

    let (out, err) = render(&dir, &envs);
    assert!(!out.contains("db:locked"), "{out}");
    assert_eq!(err, "");

    std::fs::create_dir_all(format!("{db}.lock")).unwrap();
    let (out, err) = render(&dir, &envs);
    assert_eq!(last_line(&out), "db:locked | effort:high", "{out}");
    assert!(err.contains("locked"), "stderr line stays: {err:?}");

    std::fs::remove_dir_all(format!("{db}.lock")).unwrap();
    let (out, _) = render(&dir, &envs);
    assert!(
        !out.contains("db:locked"),
        "marker gone with the lock: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_file_held_by_a_writer_shows_the_marker_and_stays_quiet() {
    let dir = fresh_dir("local");
    // A first render creates the file and its schema.
    let (out, _) = render(&dir, &[]);
    assert!(!out.contains("db:locked"), "{out}");
    let db = dir.join(".config/dbg/statusline-metrics.db");
    assert!(db.exists(), "the first render created {}", db.display());

    // Another connection holds the write lock; a render changes the token
    // counts, so it has a row to insert and cannot.
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let hook_changed = HOOK.replace("84000", "85000");
    let out = {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .env_clear()
            .env("HOME", &dir)
            .env("NO_COLOR", "1")
            .env("COLUMNS", "120")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(hook_changed.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(last_line(&stdout), "db:locked | effort:high", "{stdout}");
    assert_eq!(out.stderr, b"", "the local file never warned on stderr");

    holder.execute_batch("ROLLBACK;").unwrap();
    drop(holder);
    let (out, _) = render(&dir, &[]);
    assert!(!out.contains("db:locked"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_that_is_not_a_lock_has_no_marker() {
    let dir = fresh_dir("other");
    // The shared path is a directory: SQLite cannot open it, and that is not
    // a busy file.
    let db = dir.join("a-directory");
    std::fs::create_dir_all(&db).unwrap();
    let db = db.to_string_lossy().to_string();
    let (out, err) = render(&dir, &[("CSR_METRICS_DB", db.as_str())]);
    assert!(
        err.contains("metrics skipped"),
        "the failure is real: {err:?}"
    );
    assert!(!out.contains("db:locked"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn marker_leads_the_misc_tags_and_outlasts_them_in_one_line_mode() {
    let dir = fresh_dir("oneline");
    let db = dir.join("shared.sqlite").to_string_lossy().to_string();
    std::fs::create_dir_all(format!("{db}.lock")).unwrap();

    let (out, _) = render(&dir, &[("CSR_METRICS_DB", db.as_str())]);
    assert_eq!(
        last_line(&out),
        "db:locked | effort:high",
        "tags follow the marker: {out}"
    );

    // 80 columns hold every piece; at 70 the misc tags go and the marker
    // stays; at 60 the marker goes too, before the ctx tail ("last in/out").
    let one_line = |cols: &str| {
        render(
            &dir,
            &[
                ("CSR_METRICS_DB", db.as_str()),
                ("CSR_LINES", "one"),
                ("COLUMNS", cols),
            ],
        )
        .0
    };
    assert_eq!(
        one_line("80"),
        "/x | Fable | ctx 53% (106k/200k) | last in:84000 out:1 | db:locked | effort:high"
    );
    assert_eq!(
        one_line("70"),
        "/x | Fable | ctx 53% (106k/200k) | last in:84000 out:1 | db:locked"
    );
    assert_eq!(
        one_line("60"),
        "/x | Fable | ctx 53% (106k/200k) | last in:84000 out:1"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
