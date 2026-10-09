//! The binary as a process: the promises that live in main().
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

const HOOK: &str = r#"{"session_id":"s","prompt_id":"p","model":{"display_name":"Fable"},"workspace":{"project_dir":"/x"},"context_window":{"total_input_tokens":84000,"total_output_tokens":1,"context_window_size":200000,"used_percentage":1,"current_usage":{"cache_read_input_tokens":84000}}}"#;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(
    home: &std::path::Path,
    envs: &[(&str, &str)],
    args: &[&str],
    stdin: &str,
) -> (i32, String, String) {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "120")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write;
        // A program that answers without reading stdin (--version, --help)
        // may exit before this write lands, and a release build usually
        // does; the closed pipe is then the expected outcome, not a failure.
        if let Err(e) = child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
        }
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn locked_shared_db_keeps_the_display_and_warns_once() {
    let dir = fresh_dir("locked");
    let db = dir.join("local.sqlite");
    let dbs = db.to_string_lossy().to_string();
    let (code, unlocked, err) = run(&dir, &[("CSR_METRICS_DB", &dbs)], &[], HOOK);
    assert_eq!(code, 0);
    assert!(unlocked.contains("ctx "), "{unlocked}");
    assert_eq!(err, "", "no warning when the write succeeds");

    std::fs::create_dir_all(format!("{dbs}.lock")).unwrap();
    let (code, locked, err) = run(&dir, &[("CSR_METRICS_DB", &dbs)], &[], HOOK);
    assert_eq!(code, 0, "a held lock never fails the render");
    assert_eq!(
        locked,
        format!("{unlocked}\ndb:locked"),
        "display is the same plus the marker with the lock held"
    );
    assert_eq!(err.lines().count(), 1, "exactly one stderr line: {err:?}");
    assert!(err.contains("locked"), "{err}");
    assert!(
        std::path::Path::new(&format!("{dbs}.lock")).is_dir(),
        "lock untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_needs_its_env_and_a_render_never_flushes() {
    let dir = fresh_dir("flushenv");
    let (code, out, err) = run(&dir, &[], &["--flush"], "");
    assert_eq!(code, 0);
    assert_eq!(out, "");
    assert!(err.contains("CSR_RECORDER_DB is not set"), "{err}");

    let rec = dir.join("recorder.sqlite");
    let recs = rec.to_string_lossy().to_string();
    let (code, out, err) = run(&dir, &[("CSR_RECORDER_DB", &recs)], &["--flush"], "");
    assert_eq!(code, 0);
    assert_eq!(out, "");
    assert!(err.contains("CSR_ROOM"), "{err}");
    assert!(!rec.exists(), "no instance name, no recorder file");

    // A plain render with the flush env set does not flush.
    let (code, out, _) = run(
        &dir,
        &[("CSR_RECORDER_DB", &recs), ("CSR_ROOM", "r")],
        &[],
        HOOK,
    );
    assert_eq!(code, 0);
    assert!(out.contains("ctx "));
    assert!(!rec.exists(), "a render never writes the recorder");

    // The flush does, and reports inserted rows; a blank CSR_ROOM falls back to SANDBOX_NAME.
    let (code, out, err) = run(
        &dir,
        &[
            ("CSR_RECORDER_DB", &recs),
            ("CSR_ROOM", "  "),
            ("SANDBOX_NAME", "name-from-env"),
        ],
        &["--flush"],
        "",
    );
    assert_eq!(code, 0, "{err}");
    // One call row and one always_on row (0 characters: no CLAUDE.md in a scratch HOME).
    assert_eq!(
        out.trim(),
        format!("claude-statusline-rust --flush: 2 new row(s) in {recs}"),
        "{err}"
    );
    assert!(rec.exists());
    let conn = rusqlite::Connection::open_with_flags_and_vfs(
        &rec,
        rusqlite::OpenFlags::default(),
        "unix-dotfile",
    )
    .unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT DISTINCT room FROM measures")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["name-from-env"],
        "blank CSR_ROOM falls back to the full SANDBOX_NAME"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_renders_of_a_new_file_all_write() {
    // PRAGMA journal_mode=WAL on a brand-new file answers BUSY without the
    // busy handler, so concurrent first renders used to skip their rows.
    // Separate connections in one process share SQLite's lock state and do
    // not show it; processes do.
    for round in 0..4 {
        let dir = fresh_dir(&format!("firstlife{round}"));
        let mut children: Vec<std::process::Child> = (0..8)
            .map(|i| {
                let mut cmd = Command::new(BIN);
                cmd.env_clear()
                    .env("HOME", &dir)
                    .env("NO_COLOR", "1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let mut child = cmd.spawn().unwrap();
                let hook = HOOK
                    .replace("\"prompt_id\":\"p\"", &format!("\"prompt_id\":\"p{i}\""))
                    .replace("84000", &format!("{}", 84_000 + i * 100));
                {
                    use std::io::Write;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(hook.as_bytes())
                        .unwrap();
                }
                child
            })
            .collect();
        let mut errs = Vec::new();
        for c in children.drain(..) {
            let out = c.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(0));
            let e = String::from_utf8_lossy(&out.stderr).into_owned();
            if !e.is_empty() {
                errs.push(e);
            }
        }
        let db = dir
            .join(".config")
            .join("dbg")
            .join("statusline-metrics.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(errs.is_empty(), "round {round}: {errs:?}");
        assert_eq!(rows, 8, "round {round}: every first render writes its row");
    }
}

#[test]
fn lines_and_compact_env_reach_the_render() {
    let dir = fresh_dir("linesenv");
    let (_, multi, _) = run(&dir, &[], &[], HOOK);
    assert_eq!(multi.lines().count(), 2, "{multi:?}");
    assert!(
        !multi.contains("compact"),
        "far from the threshold: {multi}"
    );

    let (code, one, _) = run(&dir, &[("CSR_LINES", "one")], &[], HOOK);
    assert_eq!(code, 0);
    assert_eq!(one.lines().count(), 1, "{one:?}");
    assert!(one.starts_with("/x | Fable | ctx "), "{one}");

    let (_, narrow, _) = run(&dir, &[("CSR_LINES", "one"), ("COLUMNS", "20")], &[], HOOK);
    assert_eq!(narrow, "/x | Fable", "the project row alone, never blank");

    let (_, junk, _) = run(&dir, &[("CSR_LINES", "sideways")], &[], HOOK);
    assert_eq!(junk, multi, "an unknown value changes nothing");

    let (_, marked, _) = run(&dir, &[("CSR_COMPACT_RESERVE", "100000")], &[], HOOK);
    assert!(marked.contains("compact!"), "{marked}");
    let (_, off, _) = run(&dir, &[("CSR_COMPACT_RESERVE", "-1")], &[], HOOK);
    assert_eq!(off, multi, "a negative reserve is off");
    let (_, junk, _) = run(&dir, &[("CSR_COMPACT_RESERVE", "lots")], &[], HOOK);
    assert_eq!(junk, multi, "an unparsable reserve is ignored");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn activity_line_renders_from_the_transcript_and_a_missing_one_costs_nothing() {
    let dir = fresh_dir("activity");
    let t = dir.join("session.jsonl");
    std::fs::write(
        &t,
        concat!(
            r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let with = |path: &str| HOOK.replacen('{', &format!(r#"{{"transcript_path":"{path}","#), 1);

    let (code, out, err) = run(&dir, &[], &[], &with(&t.to_string_lossy()));
    assert_eq!(code, 0);
    assert!(out.contains("\ntools: Bash x1"), "{out}");
    assert_eq!(err, "");

    let (code, hidden, _) = run(
        &dir,
        &[("CSR_ACTIVITY", "0")],
        &[],
        &with(&t.to_string_lossy()),
    );
    assert_eq!(code, 0);
    assert!(!hidden.contains("tools:"), "{hidden}");

    let missing = dir.join("missing.jsonl");
    let (code, out, err) = run(&dir, &[], &[], &with(&missing.to_string_lossy()));
    assert_eq!(code, 0, "a missing transcript never fails the render");
    assert!(out.contains("ctx "), "{out}");
    assert!(!out.contains("tools:"), "{out}");
    assert_eq!(err, "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_and_help_answer_without_reading_stdin() {
    let dir = fresh_dir("version");
    let (code, out, err) = run(&dir, &[], &["--version"], "");
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!("claude-statusline-rust {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(err, "");
    let (code, out, _) = run(&dir, &[], &["-V"], "not json");
    assert_eq!(code, 0);
    assert!(out.starts_with("claude-statusline-rust "));
    let (code, out, err) = run(&dir, &[], &["--help"], "");
    assert_eq!(code, 0);
    assert!(
        out.contains("--flush") && out.contains("--version"),
        "{out}"
    );
    assert_eq!(err, "");
    let _ = std::fs::remove_dir_all(&dir);
}
