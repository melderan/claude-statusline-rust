//! The flush's error wording.
use super::*;

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "csr-flushw-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_bad_local_value_names_its_row_column_and_real_type() {
    let dir = fresh_dir("badtype");
    let h = dir.to_string_lossy().to_string();
    let cfg = Config::default();
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        local
            .execute_batch(
                "INSERT INTO metrics (session_id, in_tokens) VALUES ('s', 1);
                 INSERT INTO metrics (session_id, in_tokens) VALUES ('s', 'x');",
            )
            .unwrap();
    }
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    let err = run_flush(&cfg, &h, &rp, "inst-a").unwrap_err().to_string();
    assert!(
        err.ends_with("row 2: in_tokens: holds a Text value, which is the wrong type"),
        "{err}"
    );
    assert!(!err.contains("Null"), "the real type, not Null: {err}");
    assert!(!err.contains("index"), "no column index: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_wal_beside_a_locked_recorder_earns_no_wal_hint() {
    let dir = fresh_dir("emptywal");
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    drop(open_recorder(&rp).unwrap());
    let lock = format!("{rp}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    std::fs::File::create(format!("{rp}-wal")).unwrap();
    let e = open_recorder(&rp).unwrap_err().to_string();
    assert!(e.contains("locked"), "{e}");
    assert!(
        !e.contains("WAL mode"),
        "an empty -wal is no sign of WAL: {e}"
    );
    // The same lock with a -wal that holds frames does get the hint.
    std::fs::write(format!("{rp}-wal"), b"frames").unwrap();
    let e = open_recorder(&rp).unwrap_err().to_string();
    assert!(e.contains("WAL mode"), "{e}");
    assert!(std::path::Path::new(&lock).is_dir(), "lock untouched");
    let _ = std::fs::remove_dir_all(&dir);
}
