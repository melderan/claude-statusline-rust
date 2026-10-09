use crate::*;

#[test]
fn mode_thresholds() {
    assert_eq!(pick_mode(0), Mode::Compact);
    assert_eq!(pick_mode(59), Mode::Compact);
    assert_eq!(pick_mode(60), Mode::Standard);
    assert_eq!(pick_mode(200), Mode::Standard);
}

#[test]
fn duration_formatting() {
    assert_eq!(fmt_duration_ms(0), "0s");
    assert_eq!(fmt_duration_ms(5_000), "5s");
    assert_eq!(fmt_duration_ms(65_000), "1m05s");
    assert_eq!(fmt_duration_ms(3_725_000), "1h02m");
    assert_eq!(fmt_duration_ms(7_320_000), "2h02m");
}

#[test]
fn bytes_formatting() {
    assert_eq!(fmt_bytes(0), "-");
    assert_eq!(fmt_bytes(512), "512B");
    assert_eq!(fmt_bytes(1536), "2KB");
    assert_eq!(
        fmt_bytes(25_363),
        "25KB",
        "MEMORY.md-sized index reads as KB, not tokens"
    );
    assert_eq!(fmt_bytes(371_005), "362KB");
    assert_eq!(fmt_bytes(1_048_575), "1.0MB", "never 1024KB");
    assert_eq!(fmt_bytes(2 * 1024 * 1024), "2.0MB");
}

#[test]
fn age_formatting() {
    let (l, _) = fmt_age_secs(0);
    assert_eq!(l, "now");
    let (l, _) = fmt_age_secs(30);
    assert_eq!(l, "now");
    let (l, _) = fmt_age_secs(300);
    assert_eq!(l, "5m");
    let (l, _) = fmt_age_secs(7200);
    assert_eq!(l, "2h");
    let (l, _) = fmt_age_secs(90000);
    assert_eq!(l, "1d");
}

#[test]
fn memory_slug() {
    assert_eq!(
        path_to_memory_slug("/Users/foo/code/bar"),
        "-Users-foo-code-bar"
    );
    assert_eq!(path_to_memory_slug("/"), "-");
    assert_eq!(
        path_to_memory_slug("/home/me/my.app_v2"),
        "-home-me-my-app-v2"
    );
}

#[test]
fn relative_current_semantics() {
    assert_eq!(
        relative_current("/a/b", "/a/b").as_deref(),
        None,
        "same path → no suffix"
    );
    assert_eq!(relative_current("/a/b", "/a/b/c/d").as_deref(), Some("c/d"));
    assert_eq!(
        relative_current("/a/b", "/x/y").as_deref(),
        Some("/x/y"),
        "unrelated → absolute"
    );
}

#[test]
fn computed_ctx_with_baseline() {
    let cu = CurrentUsage {
        input_tokens: Some(1000),
        output_tokens: Some(500),
        cache_read_input_tokens: Some(10_000),
        cache_creation_input_tokens: Some(5_000),
    };
    let pct = computed_ctx_pct(Some(&cu), 200_000).unwrap();
    // (10000+5000+1000+500) + 22600 = 39100 / 200000 = 19.55%
    assert!((pct - 19.55).abs() < 0.01, "got {}", pct);
}

#[test]
fn computed_ctx_none_without_data() {
    assert!(computed_ctx_pct(None, 200_000).is_none());
    let empty = CurrentUsage::default();
    assert!(computed_ctx_pct(Some(&empty), 200_000).is_none());
    assert!(computed_ctx_pct(Some(&empty), 0).is_none());
}

#[test]
fn context_bar_bounds() {
    let b = context_bar(8, -10);
    assert!(b.contains('\u{26C1}'));
    let b = context_bar(8, 999);
    assert!(b.contains('\u{26C1}'));
    // 0% → no filled buckets (only empty color)
    let b = context_bar(4, 0);
    let filled_count = b.matches("\x1b[38;2;74").count();
    assert_eq!(filled_count, 0);
    // 100% → all filled
    let b = context_bar(4, 100);
    assert_eq!(b.matches(EMPTY_BAR).count(), 0);
}

#[test]
fn context_bar_plain_shapes() {
    assert_eq!(context_bar_plain(4, 0), "[....]");
    assert_eq!(context_bar_plain(4, 100), "[####]");
    // 50% of 4 = 2 filled
    assert_eq!(context_bar_plain(4, 50), "[##..]");
    // Out-of-range clamp
    assert_eq!(context_bar_plain(4, 999), "[####]");
    assert_eq!(context_bar_plain(4, -50), "[....]");
}

#[test]
fn config_defaults_are_opt_in() {
    let cfg = Config::default();
    assert!(!cfg.bar, "bar should be opt-in");
    assert!(!cfg.glyphs, "glyphs should be opt-in");
    assert!(cfg.color, "color should be on by default");
    assert_eq!(
        residue_turns(&cfg.residue),
        0,
        "residue line should be opt-in"
    );
}

