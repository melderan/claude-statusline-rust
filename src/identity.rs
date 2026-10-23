//! Identity: which sandbox a session runs in, as the first piece of the
//! project row (`claude·web-app`), so two terminals side by side can be
//! told apart at a glance. Read from `SANDBOX_NAME`; absent outside a
//! sandbox, where the line is as it was.

use crate::*;

/// A sandbox name split for display: `owner--claude--app` is group `claude`,
/// name `app`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct Identity {
    pub(crate) group: Option<String>,
    pub(crate) name: String,
}

/// Splits a sandbox name on `--`. Three or more fields: the last is the name,
/// the one before it the group, and a leading `claude-code-` is dropped from
/// the name because every Claude Code sandbox would repeat it. Fewer than
/// three fields: the whole name, no group. Blank: None. A control character
/// shows as `?`, so a newline or escape in the variable cannot split the
/// line or reach the terminal.
pub(crate) fn parse_identity(sandbox_name: &str) -> Option<Identity> {
    let full: String = sandbox_name
        .trim()
        .chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect();
    let full = full.as_str();
    if full.is_empty() {
        return None;
    }
    let fields: Vec<&str> = full.split("--").collect();
    if fields.len() < 3 || fields.iter().any(|f| f.is_empty()) {
        return Some(Identity {
            group: None,
            name: full.to_string(),
        });
    }
    let last = fields[fields.len() - 1];
    let name = match last.strip_prefix("claude-code-") {
        Some(rest) if !rest.is_empty() => rest,
        _ => last,
    };
    Some(Identity {
        group: Some(fields[fields.len() - 2].to_string()),
        name: name.to_string(),
    })
}

/// Bold, in a fixed 256-color code for the groups that have one, so the
/// colour says which group before the word is read. The codes are shared
/// with a status line for another assistant that shows the same piece.
/// Each is a hue no reading on the line uses: green, yellow and rose
/// already say how a number stands, so a label in one of them would
/// borrow that meaning.
fn group_color(group: Option<&str>) -> &'static str {
    match group {
        Some("claude") => "\x1b[1;38;5;208m",
        Some("gpt") => "\x1b[1;38;5;141m",
        _ => "\x1b[1m",
    }
}

/// `group·name`, or `name` alone. The middle dot is used whatever the
/// glyphs setting, so this piece reads the same as the other line's.
pub(crate) fn identity_segment(id: &Identity, cfg: &Config) -> String {
    let text = match &id.group {
        Some(g) => format!("{g}\u{b7}{}", id.name),
        None => id.name.clone(),
    };
    format!(
        "{}{}{}",
        c(cfg, group_color(id.group.as_deref())),
        text,
        reset(cfg)
    )
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn id(group: Option<&str>, name: &str) -> Option<Identity> {
        Some(Identity {
            group: group.map(str::to_string),
            name: name.to_string(),
        })
    }

    #[test]
    fn three_fields_give_group_and_name() {
        assert_eq!(
            parse_identity("acme--gpt--billing-api"),
            id(Some("gpt"), "billing-api")
        );
        assert_eq!(
            parse_identity("acme--claude--claude-statusline"),
            id(Some("claude"), "claude-statusline"),
            "only the claude-code- prefix is dropped"
        );
        assert_eq!(
            parse_identity("acme--claude--claude-code-docs"),
            id(Some("claude"), "docs")
        );
        assert_eq!(
            parse_identity("a--b--c--d"),
            id(Some("c"), "d"),
            "the last two fields count"
        );
    }

    #[test]
    fn other_shapes_show_the_whole_name() {
        assert_eq!(parse_identity("my-sandbox"), id(None, "my-sandbox"));
        assert_eq!(parse_identity("a--b"), id(None, "a--b"));
        assert_eq!(parse_identity("a----c"), id(None, "a----c"));
        assert_eq!(
            parse_identity("x--claude--claude-code-"),
            id(Some("claude"), "claude-code-"),
            "a name that is only the prefix is kept"
        );
        assert_eq!(parse_identity("  "), None);
        assert_eq!(parse_identity(""), None);
    }

    #[test]
    fn control_characters_show_as_question_marks() {
        assert_eq!(parse_identity("o--claude--a\nb"), id(Some("claude"), "a?b"));
        assert_eq!(
            parse_identity("o--claude--a\x1b[2Jb"),
            id(Some("claude"), "a?[2Jb")
        );
        assert_eq!(parse_identity("\tbox\r"), id(None, "box"), "trimmed first");
    }

    #[test]
    fn segment_is_bold_in_the_group_colour() {
        let color = Config::default();
        let plain = Config {
            color: false,
            ..Config::default()
        };
        let claude = parse_identity("o--claude--web-app").unwrap();
        assert_eq!(
            identity_segment(&claude, &color),
            "\x1b[1;38;5;208mclaude\u{b7}web-app\x1b[0m"
        );
        assert_eq!(identity_segment(&claude, &plain), "claude\u{b7}web-app");
        let gpt = parse_identity("o--gpt--billing-api").unwrap();
        assert_eq!(
            identity_segment(&gpt, &color),
            "\x1b[1;38;5;141mgpt\u{b7}billing-api\x1b[0m"
        );
        let other = parse_identity("my-sandbox").unwrap();
        assert_eq!(identity_segment(&other, &color), "\x1b[1mmy-sandbox\x1b[0m");
    }

    #[test]
    fn project_row_starts_with_the_identity_and_keeps_it() {
        let data: Input = serde_json::from_str(
            r#"{"model":{"display_name":"Claude Opus 4.6"},"workspace":{"project_dir":"/srv/app","current_dir":"/srv/app"},"version":"2.1.0"}"#,
        )
        .unwrap();
        let cfg = Config {
            color: false,
            lines: Some(LineMode::One),
            ..Config::default()
        };
        let env = Env {
            identity: parse_identity("o--gpt--billing-api"),
            ..test_env()
        };
        let lines = build_lines(&data, &cfg, &env);
        assert_eq!(
            lines.project.text(),
            "gpt\u{b7}billing-api | /srv/app | Opus 4.6 | CC:2.1.0"
        );
        assert_eq!(lines.project.pieces[0].rank, KEEP);
        let narrow = assemble(&lines, &cfg, 10);
        assert!(narrow.starts_with("gpt\u{b7}billing-api"), "{narrow}");

        let no_dir: Input =
            serde_json::from_str(r#"{"model":{"display_name":"Claude Opus 4.6"}}"#).unwrap();
        assert_eq!(
            build_lines(&no_dir, &cfg, &env).project.text(),
            "gpt\u{b7}billing-api | Opus 4.6"
        );

        let none = test_env();
        assert_eq!(
            build_lines(&data, &cfg, &none).project.text(),
            "/srv/app | Opus 4.6 | CC:2.1.0",
            "outside a sandbox the row is unchanged"
        );
    }

    fn test_env() -> Env {
        Env {
            mode: Mode::Standard,
            bar_width: 16,
            memory: (0, 0),
            on_chars: 0,
            residue: Vec::new(),
            git: None,
            voice: None,
            activity: None,
            identity: None,
            db_locked: false,
            now: 2_000_000,
        }
    }
}
