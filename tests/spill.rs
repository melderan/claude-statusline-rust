//! A shared metrics file that is busy loses no rows: a render that cannot
//! get the lock keeps its row in the local file, and a later render writes
//! it. Drives the built binary.
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

fn hook(session: &str, prompt: &str, tokens: usize) -> String {
    format!(
        r#"{{"session_id":"{session}","prompt_id":"{prompt}","model":{{"display_name":"Fable"}},"workspace":{{"project_dir":"/x"}},"context_window":{{"total_input_tokens":{tokens},"total_output_tokens":1,"context_window_size":200000,"used_percentage":1,"current_usage":{{"cache_read_input_tokens":{tokens}}}}}}}"#
    )
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-spillit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn spawn(home: &std::path::Path, db: &str, stdin: &str) -> Child {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .env_clear()
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("COLUMNS", "120")
        .env("CSR_METRICS_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child
}

/// Wait for a render: (stdout, stderr); it always exits 0.
fn finish(child: Child) -> (String, String) {
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "a render always exits 0");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn render(home: &std::path::Path, db: &str, stdin: &str) -> (String, String) {
    finish(spawn(home, db, stdin))
}

fn rows(db: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open_with_flags_and_vfs(
        db,
        rusqlite::OpenFlags::default(),
        "unix-dotfile",
    )
    .unwrap();
    let mut stmt = conn
        .prepare("SELECT session_id, prompt_id FROM metrics ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn renders_under_a_held_lock_show_the_marker_and_their_rows_land_later() {
    let dir = fresh_dir("held");
    let db = dir.join("m.db").to_string_lossy().to_string();
    let (_, err) = render(&dir, &db, &hook("a", "p0", 1000));
    assert_eq!(err, "");
    let lock = format!("{db}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    for i in 1..=3 {
        let (out, err) = render(&dir, &db, &hook("a", &format!("p{i}"), 1000 + i));
        assert!(out.contains("db:locked"), "{out}");
        assert_eq!(err.lines().count(), 1, "one stderr line: {err:?}");
        assert!(err.contains("locked"), "{err}");
    }
    assert!(std::path::Path::new(&lock).is_dir(), "lock untouched");
    std::fs::remove_dir(&lock).unwrap();
    assert_eq!(rows(&db).len(), 1, "nothing written under the lock");

    let (out, err) = render(&dir, &db, &hook("a", "p4", 1004));
    assert!(!out.contains("db:locked"), "{out}");
    assert_eq!(err, "");
    let prompts: Vec<String> = rows(&db).into_iter().map(|r| r.1).collect();
    assert_eq!(prompts, ["p0", "p1", "p2", "p3", "p4"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_renders_on_a_busy_shared_file_lose_no_rows() {
    // Eight sessions render together, five times. Each round starts under a
    // held lock that is released after twice the render's patience, so the
    // renders that start first are certain to give up; which ones do does
    // not matter. After one quiet render per session, rows equal renders.
    let dir = fresh_dir("storm");
    let db = dir.join("m.db").to_string_lossy().to_string();
    render(&dir, &db, &hook("init", "p", 1));
    let lock = format!("{db}.lock");
    let mut renders = 1;
    let mut gave_up = 0;
    for round in 0..5 {
        std::fs::create_dir_all(&lock).unwrap();
        let children: Vec<Child> = (0..8)
            .map(|s| {
                spawn(
                    &dir,
                    &db,
                    &hook(&format!("s{s}"), &format!("p{round}"), 10 * round + s),
                )
            })
            .collect();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::remove_dir(&lock).unwrap();
        for c in children {
            let (out, _) = finish(c);
            renders += 1;
            gave_up += usize::from(out.contains("db:locked"));
        }
    }
    assert!(gave_up > 0, "the held lock made some renders give up");
    for s in 0..8 {
        render(&dir, &db, &hook(&format!("s{s}"), "last", 9000 + s));
        renders += 1;
    }
    let all = rows(&db);
    let mut keys = all.clone();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), all.len(), "no row twice");
    assert_eq!(
        all.len(),
        renders,
        "every render's row is in the shared file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
