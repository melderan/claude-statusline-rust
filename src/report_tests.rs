use super::*;

/// 2026-10-10 12:00:00 UTC.
const NOW: i64 = 1_791_633_600;

fn row(ts: i64, session: &str, prompt: &str) -> ReportRow {
    ReportRow {
        ts,
        project: Some("/work/app".into()),
        model: Some("Model 1".into()),
        session_id: Some(session.into()),
        prompt_id: Some(prompt.into()),
        ..Default::default()
    }
}

#[test]
fn civil_dates_are_right_across_leap_days_and_eras() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(59), (1970, 3, 1));
    assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(NOW / 86_400), (2026, 10, 10));
}

#[test]
fn times_show_the_date_only_beyond_a_day() {
    assert_eq!(fmt_time(NOW, NOW), "12:00");
    assert_eq!(fmt_time(NOW - 86_399, NOW), "12:00");
    assert_eq!(fmt_time(NOW - 86_400, NOW), "10-09 12:00");
    assert_eq!(fmt_time(NOW + 3 * 3600, NOW), "15:00");
    assert_eq!(fmt_time(NOW + 5 * 86_400, NOW), "10-15 12:00");
}

#[test]
fn durations_and_counts() {
    assert_eq!(fmt_duration(59), "0m");
    assert_eq!(fmt_duration(45 * 60), "45m");
    assert_eq!(fmt_duration(2 * 3600 + 5 * 60), "2h05m");
    assert_eq!(fmt_duration(86_400 + 3 * 3600), "1d03h");
    assert_eq!(fmt_tokens(Some(999)), "999");
    assert_eq!(fmt_tokens(Some(237_039)), "237k");
    assert_eq!(fmt_tokens(None), "-");
    assert_eq!(project_name(Some("/a/b/app/")), "app");
    assert_eq!(project_name(Some("")), "-");
    assert_eq!(project_name(None), "-");
}

#[test]
fn a_session_costs_its_last_row_not_the_sum() {
    let mut rows = Vec::new();
    for (i, cost) in [0.5, 1.0, 1.5].into_iter().enumerate() {
        let mut r = row(NOW - 600 + i as i64, "a", &format!("p{i}"));
        r.cost_usd = Some(cost);
        rows.push(r);
    }
    let mut b = row(NOW - 300, "b", "q");
    b.cost_usd = Some(2.0);
    rows.push(b);
    let text = render_report(&rows, "24h", "x.db", NOW);
    assert!(text.contains("$1.50"), "{text}");
    assert!(text.contains("Total: 2 sessions, 4 turns, $3.50"), "{text}");
}

#[test]
fn rows_without_a_prompt_id_count_one_turn_each() {
    let mut rows = vec![row(NOW - 10, "a", "p"), row(NOW - 5, "a", "p")];
    for i in 0..3 {
        let mut r = row(NOW - 3 + i, "a", "");
        r.prompt_id = None;
        rows.push(r);
    }
    assert_eq!(sessions(&rows)[0].turns(), 4);
}

#[test]
fn the_project_is_shortened_and_the_numbers_are_not() {
    let mut r = row(NOW - 60, "a", "p");
    r.project = Some(format!("/w/{}", "x".repeat(120)));
    r.cost_usd = Some(12.34);
    r.in_tokens = Some(237_039);
    r.out_tokens = Some(380);
    r.context_pct = Some(24.0);
    r.context_cap = Some(1_000_000);
    let text = render_report(&[r], "24h", "x.db", NOW);
    for line in text.lines() {
        assert!(line.chars().count() <= REPORT_WIDTH, "{line}");
    }
    assert!(text.contains('…'), "{text}");
    assert!(text.contains("24% 237k/1000k"), "{text}");
    assert!(text.contains("$12.34"), "{text}");
    assert!(text.contains("237k/380"), "{text}");
}

#[test]
fn the_rate_line_takes_the_latest_reading_and_the_peak() {
    let mut rows = vec![row(NOW - 300, "a", "p1"), row(NOW - 200, "a", "p2")];
    rows[0].rate_5h_pct = Some(61.0);
    rows[0].rate_5h_resets = Some(NOW + 3600);
    rows[1].rate_5h_pct = Some(42.4);
    rows[1].rate_5h_resets = Some(NOW + 7200);
    rows[1].rate_7d_pct = Some(18.0);
    rows[1].rate_7d_resets = Some(NOW + 5 * 86_400);
    let line = rate_line(&rows, NOW).unwrap();
    assert_eq!(
        line,
        "Rate limits: 5h 42% (resets 14:00), 7d 18% (resets 10-15 12:00); 5h peak in window 61%"
    );
    rows.iter_mut().for_each(|r| r.rate_5h_pct = None);
    assert_eq!(rate_line(&rows, NOW), None);
}

#[test]
fn an_empty_window_is_one_line() {
    let text = render_report(&[], "7d", "x.db", NOW);
    assert_eq!(text, "No sessions in the last 7d in x.db.\n");
}