#[test]
fn residue_config_never_breaks_the_rest() {
    // An out-of-range residue must not fail the whole config and drop bar.
    let cfg: Config = serde_json::from_str(r#"{"bar": true, "residue": 300}"#).unwrap();
    assert!(cfg.bar);
    assert_eq!(residue_turns(&cfg.residue), 10);
    let cfg: Config = serde_json::from_str(r#"{"glyphs": true, "residue": -1}"#).unwrap();
    assert!(cfg.glyphs);
    assert_eq!(residue_turns(&cfg.residue), 0);
    // Not integers either: a float, a string, a huge number, junk, null.
    for (raw, want) in [
        (r#"{"bar": true, "residue": 2.5}"#, 2),
        (r#"{"bar": true, "residue": "4"}"#, 4),
        (r#"{"bar": true, "residue": 99999999999999999999}"#, 10),
        (r#"{"bar": true, "residue": "x"}"#, 0),
        (r#"{"bar": true, "residue": null}"#, 0),
        (r#"{"bar": true, "residue": [6]}"#, 0),
    ] {
        let cfg: Config = serde_json::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert!(cfg.bar, "{raw}: bar must survive");
        assert_eq!(residue_turns(&cfg.residue), want, "{raw}");
    }
    assert_eq!(clamp_residue(6), 6);
    assert_eq!(clamp_residue(i64::MAX), 10);
}

#[test]
fn fmt_delta_shapes() {
    assert_eq!(fmt_delta(0), "+0");
    assert_eq!(fmt_delta(512), "+512");
    assert_eq!(fmt_delta(-900), "-900");
    assert_eq!(fmt_delta(3_140), "+3.1k");
    assert_eq!(fmt_delta(9_940), "+9.9k");
    assert_eq!(fmt_delta(9_999), "+10k", "one shape around 10k");
    assert_eq!(fmt_delta(10_049), "+10k");
    assert_eq!(fmt_delta(84_200), "+84k");
    assert_eq!(fmt_delta(-120_400), "-120k");
}

fn rows(v: &[(&str, i64)]) -> Vec<(String, i64)> {
    v.iter().map(|(k, c)| (k.to_string(), *c)).collect()
}

#[test]
fn turn_deltas_groups_rows_by_prompt() {
    // Newest first. Turn c had three API responses; its final state is 100_000.
    let r = rows(&[
        ("c", 100_000),
        ("c", 97_000),
        ("c", 90_000),
        ("b", 88_000),
        ("a", 84_000),
    ]);
    // Window wider than the session: first delta is the launch cost from 0.
    assert_eq!(turn_deltas(&r, 6), vec![84_000, 4_000, 12_000]);
    // Window of 2: oldest turn is the baseline, not shown.
    assert_eq!(turn_deltas(&r, 2), vec![4_000, 12_000]);
    assert_eq!(turn_deltas(&r, 1), vec![12_000]);
    assert!(turn_deltas(&r, 0).is_empty());
    assert!(turn_deltas(&[], 5).is_empty());
}

#[test]
fn turn_deltas_show_compaction_as_negative() {
    let r = rows(&[("c", 30_000), ("b", 150_000), ("a", 84_000)]);
    assert_eq!(turn_deltas(&r, 10), vec![84_000, 66_000, -120_000]);
}

#[test]
fn schema_migrates_old_table_and_residue_reads_back() {
    let conn = Connection::open_in_memory().unwrap();
    // A table as the previous release created it: no session, prompt or content.
    conn.execute_batch(
        "CREATE TABLE metrics (
            id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, project TEXT, branch TEXT,
            model TEXT, in_tokens INTEGER, out_tokens INTEGER, context_cap INTEGER,
            context_pct REAL, cost_usd REAL, rate_5h_pct REAL, rate_5h_resets INTEGER,
            rate_7d_pct REAL, rate_7d_resets INTEGER);
         INSERT INTO metrics (project, in_tokens, out_tokens) VALUES ('old', 1, 1);",
    )
    .unwrap();
    ensure_schema(&conn).unwrap();
    ensure_schema(&conn).unwrap(); // idempotent
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(metrics)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for want in [
        "session_id",
        "prompt_id",
        "content",
        "always_on_chars",
        "always_on_files",
    ] {
        assert!(cols.iter().any(|c| c == want), "missing column {want}");
    }

    let mut turn = 0;
    let mut log = |prompt: &str, content: i64, in_t: i64| {
        turn += 1;
        log_metrics(
            &conn,
            "proj",
            None,
            Some("Fable"),
            Some("s1"),
            Some(prompt),
            Some(content),
            in_t,
            turn,
            200_000,
            10.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    };
    log("p1", 84_000, 84_000);
    log("p2", 88_000, 172_000);
    log("p3", 90_000, 262_000);
    log("p3", 100_000, 362_000);
    // Identical update: deduplicated, no new row.
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
        .unwrap();
    log_metrics(
        &conn,
        "proj",
        None,
        Some("Fable"),
        Some("s1"),
        Some("p3"),
        Some(100_000),
        362_000,
        turn,
        200_000,
        10.0,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "unchanged update must not add a row");

    assert_eq!(
        residue_deltas(&conn, "s1", 10).unwrap(),
        vec![84_000, 4_000, 12_000]
    );
    assert_eq!(residue_deltas(&conn, "s1", 2).unwrap(), vec![4_000, 12_000]);
    assert!(residue_deltas(&conn, "other", 5).unwrap().is_empty());

    // A turn with many API responses must not push older turns out of the
    // window (a row-based limit used to).
    for i in 1..=200_i64 {
        log_metrics(
            &conn,
            "proj",
            None,
            Some("Fable"),
            Some("s1"),
            Some("p4"),
            Some(100_000 + i * 10),
            362_000 + i * 10,
            1_000 + i,
            200_000,
            10.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    assert_eq!(residue_deltas(&conn, "s1", 1).unwrap(), vec![2_000]);
    assert_eq!(
        residue_deltas(&conn, "s1", 4).unwrap(),
        vec![84_000, 4_000, 12_000, 2_000]
    );

    // Rows without a prompt_id (older Claude Code) are one turn each.
    log_metrics(
        &conn,
        "proj",
        None,
        None,
        Some("s2"),
        None,
        Some(50_000),
        1,
        1,
        200_000,
        1.0,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    log_metrics(
        &conn,
        "proj",
        None,
        None,
        Some("s2"),
        None,
        Some(53_000),
        2,
        2,
        200_000,
        1.0,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(residue_deltas(&conn, "s2", 5).unwrap(), vec![50_000, 3_000]);
}

#[test]
fn memory_bytes_walks_subdirectories() {
    let root = std::env::temp_dir().join(format!(
        "csr-mem-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let topics = root.join("topics").join("deep");
    std::fs::create_dir_all(&topics).unwrap();
    std::fs::write(root.join("MEMORY.md"), vec![b'x'; 100]).unwrap();
    std::fs::write(root.join("a.md"), vec![b'x'; 10]).unwrap();
    std::fs::write(root.join("notes.txt"), vec![b'x'; 1000]).unwrap();
    std::fs::write(topics.join("b.md"), vec![b'x'; 20]).unwrap();
    // A nested MEMORY.md is an ordinary file, not the index.
    std::fs::write(topics.join("MEMORY.md"), vec![b'x'; 30]).unwrap();
    let (idx, other) = memory_bytes_in(&root);
    assert_eq!(idx, 100);
    assert_eq!(other, 60, "a.md + topics/deep/b.md + topics/deep/MEMORY.md");
    assert_eq!(memory_bytes_in(&root.join("missing")), (0, 0));

    // Symlinks: a linked file counts, a linked directory is walked, and a
    // link back to the root neither loops nor double-counts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root
            .join("..")
            .join(format!("csr-mem-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("far.md"), vec![b'x'; 7]).unwrap();
        symlink(outside.join("far.md"), root.join("link.md")).unwrap();
        symlink(&outside, root.join("linked-dir")).unwrap();
        symlink(&root, topics.join("loop")).unwrap();
        let (idx, other) = memory_bytes_in(&root);
        assert_eq!(idx, 100);
        assert_eq!(
            other,
            60 + 7,
            "link.md and linked-dir/far.md are one file, counted once"
        );
        // The memory directory itself may be a symlink (a common layout).
        let link_to_root = outside.join("memory");
        symlink(&root, &link_to_root).unwrap();
        assert_eq!(memory_bytes_in(&link_to_root), (100, 67));
        let _ = std::fs::remove_dir_all(&outside);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn claude_md_import_syntax() {
    let text = "See @/abs/file.md and @~/home.md here\n@./rel.md\n  @../up.md,\nmail me@example.com or @handle\n```\n@/in/fence.md\n```\n@docs/guide.md.\n@NOTES.md\n~~~\n@/in/tilde/fence.md\n~~~\nuse `@/in/code.md` not that\n@./é.md @./b.md\n``@/in/double.md``\n````\n```\n@/in/four.md\n````\n    @/indented.md\n`unmatched @./after.md\nsee `code @/in/span.md` here\n";
    assert_eq!(
        claude_md_imports(text),
        vec![
            "/abs/file.md",
            "~/home.md",
            "./rel.md",
            "../up.md",
            "handle",
            "docs/guide.md",
            "NOTES.md",
            "./é.md",
            "./b.md",
            "./after.md"
        ]
    );
}

#[test]
fn code_span_stripping() {
    assert_eq!(strip_code_spans("a `b` c"), "a   c");
    assert_eq!(strip_code_spans("``x `y` z`` w"), "  w");
    assert_eq!(strip_code_spans("`open @./x.md"), "`open @./x.md");
    assert_eq!(strip_code_spans("no code"), "no code");
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "csr-{tag}-{}-{}",
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
fn always_on_counts_the_chain_once() {
    let root = fresh_dir("on");
    let home = root.join("home");
    let proj = root.join("repos").join("app");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    let w = |p: &std::path::Path, text: &str| std::fs::write(p, text).unwrap();
    // User file imports an absolute file; that file imports the user file back (a cycle).
    let shared = root.join("shared.md");
    w(
        &home.join(".claude").join("CLAUDE.md"),
        &format!("@{}\n", shared.display()),
    );
    w(&shared, "shared text\n@./home/.claude/CLAUDE.md\n");
    assert!(
        root.join("home").join(".claude").join("CLAUDE.md").exists(),
        "cycle target exists"
    );
    // Parent-directory CLAUDE.md: relative import, bare import, fenced import, inline code.
    w(
        &root.join("repos").join("CLAUDE.md"),
        "parent\n@./inc.md\n@NOTES.md\n```\n@./ignored.md\n```\nsee `@./ignored.md`\n",
    );
    w(&root.join("repos").join("inc.md"), "12345");
    w(&root.join("repos").join("NOTES.md"), "plain notes");
    w(&root.join("repos").join("ignored.md"), "should not count");
    // Project files; the local one has a non-ASCII name and invalid UTF-8 inside.
    w(&proj.join("CLAUDE.md"), "project\n@./é.md @./b.md\n");
    w(&proj.join("é.md"), "éé"); // 2 chars, 4 bytes
    w(&proj.join("b.md"), "bb");
    w(&proj.join(".claude").join("CLAUDE.md"), "dot");
    std::fs::write(proj.join("CLAUDE.local.md"), b"loc\xffal").unwrap(); // 6 chars after lossy
    // Memory index for this project.
    let mem = home
        .join(".claude")
        .join("projects")
        .join(path_to_memory_slug(&proj.to_string_lossy()))
        .join("memory");
    std::fs::create_dir_all(&mem).unwrap();
    w(&mem.join("MEMORY.md"), "memory index");
    w(&mem.join("other.md"), "not always on");

    let on = always_on(&proj.to_string_lossy(), &home.to_string_lossy());
    let names: Vec<(String, u64)> = on
        .files
        .iter()
        .map(|(p, n)| (p.rsplit('/').next().unwrap().to_string(), *n))
        .collect();
    let _ = std::fs::remove_dir_all(&root);
    let user_len = format!("@{}\n", shared.display()).chars().count() as u64;
    assert_eq!(
        names,
        vec![
            ("CLAUDE.md".to_string(), user_len),
            ("shared.md".to_string(), 38),
            ("CLAUDE.md".to_string(), 69),
            ("inc.md".to_string(), 5),
            ("NOTES.md".to_string(), 11),
            ("CLAUDE.md".to_string(), 24),
            ("é.md".to_string(), 2),
            ("b.md".to_string(), 2),
            ("CLAUDE.md".to_string(), 3),
            ("CLAUDE.local.md".to_string(), 6),
            ("MEMORY.md".to_string(), 12),
        ]
    );
    assert_eq!(on.chars, names.iter().map(|(_, n)| n).sum::<u64>());
    assert_eq!(always_on("", ""), AlwaysOn::default());
}

#[test]
fn always_on_import_depth_is_capped_at_five() {
    let root = fresh_dir("depth");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    // user CLAUDE.md (depth 0) -> c1 -> c2 -> c3 -> c4 -> c5 -> c6
    std::fs::write(home.join(".claude").join("CLAUDE.md"), "@./c1.md").unwrap();
    for i in 1..=6 {
        std::fs::write(
            home.join(".claude").join(format!("c{i}.md")),
            format!("x @./c{}.md", i + 1),
        )
        .unwrap();
    }
    let on = always_on("", &home.to_string_lossy());
    let _ = std::fs::remove_dir_all(&root);
    let names: Vec<&str> = on
        .files
        .iter()
        .map(|(p, _)| p.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["CLAUDE.md", "c1.md", "c2.md", "c3.md", "c4.md", "c5.md"]
    );
}

#[test]
fn always_on_skips_an_oversized_import() {
    let root = fresh_dir("big");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude").join("CLAUDE.md"),
        "@./big.md\n@./ok.md",
    )
    .unwrap();
    std::fs::write(
        home.join(".claude").join("big.md"),
        vec![b'x'; IMPORT_MAX_BYTES as usize + 1],
    )
    .unwrap();
    std::fs::write(home.join(".claude").join("ok.md"), "ok").unwrap();
    let on = always_on("", &home.to_string_lossy());
    let _ = std::fs::remove_dir_all(&root);
    let names: Vec<&str> = on
        .files
        .iter()
        .map(|(p, _)| p.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["CLAUDE.md", "ok.md"],
        "a file past the size cap is skipped"
    );
    assert_eq!(on.chars, 18 + 2);
}

#[cfg(unix)]
#[test]
fn always_on_skips_a_fifo_import() {
    let root = fresh_dir("fifo");
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let fifo = home.join(".claude").join("pipe.md");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(made, "mkfifo available");
    std::fs::write(
        home.join(".claude").join("CLAUDE.md"),
        "@./pipe.md\n@./ok.md",
    )
    .unwrap();
    std::fs::write(home.join(".claude").join("ok.md"), "ok").unwrap();
    // Without the regular-file check the read blocks forever; fail on a
    // deadline instead of hanging the suite.
    let (tx, rx) = std::sync::mpsc::channel();
    let h = home.to_string_lossy().to_string();
    std::thread::spawn(move || {
        let _ = tx.send(always_on("", &h));
    });
    let on = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("always_on hung on a FIFO import");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(on.files.len(), 2);
    assert_eq!(on.chars, 19 + 2);
}

#[test]
fn metrics_db_env_override() {
    let mut cfg = Config {
        metrics_db: Some("/from/file.sqlite".into()),
        ..Config::default()
    };
    cfg.apply_metrics_env(Some("   ".into()));
    assert_eq!(
        cfg.metrics_db.as_deref(),
        Some("/from/file.sqlite"),
        "blank env is not set"
    );
    cfg.apply_metrics_env(None);
    assert_eq!(cfg.metrics_db.as_deref(), Some("/from/file.sqlite"));
    cfg.apply_metrics_env(Some("/from/env.sqlite".into()));
    assert_eq!(cfg.metrics_db.as_deref(), Some("/from/env.sqlite"));
    let mut blank = Config {
        metrics_db: Some("".into()),
        ..Config::default()
    };
    blank.apply_metrics_env(None);
    assert_eq!(blank.metrics_db, None, "blank in the file means unset");
}

#[test]
fn metrics_db_path_resolution() {
    assert_eq!(resolve_metrics_db(None, "/h"), None);
    assert_eq!(resolve_metrics_db(Some(""), "/h"), None);
    assert_eq!(resolve_metrics_db(Some("  "), "/h"), None);
    assert_eq!(
        resolve_metrics_db(Some("~/a/b.sqlite"), "/h").as_deref(),
        Some("/h/a/b.sqlite")
    );
    assert_eq!(resolve_metrics_db(Some("~"), "/h").as_deref(), Some("/h"));
    assert_eq!(
        resolve_metrics_db(Some("~bob/x.sqlite"), "/h").as_deref(),
        Some("/h/~bob/x.sqlite"),
        "~user is a literal relative name"
    );
    assert_eq!(
        resolve_metrics_db(Some("rel.sqlite"), "/h").as_deref(),
        Some("/h/rel.sqlite")
    );
    assert_eq!(
        resolve_metrics_db(Some("/abs/x.sqlite"), "/h").as_deref(),
        Some("/abs/x.sqlite")
    );
}

#[test]
fn open_metrics_db_creates_the_parent_directory() {
    let dir = fresh_dir("home");
    let cfg = Config {
        metrics_db: Some("state/deep/metrics.sqlite".into()),
        ..Config::default()
    };
    let conn = open_metrics_db(&cfg, &dir.to_string_lossy()).unwrap();
    drop(conn);
    assert!(
        dir.join("state")
            .join("deep")
            .join("metrics.sqlite")
            .is_file()
    );
    assert!(open_metrics_db(&cfg, "").is_err(), "no HOME, no metrics");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shared_metrics_db_locks_with_a_dotfile_and_never_removes_one() {
    let dir = fresh_dir("db");
    let path = dir.join("local.sqlite");
    let p = path.to_string_lossy().to_string();
    let lock = format!("{p}.lock");
    let on = AlwaysOn {
        chars: 48_000,
        files: vec![("/x/CLAUDE.md".to_string(), 48_000)],
    };
    let on2 = AlwaysOn {
        chars: 50_000,
        files: vec![("/x/CLAUDE.md".to_string(), 50_000)],
    };
    let log = |conn: &Connection, a: &AlwaysOn| {
        log_metrics(
            conn,
            "proj",
            None,
            None,
            Some("s"),
            Some("p"),
            Some(1000),
            1,
            1,
            200_000,
            1.0,
            None,
            None,
            None,
            Some(a),
        )
    };
    let conn = open_metrics_at(&p, true, RENDER_PATIENCE).unwrap();
    log(&conn, &on).unwrap();

    // Someone else holds the dotfile lock (a slow writer, a reader, a
    // killed process): our write fails, the lock stays, the file is intact.
    std::fs::create_dir_all(&lock).unwrap();
    std::fs::File::open(&lock)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let t0 = std::time::Instant::now();
    assert!(
        log(&conn, &on2).is_err(),
        "a held lock means no row, not a removal"
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(5),
        "bounded by busy_timeout"
    );
    assert!(
        std::path::Path::new(&lock).is_dir(),
        "an hour-old lock is still not ours to remove"
    );
    assert!(
        open_metrics_at(&p, true, RENDER_PATIENCE).is_err(),
        "the schema check needs the lock, so a held lock fails the open"
    );
    assert!(
        std::path::Path::new(&lock).is_dir(),
        "and the failed open did not touch the lock either"
    );
    std::fs::remove_dir_all(&lock).unwrap();

    // The dotfile VFS is in use: the lock appears during a write transaction and goes after.
    conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO metrics (project) VALUES ('x');")
        .unwrap();
    assert!(
        std::path::Path::new(&lock).exists(),
        "dotfile lock held inside the transaction"
    );
    conn.execute_batch("COMMIT;").unwrap();
    assert!(
        !std::path::Path::new(&lock).exists(),
        "lock released at commit"
    );
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");

    // always_on columns round-trip, and a change in them alone writes a new row.
    let count = || -> i64 {
        conn.query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
            .unwrap()
    };
    let before = count();
    for a in [&on, &on, &on2] {
        log(&conn, a).unwrap();
    }
    // The bare 'x' row above belongs to no session, so `on` is still a repeat
    // of this session's last row and writes nothing; the changed always-on
    // is a new row.
    assert_eq!(
        count(),
        before + 1,
        "a repeat of this session's last row is no row; a changed always-on is one"
    );
    let (chars, files): (i64, String) = conn
        .query_row(
            "SELECT always_on_chars, always_on_files FROM metrics ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(chars, 50_000);
    assert!(files.contains("/x/CLAUDE.md"));
    drop(conn);
    assert!(
        !dir.join("local.sqlite-wal").exists(),
        "no WAL beside a shared database"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn log_row(conn: &Connection, prompt: &str, content: i64, n: i64, on: &AlwaysOn) {
    log_metrics(
        conn,
        "proj",
        Some("main"),
        Some("Fable"),
        Some("s1"),
        Some(prompt),
        Some(content),
        content,
        n,
        200_000,
        10.0,
        Some(0.5),
        None,
        None,
        Some(on),
    )
    .unwrap();
}

fn on(chars: u64) -> AlwaysOn {
    AlwaysOn {
        chars,
        files: vec![("/a/CLAUDE.md".to_string(), chars)],
    }
}

#[test]
fn flush_copies_new_rows_once_and_skips_a_held_lock() {
    let dir = fresh_dir("flush");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let cfg = Config::default(); // local file under home/.config/dbg, WAL
    let recorder = dir.join("shared").join("recorder.sqlite");
    let rp = recorder.to_string_lossy().to_string();
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p1", 84_000, 1, &on(100));
        log_row(&local, "p2", 88_000, 2, &on(100));
        log_row(&local, "p3", 90_000, 3, &on(120));
    }
    // First flush: three call rows plus two always_on rows (100, then 120).
    assert_eq!(run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted, 5);
    let rec = open_recorder(&rp).unwrap();
    let count = |sql: &str| -> i64 { rec.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM measures WHERE kind='statusline'"),
        3
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM measures WHERE kind='always_on'"),
        2
    );
    let (ts, room, buffer, value, unit, data): (String, String, String, f64, String, String) =
        rec.query_row(
            "SELECT ts, room, buffer, value, unit, data FROM measures WHERE kind='statusline' ORDER BY source_id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert!(
        ts.ends_with('Z') && ts.contains('.'),
        "ISO UTC with milliseconds: {ts}"
    );
    assert_eq!(room, "inst-a");
    assert_eq!(buffer.len(), 32);
    assert_eq!(value, 90_000.0);
    assert_eq!(unit, "tokens");
    let d: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(d["always_on_chars"], 120);
    assert_eq!(d["model"], "Fable");
    assert!(
        d.get("always_on_files").is_none(),
        "the file list rides on the always_on row"
    );
    let files: String = rec
        .query_row(
            "SELECT data FROM measures WHERE kind='always_on' ORDER BY source_id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(files.contains("/a/CLAUDE.md"));

    // Second flush: nothing new.
    assert_eq!(run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted, 0);
    assert_eq!(count("SELECT COUNT(*) FROM measures"), 5);

    // A flush that lost its mark (killed before the update) inserts nothing twice.
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        local
            .execute("UPDATE flush_state SET last_id = 0", [])
            .unwrap();
    }
    assert_eq!(
        run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted,
        0,
        "reported count is rows inserted"
    );
    assert_eq!(count("SELECT COUNT(*) FROM measures"), 5);

    // New local rows after the mark flush alone (same always-on, so no change row).
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p4", 95_000, 4, &on(120));
    }
    assert_eq!(run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted, 1);
    assert_eq!(count("SELECT COUNT(*) FROM measures"), 6);
    drop(rec);

    // Recorder lock held by someone else: skip, no removal, mark unchanged, message names the recorder.
    let lock = format!("{rp}.lock");
    std::fs::create_dir_all(&lock).unwrap();
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p5", 97_000, 5, &on(120));
    }
    let t0 = std::time::Instant::now();
    let err = run_flush(&cfg, &h, &rp, "inst-a").unwrap_err().to_string();
    assert!(err.starts_with("recorder "), "which file was locked: {err}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        std::path::Path::new(&lock).is_dir(),
        "the lock is not ours to remove"
    );
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        let last: i64 = local
            .query_row("SELECT last_id FROM flush_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(last, 4, "mark unchanged by a skipped flush");
    }
    std::fs::remove_dir_all(&lock).unwrap();
    assert_eq!(run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted, 1);
    assert!(!dir.join("shared").join("recorder.sqlite-wal").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_tells_a_recreated_local_file_apart() {
    // A recreated local file restarts its ids at 1; without the buffer id its
    // rows collided on the unique key and were dropped.
    let dir = fresh_dir("rebuilt");
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    let cfg = Config::default();
    let life = |name: &str, rows: &[(&str, i64)]| -> String {
        let home = dir.join(name);
        std::fs::create_dir_all(&home).unwrap();
        let h = home.to_string_lossy().to_string();
        let local = open_metrics_db(&cfg, &h).unwrap();
        for (i, (p, c)) in rows.iter().enumerate() {
            log_row(&local, p, *c, i as i64 + 1, &on(100));
        }
        h
    };
    let h1 = life("life1", &[("p1", 84_000), ("p2", 88_000), ("p3", 90_000)]);
    let h2 = life("life2", &[("q1", 50_000), ("q2", 52_000)]);
    assert_eq!(run_flush(&cfg, &h1, &rp, "inst-a").unwrap().inserted, 4); // 3 calls + 1 always_on
    assert_eq!(run_flush(&cfg, &h2, &rp, "inst-a").unwrap().inserted, 3); // 2 calls + 1 always_on
    let rec = open_recorder(&rp).unwrap();
    let calls: i64 = rec
        .query_row(
            "SELECT COUNT(*) FROM measures WHERE kind='statusline'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let buffers: i64 = rec
        .query_row("SELECT COUNT(DISTINCT buffer) FROM measures", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(calls, 5, "both lives' rows land");
    assert_eq!(buffers, 2);
    // The buffer id is stable across opens of the same file.
    let local = open_metrics_db(&cfg, &h1).unwrap();
    assert_eq!(buffer_id(&local).unwrap(), buffer_id(&local).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_marks_are_per_recorder_and_the_source_is_the_configured_file() {
    let dir = fresh_dir("marks");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    // The local file is the configured one, not the default path.
    let cfg = Config {
        metrics_db: Some(dir.join("local.sqlite").to_string_lossy().to_string()),
        ..Config::default()
    };
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p1", 84_000, 1, &on(100));
        log_row(&local, "p2", 88_000, 2, &on(100));
    }
    assert!(
        !home.join(".config").join("dbg").exists(),
        "default file untouched"
    );
    let ra = dir.join("a.sqlite").to_string_lossy().to_string();
    let rb = dir.join("b.sqlite").to_string_lossy().to_string();
    assert_eq!(run_flush(&cfg, &h, &ra, "inst-a").unwrap().inserted, 3);
    assert_eq!(
        run_flush(&cfg, &h, &rb, "inst-a").unwrap().inserted,
        3,
        "a second recorder has its own mark"
    );
    assert_eq!(run_flush(&cfg, &h, &ra, "inst-a").unwrap().inserted, 0);
    // Two spellings of one recorder path share one mark.
    let ra2 = dir.join(".").join("a.sqlite").to_string_lossy().to_string();
    assert_eq!(run_flush(&cfg, &h, &ra2, "inst-a").unwrap().inserted, 0);
    let local = open_metrics_db(&cfg, &h).unwrap();
    let marks: i64 = local
        .query_row("SELECT COUNT(*) FROM flush_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(marks, 2, "one mark per recorder file, not per spelling");

    // The mark only moves forward, and a mark that cannot be read is an error.
    assert_eq!(advance_mark(&local, "/t", 5).unwrap(), 5);
    assert_eq!(
        advance_mark(&local, "/t", 3).unwrap(),
        5,
        "a late flush cannot move it back"
    );
    assert_eq!(advance_mark(&local, "/t", 9).unwrap(), 9);
    let broken = Connection::open_in_memory().unwrap();
    broken
        .execute_batch(
            "CREATE TABLE flush_state (target TEXT PRIMARY KEY, last_id TEXT NOT NULL, ts TEXT NOT NULL);
             INSERT INTO flush_state VALUES ('/t', 'bogus', 'x');",
        )
        .unwrap();
    assert!(
        last_flushed_id(&broken, "/t").is_err(),
        "a bad mark is not flush-everything-again"
    );
    assert_eq!(
        last_flushed_id(&broken, "/other").unwrap(),
        0,
        "no mark is 0"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recorder_refuses_a_table_without_buffer() {
    let dir = fresh_dir("oldrec");
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    {
        let conn = Connection::open_with_flags_and_vfs(
            &rp,
            rusqlite::OpenFlags::default(),
            "unix-dotfile",
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE measures (id INTEGER PRIMARY KEY, ts TEXT NOT NULL, room TEXT NOT NULL,
               source TEXT NOT NULL, source_id INTEGER NOT NULL, session_id TEXT, prompt_id TEXT,
               kind TEXT NOT NULL, key TEXT NOT NULL, value REAL, unit TEXT, data TEXT,
               UNIQUE(room, source, source_id));",
        )
        .unwrap();
    }
    let err = open_recorder(&rp).unwrap_err().to_string();
    assert!(err.contains("buffer"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recorder_refuses_the_wrong_unique_key_without_writing() {
    let dir = fresh_dir("wrongkey");
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    {
        let conn = Connection::open_with_flags_and_vfs(
            &rp,
            rusqlite::OpenFlags::default(),
            "unix-dotfile",
        )
        .unwrap();
        // The buffer column is there, the key is the old three-part one.
        conn.execute_batch(
            "CREATE TABLE measures (id INTEGER PRIMARY KEY, ts TEXT NOT NULL, room TEXT NOT NULL,
               source TEXT NOT NULL, buffer TEXT NOT NULL, source_id INTEGER NOT NULL, session_id TEXT,
               prompt_id TEXT, kind TEXT NOT NULL, key TEXT NOT NULL, value REAL, unit TEXT, data TEXT,
               UNIQUE(room, source, source_id));",
        )
        .unwrap();
    }
    let before = std::fs::read(&rp).unwrap();
    let err = open_recorder(&rp).unwrap_err().to_string();
    assert!(
        err.contains("UNIQUE(room, source, buffer, source_id)"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&rp).unwrap(),
        before,
        "a refused file is not modified"
    );
    // And a flush into it is refused by name, too.
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let cfg = Config::default();
    {
        let local = open_metrics_db(&cfg, &home.to_string_lossy()).unwrap();
        log_row(&local, "p1", 84_000, 1, &on(100));
    }
    let err = run_flush(&cfg, &home.to_string_lossy(), &rp, "inst-a")
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("recorder ") && err.contains("UNIQUE"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_reports_rows_left_behind_by_the_batch_cap() {
    let dir = fresh_dir("cap");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let cfg = Config::default();
    let n = FLUSH_BATCH * FLUSH_MAX_BATCHES as i64 + 5;
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        local.execute_batch("BEGIN").unwrap();
        for i in 0..n {
            log_row(&local, "p", 1_000 + i, i, &on(100));
        }
        local.execute_batch("COMMIT").unwrap();
    }
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    let r = run_flush(&cfg, &h, &rp, "inst-a").unwrap();
    assert_eq!(r.left_behind, 5, "the cap must be reported with the count");
    assert_eq!(
        r.inserted as i64,
        FLUSH_BATCH * FLUSH_MAX_BATCHES as i64 + 1
    );
    let r = run_flush(&cfg, &h, &rp, "inst-a").unwrap();
    assert_eq!((r.inserted, r.left_behind), (5, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_treats_an_unreadable_previous_always_on_as_an_error() {
    let dir = fresh_dir("prevon");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let cfg = Config::default();
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p1", 84_000, 1, &on(100));
    }
    assert_eq!(run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted, 2);
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        // SQLite keeps text in an INTEGER column; the previous-value read then fails.
        local
            .execute(
                "UPDATE metrics SET always_on_chars = 'bogus' WHERE id = 1",
                [],
            )
            .unwrap();
        log_row(&local, "p2", 88_000, 2, &on(100));
    }
    let err = run_flush(&cfg, &h, &rp, "inst-a").unwrap_err().to_string();
    assert!(
        err.starts_with("local metrics file ") && err.contains("row 1"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build a recorder file with the given DDL and say whether open_recorder takes it.
fn recorder_shape(tag: &str, ddl: &str) -> Result<(), String> {
    let dir = fresh_dir(tag);
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    {
        let conn = Connection::open_with_flags_and_vfs(
            &rp,
            rusqlite::OpenFlags::default(),
            "unix-dotfile",
        )
        .unwrap();
        conn.execute_batch(ddl).unwrap();
    }
    let before = std::fs::read(&rp).unwrap();
    let r = open_recorder(&rp).map(|_| ()).map_err(|e| e.to_string());
    if r.is_err() {
        assert_eq!(
            std::fs::read(&rp).unwrap(),
            before,
            "{tag}: a refused file is not modified"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    r
}

const COLS: &str =
    "id INTEGER PRIMARY KEY, ts TEXT NOT NULL, room TEXT NOT NULL, source TEXT NOT NULL,
    buffer TEXT NOT NULL, source_id INTEGER NOT NULL, session_id TEXT, prompt_id TEXT,
    kind TEXT NOT NULL, key TEXT NOT NULL, value REAL, unit TEXT, data TEXT";

#[test]
fn recorder_key_check_looks_at_every_unique_index() {
    // The right key as a table constraint, or as a named unique index: accepted.
    recorder_shape(
        "k-ok",
        &format!("CREATE TABLE measures ({COLS}, UNIQUE(room, source, buffer, source_id));"),
    )
    .unwrap();
    recorder_shape(
        "k-named",
        &format!("CREATE TABLE measures ({COLS}); CREATE UNIQUE INDEX k ON measures(room, source, buffer, source_id);"),
    )
    .unwrap();
    // A second unique index that contains id, or covers the key, is harmless.
    recorder_shape(
        "k-extra-id",
        &format!("CREATE TABLE measures ({COLS}, UNIQUE(room, source, buffer, source_id), UNIQUE(id, ts));"),
    )
    .unwrap();
    recorder_shape(
        "k-superset",
        &format!("CREATE TABLE measures ({COLS}, UNIQUE(room, source, buffer, source_id), UNIQUE(room, source, buffer, source_id, kind));"),
    )
    .unwrap();
    // The hand-migrated old recorder: the right index beside the old three-part autoindex.
    let e = recorder_shape(
        "k-narrow-beside",
        &format!("CREATE TABLE measures ({COLS}, UNIQUE(room, source, source_id)); CREATE UNIQUE INDEX k ON measures(room, source, buffer, source_id);"),
    )
    .unwrap_err();
    assert!(e.contains("narrower"), "{e}");
    // A partial index on the right columns dedupes nothing.
    let e = recorder_shape(
        "k-partial",
        &format!("CREATE TABLE measures ({COLS}); CREATE UNIQUE INDEX k ON measures(room, source, buffer, source_id) WHERE kind = 'never';"),
    )
    .unwrap_err();
    assert!(e.contains("partial"), "{e}");
    // Wrong order, wrong width, wrong collation, an expression index, no index.
    assert!(
        recorder_shape(
            "k-order",
            &format!("CREATE TABLE measures ({COLS}, UNIQUE(source, room, buffer, source_id));")
        )
        .is_err()
    );
    assert!(
        recorder_shape(
            "k-five",
            &format!(
                "CREATE TABLE measures ({COLS}, UNIQUE(room, source, buffer, source_id, kind));"
            )
        )
        .is_err()
    );
    let e = recorder_shape(
        "k-nocase",
        &format!("CREATE TABLE measures ({COLS}); CREATE UNIQUE INDEX k ON measures(room COLLATE NOCASE, source, buffer, source_id);"),
    )
    .unwrap_err();
    assert!(e.contains("collation"), "{e}");
    let e = recorder_shape(
        "k-expr",
        &format!("CREATE TABLE measures ({COLS}); CREATE UNIQUE INDEX k ON measures(lower(room), source, buffer, source_id);"),
    )
    .unwrap_err();
    assert!(e.contains("UNIQUE(room, source, buffer, source_id)"), "{e}");
    let e = recorder_shape("k-none", &format!("CREATE TABLE measures ({COLS});")).unwrap_err();
    assert!(e.contains("no such index"), "{e}");
    // A held lock is reported as such, without the WAL hint; a recorder
    // left in WAL mode gets the hint.
    let dir = fresh_dir("k-hints");
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    drop(open_recorder(&rp).unwrap());
    std::fs::create_dir_all(format!("{rp}.lock")).unwrap();
    let e = open_recorder(&rp).unwrap_err().to_string();
    assert!(e.contains("locked") && !e.contains("WAL mode"), "{e}");
    std::fs::remove_dir_all(format!("{rp}.lock")).unwrap();
    {
        let c = Connection::open(&rp).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO measures (ts, room, source, buffer, source_id, kind, key) VALUES ('t','r','s','b',1,'k','v');").unwrap();
    }
    let e = open_recorder(&rp).unwrap_err().to_string();
    assert!(e.contains("WAL mode"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
    // DESC on a key column is still the key; a covering index with a loose collation is not.
    recorder_shape(
        "k-desc",
        &format!("CREATE TABLE measures ({COLS}); CREATE UNIQUE INDEX k ON measures(room, source, buffer DESC, source_id);"),
    )
    .unwrap();
    let e = recorder_shape(
        "k-superset-nocase",
        &format!("CREATE TABLE measures ({COLS}, UNIQUE(room, source, buffer, source_id)); CREATE UNIQUE INDEX k2 ON measures(room COLLATE NOCASE, source, buffer, source_id, kind);"),
    )
    .unwrap_err();
    assert!(e.contains("collation") && e.contains("room"), "{e}");
}

// The brand-new-file race (PRAGMA journal_mode=WAL answering BUSY without
// the busy handler) does not show between connections of one process,
// which share SQLite's lock state; tests/cli.rs reproduces it with
// concurrent processes.

#[test]
fn complete_schema_needs_no_write_lock() {
    let dir = fresh_dir("nolock");
    let path = dir.join("metrics.db").to_string_lossy().to_string();
    drop(open_metrics_at(&path, false, RENDER_PATIENCE).unwrap());
    // Someone holds the write lock; a render must still open a complete file.
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let t0 = std::time::Instant::now();
    let opened = open_metrics_at(&path, false, RENDER_PATIENCE);
    assert!(opened.is_ok(), "{:?}", opened.err().map(|e| e.to_string()));
    assert!(t0.elapsed() < std::time::Duration::from_millis(500));
    holder.execute_batch("COMMIT;").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_names_the_row_of_a_bad_local_value() {
    let dir = fresh_dir("badrow");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let cfg = Config::default();
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        log_row(&local, "p1", 84_000, 1, &on(100));
        log_row(&local, "p2", 88_000, 2, &on(100));
        local
            .execute("UPDATE metrics SET in_tokens = 'x' WHERE id = 2", [])
            .unwrap();
    }
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    let err = run_flush(&cfg, &h, &rp, "inst-a").unwrap_err().to_string();
    assert!(err.contains("row 2") && err.contains("in_tokens"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn buffer_id_is_minted_once_under_concurrent_first_flushes() {
    let dir = fresh_dir("mint");
    let path = dir.join("local.sqlite");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let p = path.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                let c = Connection::open(&p).unwrap();
                c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                b.wait();
                buffer_id(&c).unwrap()
            })
        })
        .collect();
    let ids: std::collections::HashSet<String> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(ids.len(), 1, "eight first flushes, one identity: {ids:?}");
    let c = Connection::open(&path).unwrap();
    let rows: i64 = c
        .query_row("SELECT COUNT(*) FROM buffer_identity", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    assert!(ids.contains(&buffer_id(&c).unwrap()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_walks_every_batch_of_a_long_backlog() {
    let dir = fresh_dir("backlog");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let h = home.to_string_lossy().to_string();
    let cfg = Config::default();
    let n = FLUSH_BATCH + 1;
    {
        let local = open_metrics_db(&cfg, &h).unwrap();
        local.execute_batch("BEGIN").unwrap();
        for i in 0..n {
            log_row(&local, "p", 1_000 + i, i, &on(100));
        }
        local.execute_batch("COMMIT").unwrap();
    }
    let rp = dir.join("recorder.sqlite").to_string_lossy().to_string();
    assert_eq!(
        run_flush(&cfg, &h, &rp, "inst-a").unwrap().inserted as i64,
        n + 1,
        "every local row plus one always_on row, across two batches"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recorder_ts_shapes() {
    assert_eq!(
        recorder_ts("2026-10-02T18:00:00"),
        "2026-10-02T18:00:00.000Z"
    );
    assert_eq!(
        recorder_ts("2026-10-02T18:00:00.123Z"),
        "2026-10-02T18:00:00.123Z"
    );
    assert_eq!(
        recorder_ts("2026-10-02 18:00:00"),
        "2026-10-02T18:00:00.000Z"
    );
    assert_eq!(
        recorder_ts("2026-10-02T18:00:00.123"),
        "2026-10-02T18:00:00.123Z"
    );
}

#[test]
fn mint_id_is_hex_and_unique() {
    let a = mint_id();
    let b = mint_id();
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn fmt_chars_unit() {
    assert_eq!(fmt_chars(512), "512ch");
    assert_eq!(fmt_chars(4_800), "4.8kch");
    assert_eq!(fmt_chars(47_735), "48kch");
}

#[test]
fn truthy_parsing() {
    assert!(truthy("1"));
    assert!(truthy("true"));
    assert!(truthy("TRUE"));
    assert!(truthy("yes"));
    assert!(truthy("on"));
    assert!(!truthy("0"));
    assert!(!truthy("false"));
    assert!(!truthy("no"));
    assert!(!truthy(""));
}

// ── extras: prompt cache, pace, PR, mode tags ──

fn plain() -> Config {
    Config {
        color: false,
        ..Config::default()
    }
}

#[test]
fn cache_segment_warm_shows_hit_ttl_and_clock_expiry() {
    let pc = PromptCache {
        warm: Some(true),
        caching_observed: Some(true),
        ttl: Some("1h".into()),
        expires_at: Some(1_791_242_565),
        misses: Some(0),
        hit_ratio: Some(0.979_871),
        last_miss_cause: None,
        recache_tokens_if_cold: Some(253_799),
    };
    let now = 1_791_242_565 - 42 * 60 - 10;
    assert_eq!(
        cache_segment(&pc, now, &plain()).as_deref(),
        Some("cache 98% warm 1h, cold in 42m (23:22Z)")
    );
}

#[test]
fn cache_segment_cold_names_the_rewarm_cost_and_the_last_miss() {
    let pc = PromptCache {
        warm: Some(false),
        caching_observed: Some(true),
        ttl: None,
        expires_at: None,
        misses: Some(2),
        hit_ratio: Some(0.71),
        last_miss_cause: Some(serde_json::Value::from("system_prompt_changed")),
        recache_tokens_if_cold: Some(253_799),
    };
    assert_eq!(
        cache_segment(&pc, 0, &plain()).as_deref(),
        Some("cache cold 71% (+253k to rewarm) miss:2 (system_prompt_changed)")
    );
}

#[test]
fn cache_segment_is_silent_until_caching_is_observed() {
    let pc = PromptCache {
        warm: Some(false),
        caching_observed: Some(false),
        ..Default::default()
    };
    assert_eq!(cache_segment(&pc, 0, &plain()), None);
    assert_eq!(cache_segment(&PromptCache::default(), 0, &plain()), None);
}

#[test]
fn pace_is_used_over_elapsed_and_quiet_early_in_the_window() {
    // half the 5 h window gone, 50% used: an even spend
    let now = 1_000_000;
    let resets = now + FIVE_HOURS / 2;
    assert_eq!(pace(50.0, resets, FIVE_HOURS, now), Some(1.0));
    // a quarter gone, 50% used: twice the even pace
    let resets = now + FIVE_HOURS * 3 / 4;
    assert_eq!(pace(50.0, resets, FIVE_HOURS, now), Some(2.0));
    // first minute of the window: no number yet
    let resets = now + FIVE_HOURS - 60;
    assert_eq!(pace(1.0, resets, FIVE_HOURS, now), None);
    // a reset already in the past counts as the whole window elapsed
    assert_eq!(pace(80.0, now - 10, SEVEN_DAYS, now), Some(0.8));
    assert_eq!(pace(80.0, now, 0, now), None);
}

#[test]
fn pace_colours_by_threshold() {
    let cfg = Config::default();
    assert!(fmt_pace(0.9, &cfg).contains(GREEN));
    assert!(fmt_pace(1.1, &cfg).contains(AMBER));
    assert!(fmt_pace(1.3, &cfg).contains(ROSE));
    assert_eq!(fmt_pace(1.25, &plain()), " pace 1.2x");
}

#[test]
fn pr_tag_reads_github_and_gitlab_shapes() {
    let pr = Pr {
        number: Some(123),
        review_state: Some("approved".into()),
        kind: None,
    };
    assert_eq!(pr_tag(&pr).as_deref(), Some("PR#123 approved"));
    let mr = Pr {
        number: Some(45),
        review_state: Some("draft".into()),
        kind: Some("mr".into()),
    };
    assert_eq!(pr_tag(&mr).as_deref(), Some("MR!45 draft"));
    assert_eq!(pr_tag(&Pr::default()), None);
    let bare = Pr {
        number: Some(7),
        ..Default::default()
    };
    assert_eq!(pr_tag(&bare).as_deref(), Some("PR#7"));
}

#[test]
fn mode_tags_show_effort_fast_thinking_off_name_and_worktree() {
    let data: Input = serde_json::from_str(
        r#"{"effort":{"level":"high"},"fast_mode":true,"thinking":{"enabled":false},
            "session_name":"a very long session name that keeps on going past the limit",
            "worktree":{"name":"feature-x","path":"/w/feature-x"}}"#,
    )
    .unwrap();
    assert_eq!(
        mode_tags(&data),
        vec![
            "effort:high",
            "fast",
            "think:off",
            "\"a very long session name that k\u{2026}\"",
            "wt:feature-x"
        ]
    );
    // defaults are silent: thinking on, fast off, no names
    let quiet: Input =
        serde_json::from_str(r#"{"thinking":{"enabled":true},"fast_mode":false}"#).unwrap();
    assert!(mode_tags(&quiet).is_empty());
}

#[test]
fn new_hook_fields_parse_from_a_real_shaped_payload() {
    // trimmed from a real Claude Code 2.1.289 hook payload; values changed
    let data: Input = serde_json::from_str(
        r#"{"context_window":{"total_input_tokens":258285,"total_output_tokens":3,"context_window_size":1000000,
            "used_percentage":26,"remaining_percentage":74},
            "exceeds_200k_tokens":true,"fast_mode":false,"effort":{"level":"high"},"thinking":{"enabled":true},
            "prompt_cache":{"warm":true,"caching_observed":true,"ttl":"1h","expires_at":1791242565,"requests":67,
            "misses":0,"expected_rebuilds":0,"hit_ratio":0.9798,"cache_write_tokens":232037,"last_miss_at":null,
            "last_miss_cause":null,"miss_causes":{},"recache_tokens_if_cold":253799},
            "output_style":{"name":"default"},"rate_limits":null,"pr":null,"worktree":null}"#,
    )
    .unwrap();
    assert_eq!(data.exceeds_200k_tokens, Some(true));
    let pc = data.prompt_cache.as_ref().unwrap();
    assert_eq!(pc.warm, Some(true));
    assert_eq!(pc.recache_tokens_if_cold, Some(253_799));
    assert!(data.pr.is_none() && data.worktree.is_none() && data.rate_limits.is_none());
}

#[test]
fn time_helpers() {
    assert_eq!(fmt_hm_utc(1_791_242_565), "23:22Z");
    assert_eq!(fmt_in(0), "now");
    assert_eq!(fmt_in(30), "in 1m");
    assert_eq!(fmt_in(42 * 60), "in 42m");
    assert_eq!(fmt_in(3600 + 5 * 60), "in 1h05m");
    assert_eq!(fmt_in(2 * 86400 + 3 * 3600), "in 2d3h");
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("exactly-ten", 11), "exactly-ten");
    assert_eq!(truncate("abcdefgh", 4), "abc\u{2026}");
}

#[test]
fn miss_cause_reads_the_string_and_the_object_shape() {
    let obj: serde_json::Value = serde_json::from_str(r#"{"causes":["ttl_expired_1h"]}"#).unwrap();
    assert_eq!(miss_cause_text(&obj).as_deref(), Some("ttl_expired_1h"));
    let two: serde_json::Value = serde_json::from_str(r#"{"causes":["a","b"]}"#).unwrap();
    assert_eq!(miss_cause_text(&two).as_deref(), Some("a+b"));
    assert_eq!(
        miss_cause_text(&serde_json::Value::from("plain")).as_deref(),
        Some("plain")
    );
    assert_eq!(miss_cause_text(&serde_json::Value::from("")), None);
    assert_eq!(miss_cause_text(&serde_json::Value::from(7)), None);
    assert_eq!(miss_cause_text(&serde_json::json!({"causes": []})), None);
}

#[test]
fn a_surprising_sub_object_shape_costs_one_segment_not_the_line() {
    // the real 2026-10-06 payload shape for last_miss_cause: an object
    let data: Input = serde_json::from_str(
        r#"{"model":{"display_name":"Fable 5.1"},
            "prompt_cache":{"warm":true,"caching_observed":true,"ttl":"1h","expires_at":1791249359,"misses":1,
            "hit_ratio":0.969,"last_miss_cause":{"causes":["ttl_expired_1h"]},"recache_tokens_if_cold":351352}}"#,
    )
    .unwrap();
    assert_eq!(
        data.model.as_ref().unwrap().display_name.as_deref(),
        Some("Fable 5.1")
    );
    let seg = cache_segment(
        data.prompt_cache.as_ref().unwrap(),
        1791249359 - 600,
        &plain(),
    )
    .unwrap();
    assert_eq!(
        seg,
        "cache 97% warm 1h, cold in 10m (01:15Z) miss:1 (ttl_expired_1h)"
    );
    // a shape nothing here expects: the cache segment is dropped, the model survives
    let data: Input = serde_json::from_str(
        r#"{"model":{"display_name":"Fable 5.1"},"prompt_cache":"nope","pr":[1,2],"effort":{"level":{"x":1}}}"#,
    )
    .unwrap();
    assert_eq!(
        data.model.as_ref().unwrap().display_name.as_deref(),
        Some("Fable 5.1")
    );
    assert!(data.prompt_cache.is_none() && data.pr.is_none() && data.effort.is_none());
}

// ── voice card (claude-code-tts docs/voice-card.md, schema 1) ──

const CARD: &str = r#"{"schema":1,"session":"-Users-me-code-app","persona":"my-persona","backend":"mlx",
  "voice":"some-engine/model-bf16:speaker_a","speed":1.8,"muted":false,"intermediate":true,"mode":"queue",
  "written_at":"2026-10-05T22:50:12Z","claude_tts":"9.48.0"}"#;

#[test]
fn voice_segment_reads_the_card_and_shortens_the_engine_voice() {
    assert_eq!(
        voice_segment(CARD).as_deref(),
        Some("voice: my-persona (speaker_a) 1.8x")
    );
    let piper = r#"{"schema":1,"persona":"my-piper","backend":"piper","voice":"en_US-demo-medium","speed":2.0,"muted":true}"#;
    assert_eq!(
        voice_segment(piper).as_deref(),
        Some("voice: my-piper (en_US-demo-medium) 2.0x muted")
    );
}

#[test]
fn voice_segment_shows_nothing_it_does_not_understand() {
    assert_eq!(
        voice_segment(r#"{"schema":2,"persona":"x","speed":1.0}"#),
        None
    );
    assert_eq!(voice_segment(r#"{"persona":"x"}"#), None);
    assert_eq!(voice_segment(r#"{"schema":1,"speed":1.0}"#), None);
    assert_eq!(voice_segment("not json"), None);
    assert_eq!(read_voice_card("/nonexistent/voice.d/none.json"), None);
}

#[test]
fn voice_session_prefers_the_env_then_the_project_slug() {
    assert_eq!(
        voice_session(Some("my-session"), "/Users/me/code/app").as_deref(),
        Some("my-session")
    );
    assert_eq!(
        voice_session(Some(""), "/Users/me/code/app").as_deref(),
        Some("-Users-me-code-app")
    );
    assert_eq!(voice_session(None, ""), None);
    assert_eq!(project_slug("/w/a_b.c d"), "-w-a-b-c-d");
    assert_eq!(
        voice_card_path("/home/x", "session"),
        "/home/x/.claude-tts/voice.d/session.json"
    );
}

#[test]
fn voice_card_is_read_from_disk_when_present() {
    let dir = std::env::temp_dir().join(format!("csr-voice-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".claude-tts/voice.d")).unwrap();
    let home = dir.to_str().unwrap();
    std::fs::write(voice_card_path(home, "session"), CARD).unwrap();
    let card = read_voice_card(&voice_card_path(home, "session")).unwrap();
    assert_eq!(
        voice_segment(&card).as_deref(),
        Some("voice: my-persona (speaker_a) 1.8x")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Review fixes ──────────────────────────────────────────────────────

/// Two sessions reporting the same numbers back to back are two rows; the
/// duplicate check looks at this session's last row only.
#[test]
fn identical_rows_from_two_sessions_are_both_kept() {
    let conn = Connection::open_in_memory().unwrap();
    ensure_schema(&conn).unwrap();
    for sid in ["s1", "s2", "s1"] {
        log_metrics(
            &conn,
            "proj",
            None,
            Some("Fable"),
            Some(sid),
            Some("p1"),
            Some(50_000),
            50_000,
            10,
            200_000,
            25.0,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
        .unwrap();
    // s1, then s2 (new session, kept), then s1 again (same as s1's last row, dropped).
    assert_eq!(n, 2);
}

/// Ahead/behind across a merge commit, checked against git's own left-right
/// count. Layout: A - B - M (upstream) where M merges C; C - D (head). Head is
/// one ahead (D) and two behind (B, M). A walk that stops at the first sight
/// of the merge base C gets this wrong when the walk reaches C before B.
#[test]
fn ahead_behind_counts_across_a_merge() {
    let dir = std::env::temp_dir().join(format!("csr-ab-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let commit = |msg: &str| {
        std::fs::write(dir.join(msg), msg).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", msg]);
    };
    git(&["init", "-q", "-b", "main"]);
    commit("A");
    git(&["checkout", "-q", "-b", "feature"]);
    commit("C");
    git(&["checkout", "-q", "main"]);
    commit("B");
    git(&["merge", "-q", "--no-ff", "-m", "M", "feature"]);
    git(&["checkout", "-q", "feature"]);
    commit("D");
    // feature tracks a "remote" branch that points at main's merge commit.
    git(&["update-ref", "refs/remotes/origin/feature", "main"]);
    git(&[
        "config",
        "remote.origin.url",
        "https://example.invalid/r.git",
    ]);
    git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    git(&["config", "branch.feature.remote", "origin"]);
    git(&["config", "branch.feature.merge", "refs/heads/feature"]);

    let expect = git(&["rev-list", "--left-right", "--count", "HEAD...@{u}"]);
    assert_eq!(expect, "1\t2", "git's own count is the oracle");

    let repo = gix::open(&dir).unwrap();
    assert_eq!(ahead_behind(&repo), Some((1, 2)));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── lines: one-line mode and the row layout ──

#[test]
fn strip_ansi_removes_escapes_and_keeps_text() {
    assert_eq!(strip_ansi("plain"), "plain");
    assert_eq!(strip_ansi("\x1b[0m"), "");
    assert_eq!(
        strip_ansi("a \x1b[38;2;100;116;139m|\x1b[0m b"),
        "a | b",
        "24-bit colour pairs vanish, the text between stays"
    );
    assert_eq!(strip_ansi("\x1b[1;31mred\x1b[0m\x1b[0m!"), "red!");
    assert_eq!(strip_ansi("\u{26C1}\u{2192} ok"), "\u{26C1}\u{2192} ok");
    assert_eq!(strip_ansi("tail\x1b"), "tail", "a lone ESC is dropped");
    assert_eq!(
        strip_ansi("x\x1b[38;2;1"),
        "x",
        "an unterminated sequence never panics"
    );
    assert_eq!(strip_ansi("\x1bQ?"), "Q?", "only CSI is skipped");
    assert_eq!(visible_len("\x1b[38;2;1;2;3mab\x1b[0m\u{26C1}"), 3);
}

#[test]
fn line_mode_parses_leniently() {
    assert_eq!(LineMode::parse("one"), Some(LineMode::One));
    assert_eq!(LineMode::parse(" ONE "), Some(LineMode::One));
    assert_eq!(LineMode::parse("Multi"), Some(LineMode::Multi));
    assert_eq!(LineMode::parse("two"), None);
    assert_eq!(LineMode::parse(""), None);
    let d = Config::default();
    assert_eq!(d.line_mode(), LineMode::Multi, "multi is the default");
}

#[test]
fn lines_config_never_breaks_the_rest() {
    let cfg: Config = serde_json::from_str(r#"{"bar": true, "lines": "one"}"#).unwrap();
    assert!(cfg.bar);
    assert_eq!(cfg.line_mode(), LineMode::One);
    let cfg: Config = serde_json::from_str(r#"{"bar": true, "lines": "bogus"}"#).unwrap();
    assert!(cfg.bar, "a bad value costs only itself");
    assert_eq!(cfg.line_mode(), LineMode::Multi);
    let cfg: Config = serde_json::from_str(r#"{"lines": 3}"#).unwrap();
    assert_eq!(cfg.line_mode(), LineMode::Multi);
}

/// A hook payload that fills every row. The rate-limit windows and the
/// cache expiry are long past, so the output does not move with the clock.
const FIXTURE: &str = r#"{"model":{"display_name":"Claude Opus 4.6"},"workspace":{"project_dir":"/srv/app","current_dir":"/srv/app/crates/core","git_worktree":"fix-auth"},"context_window":{"total_input_tokens":84210,"total_output_tokens":1900,"context_window_size":200000,"used_percentage":43,"current_usage":{"input_tokens":10,"output_tokens":900,"cache_read_input_tokens":60000,"cache_creation_input_tokens":3090}},"cost":{"total_cost_usd":1.234,"total_duration_ms":3725000},"version":"2.1.0","exceeds_200k_tokens":false,"effort":{"level":"high"},"vim":{"mode":"NORMAL"},"agent":{"name":"reviewer"},"pr":{"number":7,"review_state":"approved"},"prompt_cache":{"warm":true,"caching_observed":true,"ttl":"1h","expires_at":1000100,"misses":0,"hit_ratio":0.97},"rate_limits":{"five_hour":{"used_percentage":62.5,"resets_at":1000000},"seven_day":{"used_percentage":41.2,"resets_at":1000000}}}"#;

fn fixture_env() -> Env {
    Env {
        mode: Mode::Standard,
        bar_width: 16,
        memory: (0, 0),
        on_chars: 0,
        residue: Vec::new(),
        git: None,
        voice: None,
        now: 2_000_000,
    }
}

fn fixture_lines(cfg: &Config, env: &Env) -> Lines {
    let data: Input = serde_json::from_str(FIXTURE).unwrap();
    build_lines(&data, cfg, env)
}

fn one_line_cfg() -> Config {
    Config {
        lines: Some(LineMode::One),
        ..plain()
    }
}

/// What the single-row-per-kind layout printed before the rows were built
/// separately, captured from that version on this payload.
const MULTI_GOLDEN_PLAIN: &str = "/srv/app | cd:crates/core | Opus 4.6 | CC:2.1.0 | dur:1h02m\nctx 43% (86k/200k) | last in:84210 out:1900 | $1.23 | cache 97% warm 1h, cold now (13:48Z)\ngit: fix-auth\n5h window: 62% used !, resets now @ Mon Jan 12 13:46 UTC pace 0.6x\n7d window: 41% used, resets now @ Mon Jan 12 13:46 UTC pace 0.4x\neffort:high | [NORMAL] | {reviewer}";

const MULTI_GOLDEN_COLOR: &str = "/srv/app \x1b[38;2;100;116;139m|\x1b[0m cd:crates/core | Opus 4.6 \x1b[38;2;100;116;139m|\x1b[0m CC:2.1.0 \x1b[38;2;100;116;139m|\x1b[0m dur:1h02m\nctx \x1b[38;2;250;204;21m43%\x1b[0m (86k/200k) \x1b[38;2;100;116;139m|\x1b[0m last in:84210 out:1900 \x1b[38;2;100;116;139m|\x1b[0m $1.23 \x1b[38;2;100;116;139m|\x1b[0m cache \x1b[38;2;74;222;128m97%\x1b[0m \x1b[38;2;74;222;128mwarm\x1b[0m 1h, cold now (13:48Z)\ngit: fix-auth\n5h window: 62% used !, resets now @ Mon Jan 12 13:46 UTC \x1b[38;2;74;222;128mpace 0.6x\x1b[0m\n7d window: 41% used, resets now @ Mon Jan 12 13:46 UTC \x1b[38;2;74;222;128mpace 0.4x\x1b[0m\neffort:high | [NORMAL] | {reviewer}";

#[test]
fn default_multi_line_output_is_byte_identical_to_before_the_split() {
    let env = fixture_env();
    let cfg = plain();
    assert_eq!(cfg.line_mode(), LineMode::Multi);
    let lines = fixture_lines(&cfg, &env);
    assert_eq!(assemble(&lines, &cfg, 100), MULTI_GOLDEN_PLAIN);
    assert_eq!(
        assemble(&lines, &cfg, 10),
        MULTI_GOLDEN_PLAIN,
        "multi never looks at the width"
    );
    let colored = Config::default();
    let lines = fixture_lines(&colored, &env);
    assert_eq!(assemble(&lines, &colored, 100), MULTI_GOLDEN_COLOR);
}

#[test]
fn multi_line_without_a_window_size_keeps_cost_on_the_project_row() {
    let mut data: Input = serde_json::from_str(FIXTURE).unwrap();
    data.context_window = None;
    let cfg = plain();
    let lines = build_lines(&data, &cfg, &fixture_env());
    assert_eq!(lines.ctx, "");
    assert!(
        lines
            .project
            .ends_with(" | $1.23 | cache 97% warm 1h, cold now (13:48Z)"),
        "{}",
        lines.project
    );
    assert!(!assemble(&lines, &cfg, 100).contains("ctx "));
}

#[test]
fn multi_line_without_a_project_row_starts_with_a_blank_line() {
    let lines = Lines {
        ctx: "ctx 1%".into(),
        misc: "m".into(),
        ..Lines::default()
    };
    assert_eq!(assemble(&lines, &plain(), 80), "\nctx 1%\nm");
    assert_eq!(assemble(&Lines::default(), &plain(), 80), "");
}

#[test]
fn rows_fill_from_the_environment() {
    let env = Env {
        memory: (25_363, 4_000),
        on_chars: 21_000,
        residue: vec![74_000, 3_100],
        git: Some(GitInfo {
            branch: "main".into(),
            age_secs: None,
            ahead: 2,
            behind: 0,
            dirty: true,
        }),
        voice: Some("voice: amy 2.0x".into()),
        ..fixture_env()
    };
    let lines = fixture_lines(&plain(), &env);
    assert!(
        lines.project.ends_with("mem:25KB+4KB | on:21kch"),
        "{}",
        lines.project
    );
    assert_eq!(lines.residue, "res: +74k +3.1k");
    assert_eq!(lines.git, "git: main * ahead:2 | PR#7 approved");
    assert_eq!(
        lines.misc,
        "effort:high | voice: amy 2.0x | [NORMAL] | {reviewer}"
    );
    let off = Config {
        voice: false,
        ..plain()
    };
    assert!(!fixture_lines(&off, &env).misc.contains("voice"));
}

#[test]
fn one_line_at_width_200_keeps_what_fits_and_drops_the_least_important() {
    let env = Env {
        residue: vec![74_000],
        ..fixture_env()
    };
    let cfg = one_line_cfg();
    let lines = fixture_lines(&cfg, &env);
    let out = assemble(&lines, &cfg, 200);
    assert!(!out.contains('\n'));
    assert!(visible_len(&out) <= 200, "{} wide", visible_len(&out));
    // Row widths: project 59, ctx 90, git 13, misc 35, five-hour 66. Misc
    // would make 206, so it goes, and everything below it with it.
    assert_eq!(
        out,
        "/srv/app | cd:crates/core | Opus 4.6 | CC:2.1.0 | dur:1h02m | ctx 43% (86k/200k) | last in:84210 out:1900 | $1.23 | cache 97% warm 1h, cold now (13:48Z) | git: fix-auth"
    );
    assert_eq!(visible_len(&out), 168);
    // Ten columns more and the misc row fits; the windows are still below it.
    let out = assemble(&lines, &cfg, 210);
    assert!(out.ends_with(" | git: fix-auth | effort:high | [NORMAL] | {reviewer}"));
    assert!(!out.contains("res:") && !out.contains("window"), "{out}");
}

#[test]
fn one_line_everything_fits_when_the_terminal_is_wide_enough() {
    let env = Env {
        residue: vec![74_000],
        ..fixture_env()
    };
    let cfg = one_line_cfg();
    let lines = fixture_lines(&cfg, &env);
    let out = assemble(&lines, &cfg, 1000);
    let rows = [
        &lines.project,
        &lines.ctx,
        &lines.residue,
        &lines.git,
        &lines.five_hour,
        &lines.seven_day,
        &lines.misc,
    ];
    assert_eq!(
        out,
        rows.iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join(" | "),
        "render order, plain separator when colour is off"
    );
}

#[test]
fn one_line_at_width_80_keeps_only_the_project_row_here() {
    let cfg = one_line_cfg();
    let lines = fixture_lines(&cfg, &fixture_env());
    // The project row is 59 columns and ctx 90: the pair needs 152.
    let out = assemble(&lines, &cfg, 80);
    assert_eq!(
        out,
        "/srv/app | cd:crates/core | Opus 4.6 | CC:2.1.0 | dur:1h02m"
    );
    assert!(visible_len(&out) <= 80);
    let out = assemble(&lines, &cfg, 151);
    assert!(!out.contains("ctx 43%"), "{out}");
    let out = assemble(&lines, &cfg, 152);
    assert!(out.ends_with("(13:48Z)") && !out.contains("git:"), "{out}");
    assert_eq!(visible_len(&out), 152);
    let out = assemble(&lines, &cfg, 170);
    assert!(out.ends_with(" | git: fix-auth"), "{out}");
}

#[test]
fn one_line_at_width_40_never_goes_blank() {
    let cfg = one_line_cfg();
    let lines = fixture_lines(&cfg, &fixture_env());
    let out = assemble(&lines, &cfg, 40);
    assert_eq!(
        out, lines.project,
        "the project row survives even when it alone is too wide"
    );
    // With no project row, the next row up takes its place.
    let no_project = Lines {
        project: String::new(),
        ..fixture_lines(&cfg, &fixture_env())
    };
    let out = assemble(&no_project, &cfg, 40);
    assert!(out.starts_with("ctx 43%"), "{out}");
    assert!(!out.contains(" | git"), "{out}");
    assert_eq!(assemble(&Lines::default(), &cfg, 40), "");
}

#[test]
fn one_line_drops_by_priority_and_counts_separators() {
    let lines = Lines {
        project: "P".repeat(10),
        ctx: "C".repeat(10),
        residue: "R".repeat(10),
        git: "G".repeat(10),
        five_hour: "F".repeat(10),
        seven_day: "S".repeat(10),
        misc: "M".repeat(10),
    };
    let cfg = one_line_cfg();
    let sep = " | ";
    let show = |w: usize| assemble(&lines, &cfg, w);
    // N rows of 10 take 10N + 3(N-1) columns: 88 for seven, then 75, 62,
    // 49, 36, 23, 10.
    assert_eq!(visible_len(&show(88)), 88);
    assert!(show(88).contains('R'));
    // One short: residue goes. Then seven-day, five-hour, misc, git, ctx.
    assert!(!show(87).contains('R') && show(87).contains('S'));
    assert!(!show(74).contains('S') && show(74).contains('F'));
    assert!(!show(61).contains('F') && show(61).contains('M'));
    assert!(!show(48).contains('M') && show(48).contains('G'));
    assert!(!show(35).contains('G') && show(35).contains('C'));
    assert_eq!(
        show(23),
        format!("{}{}{}", "P".repeat(10), sep, "C".repeat(10))
    );
    // Project last, and never dropped.
    assert_eq!(show(22), "P".repeat(10));
    assert_eq!(show(0), "P".repeat(10));
}

#[test]
fn one_line_separator_is_dim_when_colour_is_on() {
    let lines = Lines {
        project: "a".into(),
        ctx: "b".into(),
        ..Lines::default()
    };
    let cfg = Config {
        lines: Some(LineMode::One),
        ..Config::default()
    };
    assert_eq!(
        assemble(&lines, &cfg, 80),
        format!("a {}|{} b", DIM, RESET),
        "the same DIM + reset pair the rows use inside themselves"
    );
    assert_eq!(visible_len(&assemble(&lines, &cfg, 80)), 5);
}

#[test]
fn one_line_width_ignores_colour_codes() {
    let env = fixture_env();
    let cfg = Config {
        lines: Some(LineMode::One),
        ..Config::default()
    };
    let lines = fixture_lines(&cfg, &env);
    let wide = assemble(&lines, &cfg, 160);
    let plain_cfg = one_line_cfg();
    let plain_wide = assemble(&fixture_lines(&plain_cfg, &env), &plain_cfg, 160);
    assert_eq!(
        strip_ansi(&wide),
        plain_wide,
        "colour on or off, the same pieces survive the same width"
    );}
