//! Fuel file: the context use of the latest render, as a small JSON file
//! another program can read (`fuel_file` / `CSR_FUEL_FILE`), and `--fuel`,
//! the one line that file says.

use crate::*;

/// A file this young that already says the same numbers is left alone: the
/// hook runs several times a second, the numbers change once per response.
const FUEL_FRESH_SECS: u64 = 60;

/// What the fuel file holds, besides the labels.
#[derive(Debug, PartialEq)]
pub(crate) struct Fuel {
    pub(crate) used_pct: f64,
    pub(crate) used_tokens: i64,
    pub(crate) window_tokens: i64,
}

/// The numbers the ctx row shows: the baseline-corrected count when the
/// payload has `current_usage`, else Claude Code's own `used_percentage`.
/// None before the first response of a session, when neither is known.
pub(crate) fn fuel_from(data: &Input) -> Option<Fuel> {
    let cw = data.context_window.as_ref()?;
    let window = cw.context_window_size.filter(|&n| n > 0)?;
    let (used_tokens, used_pct) = match content_tokens(cw.current_usage.as_ref()) {
        Some(content) => {
            let used = content + CONTEXT_BASELINE;
            (used, used as f64 * 100.0 / window as f64)
        }
        None => {
            let pct = cw.used_percentage?;
            ((pct * window as f64 / 100.0).round() as i64, pct)
        }
    };
    Some(Fuel {
        used_pct: (used_pct * 10.0).round() / 10.0,
        used_tokens,
        window_tokens: window,
    })
}

/// `room=a,house=b` into ordered pairs; an entry with no `=` or an empty key
/// is dropped.
pub(crate) fn parse_labels(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// The JSON document. Labels go first and the measured fields after, so a
/// label can never stand in for a number.
pub(crate) fn fuel_json(
    fuel: &Fuel,
    labels: &[(String, String)],
    session_id: Option<&str>,
    as_of: &str,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (k, v) in labels {
        m.insert(k.clone(), serde_json::Value::from(v.as_str()));
    }
    m.insert("used_pct".into(), fuel.used_pct.into());
    m.insert("used_tokens".into(), fuel.used_tokens.into());
    m.insert("window_tokens".into(), fuel.window_tokens.into());
    m.insert("session_id".into(), session_id.into());
    m.insert("as_of".into(), as_of.into());
    m.insert("source".into(), "statusline".into());
    serde_json::Value::Object(m)
}

/// True when the file at `path` is younger than `FUEL_FRESH_SECS` and holds
/// the same numbers for the same session, so writing again says nothing new.
fn fuel_unchanged(path: &std::path::Path, doc: &serde_json::Value) -> bool {
    let young = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < FUEL_FRESH_SECS);
    if !young {
        return false;
    }
    let Some(old) = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    else {
        return false;
    };
    ["used_tokens", "window_tokens", "session_id"]
        .iter()
        .all(|k| old.get(k) == doc.get(k))
}

/// Write `doc` to `path` through a temporary file in the same directory and
/// a rename, so a reader sees the old file or the new one, never half of
/// one. Mode 0644. The temporary name carries the pid: two sessions writing
/// the same file at once each rename their own whole file.
pub(crate) fn write_fuel(path: &std::path::Path, doc: &serde_json::Value) -> std::io::Result<bool> {
    use std::io::Write as _;
    if fuel_unchanged(path, doc) {
        return Ok(false);
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("fuel_file has no file name"))?;
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut body = serde_json::to_vec(doc).map_err(std::io::Error::other)?;
    body.push(b'\n');
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    drop(f);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(true)
}

/// The render's part: write the fuel file when one is configured and the
/// payload has numbers. Any failure is one stderr line; the status line
/// prints as always.
pub(crate) fn record_fuel(cfg: &Config, home: &str, data: &Input) {
    let Some(path) = resolve_metrics_db(cfg.fuel_file.as_deref(), home) else {
        return;
    };
    let Some(fuel) = fuel_from(data) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let doc = fuel_json(
        &fuel,
        &cfg.fuel_labels,
        data.session_id.as_deref(),
        &utc_stamp(now),
    );
    if let Err(e) = write_fuel(std::path::Path::new(&path), &doc) {
        eprintln!("claude-statusline-rust: fuel file {path}: {e}");
    }
}

/// `fuel: 37.2% of 1000k tokens, as of 2026-10-20T18:45:00.123Z`, from a
/// fuel file's JSON.
pub(crate) fn fuel_line(doc: &serde_json::Value) -> Option<String> {
    let pct = doc.get("used_pct")?.as_f64()?;
    let window = doc.get("window_tokens")?.as_i64()?;
    let as_of = doc.get("as_of")?.as_str()?;
    Some(format!(
        "fuel: {pct:.1}% of {}k tokens, as of {as_of}",
        (window + 500).div_euclid(1000)
    ))
}

