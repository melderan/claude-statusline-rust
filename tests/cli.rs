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
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
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
    let db = dir.join("room.sqlite");
    let dbs = db.to_string_lossy().to_string();
    let (code, unlocked, err) = run(&dir, &[("CSR_METRICS_DB", &dbs)], &[], HOOK);
    assert_eq!(code, 0);
    assert!(unlocked.contains("ctx "), "{unlocked}");
    assert_eq!(err, "", "no warning when the write succeeds");

    std::fs::create_dir_all(format!("{dbs}.lock")).unwrap();
    let (code, locked, err) = run(&dir, &[("CSR_METRICS_DB", &dbs)], &[], HOOK);
    assert_eq!(code, 0, "a held lock never fails the render");
    assert_eq!(locked, unlocked, "display identical with the lock held");
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
    assert!(!rec.exists(), "no room, no recorder file");

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
            ("SANDBOX_NAME", "sbx--x--room"),
        ],
        &["--flush"],
        "",
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("1 new row(s)") || out.contains("2 new row(s)"),
        "{out} {err}"
    );
    assert!(rec.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
