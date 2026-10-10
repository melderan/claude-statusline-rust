//! The project line's `+added -removed` and `api:N%` segments, and where they
//! sit in the one-line drop order. Drives the built binary.
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

/// A payload that renders the project row alone: no context window, no git
/// repository, so one-line mode has nothing else to join.
fn payload(cost: &str) -> String {
    format!(
        r#"{{"version":"2.1.292","session_id":"s","prompt_id":"p","model":{{"display_name":"Fable"}},"workspace":{{"project_dir":"/x"}},"cost":{cost}}}"#
    )
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-tail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Render `stdin` with plain text unless `envs` says otherwise.
fn render(tag: &str, envs: &[(&str, &str)], stdin: &str) -> String {
    use std::io::Write;
    let home = fresh_dir(tag);
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("HOME", &home)
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
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "a render always exits 0");
    let _ = std::fs::remove_dir_all(&home);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The first output line, the project row.
fn project_line(tag: &str, cost: &str) -> String {
    let out = render(tag, &[], &payload(cost));
    out.lines().next().unwrap_or("").to_string()
}

const FULL_COST: &str = r#"{"total_duration_ms":4320000,"total_api_duration_ms":475200,"total_lines_added":288,"total_lines_removed":47}"#;

#[test]
fn both_segments_follow_the_duration() {
    assert_eq!(
        project_line("both", FULL_COST),
        "/x | Fable | CC:2.1.292 | dur:1h12m | +288 -47 | api:11%"
    );
}

#[test]
fn lines_without_api_time() {
    let cost = r#"{"total_duration_ms":4320000,"total_lines_added":3,"total_lines_removed":0}"#;
    assert_eq!(
        project_line("lines", cost),
        "/x | Fable | CC:2.1.292 | dur:1h12m | +3 -0"
    );
}

#[test]
fn one_count_present_reads_the_other_as_zero() {
    let cost = r#"{"total_lines_removed":5}"#;
    assert_eq!(
        project_line("one-count", cost),
        "/x | Fable | CC:2.1.292 | +0 -5"
    );
}

#[test]
fn api_time_without_lines() {
    let cost = r#"{"total_duration_ms":4320000,"total_api_duration_ms":475200}"#;
    assert_eq!(
        project_line("api", cost),
        "/x | Fable | CC:2.1.292 | dur:1h12m | api:11%"
    );
}

#[test]
fn api_share_is_rounded() {
    let cost = r#"{"total_duration_ms":1000,"total_api_duration_ms":995}"#;
    assert!(project_line("round-up", cost).ends_with(" | api:100%"));
    let cost = r#"{"total_duration_ms":1000,"total_api_duration_ms":4}"#;
    assert!(project_line("round-down", cost).ends_with(" | api:0%"));
}

#[test]
fn zero_lines_show_nothing() {
    let cost = r#"{"total_duration_ms":60000,"total_api_duration_ms":0,"total_lines_added":0,"total_lines_removed":0}"#;
    let line = project_line("zero", cost);
    assert!(!line.contains('+'), "{line}");
    assert!(!line.contains('-'), "{line}");
    // Zero API time against a positive wall time is a real 0%, not absent.
    assert_eq!(line, "/x | Fable | CC:2.1.292 | dur:1m00s | api:0%");
}

#[test]
fn zero_wall_time_shows_no_api_share() {
    let cost = r#"{"total_duration_ms":0,"total_api_duration_ms":500}"#;
    let line = project_line("zero-wall", cost);
    assert!(!line.contains("api:"), "{line}");
    assert_eq!(line, "/x | Fable | CC:2.1.292 | dur:0s");
}

#[test]
fn absent_fields_change_nothing() {
    let line = project_line("absent", r#"{"total_duration_ms":4320000}"#);
    assert_eq!(line, "/x | Fable | CC:2.1.292 | dur:1h12m");
    let line = project_line("absent-api", r#"{"total_api_duration_ms":500}"#);
    assert_eq!(line, "/x | Fable | CC:2.1.292");
}

#[test]
fn colour_marks_plus_green_and_minus_red() {
    let out = render("colour", &[("CSR_COLOR", "1")], &payload(FULL_COST));
    let green = "\x1b[38;2;74;222;128m+288\x1b[0m";
    let red = "\x1b[38;2;251;113;133m-47\x1b[0m";
    assert!(out.contains(&format!("{green} {red}")), "{out:?}");
}

/// One-line mode at exactly the width of each shorter row: the pieces go in
/// the order version, duration, lines, api, and the project head stays.
#[test]
fn one_line_drops_duration_then_lines_then_api() {
    let steps = [
        "/x | Fable | CC:2.1.292 | dur:1h12m | +288 -47 | api:11%",
        "/x | Fable | dur:1h12m | +288 -47 | api:11%",
        "/x | Fable | +288 -47 | api:11%",
        "/x | Fable | api:11%",
        "/x | Fable",
    ];
    for expected in steps {
        let cols = expected.chars().count().to_string();
        let out = render(
            "drop",
            &[("CSR_LINES", "one"), ("COLUMNS", &cols)],
            &payload(FULL_COST),
        );
        assert_eq!(out.trim_end(), expected, "at {cols} columns");
    }
}

/// Between those widths nothing is ever shown out of order: a piece that
/// goes earlier being present means every piece that goes later is too.
#[test]
fn one_line_never_keeps_a_piece_its_neighbour_lost() {
    for cols in 10..=60 {
        let out = render(
            "sweep",
            &[("CSR_LINES", "one"), ("COLUMNS", &cols.to_string())],
            &payload(FULL_COST),
        );
        let has = |s: &str| out.contains(s);
        if has("CC:") {
            assert!(has("dur:"), "{cols}: {out}");
        }
        if has("dur:") {
            assert!(has("+288"), "{cols}: {out}");
        }
        if has("+288") {
            assert!(has("api:"), "{cols}: {out}");
        }
    }
}