/// `--fuel [PATH]`: print the line for the fuel file at PATH, or at the
/// configured `fuel_file`. Exit 1 when there is no file to read or it does
/// not parse, 2 when no path is given or configured.
pub(crate) fn fuel_main(args: &[String]) {
    let home = home_dir().unwrap_or_default();
    let explicit = args
        .iter()
        .skip_while(|a| *a != "--fuel")
        .nth(1)
        .filter(|a| !a.starts_with('-'))
        .cloned();
    let raw = explicit.or_else(|| Config::load().fuel_file);
    let Some(path) = resolve_metrics_db(raw.as_deref(), &home) else {
        eprintln!("claude-statusline-rust: --fuel needs a PATH or fuel_file / CSR_FUEL_FILE");
        std::process::exit(2);
    };
    let line = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|doc| fuel_line(&doc));
    match line {
        Some(l) => println!("{l}"),
        None => {
            eprintln!("claude-statusline-rust: no readable fuel file at {path}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(json: &str) -> Input {
        serde_json::from_str(json).unwrap()
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("csr-fuel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn current_usage_wins_and_matches_the_ctx_row() {
        let d = input(
            r#"{"context_window":{"context_window_size":1000000,"used_percentage":5,
            "current_usage":{"input_tokens":10,"output_tokens":390,
            "cache_read_input_tokens":300000,"cache_creation_input_tokens":2000}}}"#,
        );
        let f = fuel_from(&d).unwrap();
        assert_eq!(f.used_tokens, 302_400 + CONTEXT_BASELINE);
        assert_eq!(f.window_tokens, 1_000_000);
        assert_eq!(f.used_pct, 32.5);
    }

    #[test]
    fn used_percentage_when_no_current_usage() {
        let d =
            input(r#"{"context_window":{"context_window_size":200000,"used_percentage":41.26}}"#);
        assert_eq!(
            fuel_from(&d),
            Some(Fuel {
                used_pct: 41.3,
                used_tokens: 82_520,
                window_tokens: 200_000
            })
        );
    }

    #[test]
    fn nothing_known_is_no_fuel() {
        // The first render of a session (2.1.302): nulls everywhere.
        let d = input(
            r#"{"context_window":{"total_input_tokens":0,"context_window_size":1000000,
            "used_percentage":null,"current_usage":null}}"#,
        );
        assert_eq!(fuel_from(&d), None);
        assert_eq!(
            fuel_from(&input(r#"{"context_window":{"used_percentage":3}}"#)),
            None
        );
        assert_eq!(fuel_from(&Input::default()), None);
    }

    #[test]
    fn labels_parse_and_never_replace_a_number() {
        let labels = parse_labels(" room=statusline , house=claude,bad,=x,used_pct=99");
        assert_eq!(labels[0], ("room".into(), "statusline".into()));
        assert_eq!(labels.len(), 3);
        let fuel = Fuel {
            used_pct: 12.0,
            used_tokens: 120,
            window_tokens: 1000,
        };
        let doc = fuel_json(&fuel, &labels, Some("s1"), "T");
        assert_eq!(doc["used_pct"], 12.0);
        assert_eq!(doc["house"], "claude");
        assert_eq!(doc["source"], "statusline");
        assert_eq!(doc["session_id"], "s1");
    }

    #[test]
    fn write_is_whole_0644_and_skips_a_fresh_same_file() {
        let dir = tmp_dir("write");
        let path = dir.join("fuel.json");
        let fuel = Fuel {
            used_pct: 1.0,
            used_tokens: 10,
            window_tokens: 1000,
        };
        let a = fuel_json(&fuel, &[], Some("s"), "first");
        assert!(write_fuel(&path, &a).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o644);
        }
        // Same numbers, a later stamp: nothing new, no write.
        let b = fuel_json(&fuel, &[], Some("s"), "second");
        assert!(!write_fuel(&path, &b).unwrap());
        // New numbers: written.
        let c = fuel_json(
            &Fuel {
                used_tokens: 11,
                ..fuel
            },
            &[],
            Some("s"),
            "third",
        );
        assert!(write_fuel(&path, &c).unwrap());
        let back: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["as_of"], "third");
        // No temporary file is left behind.
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn the_line_reads_the_file() {
        let doc = serde_json::json!({"used_pct": 37.2, "window_tokens": 1_000_000,
            "as_of": "2026-10-20T18:45:00.000Z"});
        assert_eq!(
            fuel_line(&doc).unwrap(),
            "fuel: 37.2% of 1000k tokens, as of 2026-10-20T18:45:00.000Z"
        );
        assert_eq!(fuel_line(&serde_json::json!({"used_pct": 1})), None);
    }
}
