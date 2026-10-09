//! A shared metrics file (`CSR_METRICS_DB`, dotfile lock, rollback journal):
//! how long a render waits for it, and that the first renders of a new one
//! all write their rows.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline-rust");

fn hook(i: usize) -> String {
    format!(
        r#"{{"session_id":"s","prompt_id":"p{i}","model":{{"display_name":"Fable"}},"workspace":{{"project_dir":"/x"}},"context_window":{{"total_input_tokens":{},"total_output_tokens":1,"context_window_size":200000,"used_percentage":1,"current_usage":{{"cache_read_input_tokens":84000}}}}}}"#,
        84_000 + i * 100
    )
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("csr-shared-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn spawn(home: &std::path::Path, db: &str, stdin: &str) -> Child {
    let mut child = Command::new(BIN)
        .env_clear()
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("CSR_METRICS_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
    }
    child
}

/// Run one render; exit code, stderr and wall time.
fn render(home: &std::path::Path, db: &str, stdin: &str) -> (i32, String, Duration) {
    let t0 = Instant::now();
    let out = spawn(home, db, stdin).wait_with_output().unwrap();
    let took = t0.elapsed();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        took,
    )
}

/// Rows in the metrics table; 0 when no render got far enough to create it.
fn rows(db: &str) -> i64 {
    let conn = rusqlite::Connection::open_with_flags_and_vfs(
        db,
        rusqlite::OpenFlags::default(),
        "unix-dotfile",
    )
    .unwrap();
    match conn.query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0)) {
        Ok(n) => n,
        Err(e) if e.to_string().contains("no such table") => 0,
        Err(e) => panic!("counting rows: {e}"),
    }
}

/// The fastest of five renders under a held lock. Load on the machine only
/// adds time, so the minimum is the stable measure of the program's own wait.
fn fastest_locked_render(home: &std::path::Path, db: &str) -> Duration {
    (0..5)
        .map(|i| {
            let (code, err, took) = render(home, db, &hook(100 + i));
            assert_eq!(code, 0, "a held lock never fails the render");
            assert_eq!(err.lines().count(), 1, "one stderr line: {err:?}");
            assert!(err.contains("locked"), "{err}");
            took
        })
        .min()
        .unwrap()
}

// The render waits 50 ms for a lock; under a held lock the fastest of five
// renders ends in about 55 ms, process start included. 120 ms leaves room
// for a slow machine and fails if that patience triples (about 155 ms).
const LOCKED_RENDER_BOUND: Duration = Duration::from_millis(120);

#[test]
fn a_render_under_a_held_lock_gives_up_within_the_render_patience() {
    let dir = fresh_dir("held");
    let db = dir.join("m.db").to_string_lossy().to_string();
    let (code, err, _) = render(&dir, &db, &hook(0));
    assert_eq!((code, err.as_str()), (0, ""), "the file is created");
    let lock = format!("{db}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let took = fastest_locked_render(&dir, &db);
    assert!(
        took < LOCKED_RENDER_BOUND,
        "a render on an existing shared file waited {took:?}"
    );
    assert!(std::path::Path::new(&lock).is_dir(), "lock untouched");
    std::fs::remove_dir(&lock).unwrap();
    assert_eq!(rows(&db), 1, "no row written under the held lock");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_old_empty_file_under_a_held_lock_is_not_waited_on_as_new() {
    // A first opener killed mid-creation can leave an empty file and its
    // lock. Renders on it must not wait the brand-new-file patience forever.
    let dir = fresh_dir("oldempty");
    let db = dir.join("m.db").to_string_lossy().to_string();
    let f = std::fs::File::create(&db).unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(f);
    let lock = format!("{db}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let took = fastest_locked_render(&dir, &db);
    assert!(took < LOCKED_RENDER_BOUND, "waited {took:?}");
    assert!(std::path::Path::new(&lock).is_dir(), "lock untouched");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_render_of_a_brand_new_file_waits_longer_but_bounded() {
    // No file yet and a lock held throughout: the render waits the
    // brand-new-file patience (1 s per lock) and then gives up, once.
    let dir = fresh_dir("newheld");
    let db = dir.join("m.db").to_string_lossy().to_string();
    let lock = format!("{db}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let (code, err, took) = render(&dir, &db, &hook(0));
    assert_eq!(code, 0, "a held lock never fails the render");
    assert_eq!(err.lines().count(), 1, "one stderr line: {err:?}");
    assert!(err.contains("locked"), "{err}");
    assert!(
        took >= Duration::from_millis(900),
        "a brand-new file gets the longer patience, waited only {took:?}"
    );
    assert!(
        took < Duration::from_secs(4),
        "the longer patience is bounded, waited {took:?}"
    );
    assert!(std::path::Path::new(&lock).is_dir(), "lock untouched");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_renders_of_a_new_shared_file_all_write() {
    // Eight renders start together on a file that does not exist yet. The
    // first to get the lock creates the table; the others used to give up
    // after 50 ms and skip their rows (about one in a hundred on local disk,
    // one in six on a mount with slow fsync). A lock held while they start
    // makes every one of them a first render and makes the race certain:
    // with only 50 ms of patience all eight would skip. It is released well
    // inside the 1 s a first render waits.
    for round in 0..3 {
        let dir = fresh_dir(&format!("first{round}"));
        let db = dir.join("m.db").to_string_lossy().to_string();
        let lock = format!("{db}.lock");
        std::fs::create_dir_all(&lock).unwrap();
        let children: Vec<Child> = (0..8).map(|i| spawn(&dir, &db, &hook(i))).collect();
        std::thread::sleep(Duration::from_millis(300));
        std::fs::remove_dir(&lock).unwrap();
        let errs: Vec<String> = children
            .into_iter()
            .map(|c| {
                let out = c.wait_with_output().unwrap();
                assert_eq!(out.status.code(), Some(0));
                String::from_utf8_lossy(&out.stderr).into_owned()
            })
            .filter(|e| !e.is_empty())
            .collect();
        let n = rows(&db);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(errs.is_empty(), "round {round}: {errs:?}");
        assert_eq!(n, 8, "round {round}: every first render writes its row");
    }
}
