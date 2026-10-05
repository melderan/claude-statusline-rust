// ─────────────────────────────────────────────────────────────────────
// Directory + memory
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn home_dir() -> Option<String> {
    std::env::var("HOME").ok()
}

/// Shorten a path by substituting $HOME with ~.
pub(crate) fn tilde(path: &str) -> String {
    if let Some(home) = home_dir()
        && let Some(rest) = path.strip_prefix(&home)
    {
        return format!("~{}", rest);
    }
    path.to_string()
}

/// If `current` is under `project`, return the relative suffix (leading "/" stripped).
/// Otherwise return the full current path (tilde-shortened).
pub(crate) fn relative_current(project: &str, current: &str) -> Option<String> {
    if current == project {
        return None;
    }
    if let Some(rest) = current.strip_prefix(project) {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    } else {
        Some(tilde(current))
    }
}

/// Claude Code memory slug: absolute path with '/' → '-'.
/// Matches ~/.claude/projects/<slug>/memory/ layout.
pub(crate) fn path_to_memory_slug(abs_path: &str) -> String {
    abs_path.replace('/', "-")
}

/// Returns (MEMORY.md bytes, other-memory-files bytes).
///
/// Only the top-level MEMORY.md is loaded at session start; every other
/// `.md` under the memory directory, at any depth, is reachable by recall,
/// so the second number walks subdirectories too.
pub(crate) fn memory_bytes(project_dir: &str) -> (u64, u64) {
    let home = match home_dir() {
        Some(h) => h,
        None => return (0, 0),
    };
    let slug = path_to_memory_slug(project_dir);
    let dir = std::path::PathBuf::from(format!("{}/.claude/projects/{}/memory", home, slug));
    memory_bytes_in(&dir)
}

/// Depth limit for the memory walk; no sane memory tree is this deep, and it
/// bounds the work even if the visited set misses a loop.
pub(crate) const MEMORY_WALK_MAX_DEPTH: usize = 8;

pub(crate) fn memory_bytes_in(dir: &std::path::Path) -> (u64, u64) {
    let mut index = 0u64;
    let mut other = 0u64;
    // Symlinks are followed (the memory directory may itself be a link), so
    // remember each real path, directory or file, and count it once: a link
    // back up the tree cannot loop, and a file reached twice is one file.
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((path, depth)) = stack.pop() {
        match std::fs::canonicalize(&path) {
            Ok(real) => {
                if !seen.insert(real) {
                    continue;
                }
            }
            Err(_) => continue,
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // DirEntry::metadata does not follow a symlink; fs::metadata does.
            let meta = match std::fs::metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                if depth < MEMORY_WALK_MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !name_str.ends_with(".md") {
                continue;
            }
            if let Ok(real) = std::fs::canonicalize(entry.path())
                && !seen.insert(real)
            {
                continue;
            }
            if depth == 0 && name_str == "MEMORY.md" {
                index += meta.len();
            } else {
                other += meta.len();
            }
        }
    }
    (index, other)
}

// ─────────────────────────────────────────────────────────────────────
// Always-on text: what every turn carries before anyone speaks
// ─────────────────────────────────────────────────────────────────────

/// The files Claude Code loads into every turn of a session: the user
/// CLAUDE.md, every CLAUDE.md / .claude/CLAUDE.md / CLAUDE.local.md from the
/// project directory up to the root, their `@path` imports (depth-capped like
/// Claude Code's own 5), and the memory index MEMORY.md. Characters, not
/// tokens: roughly four characters to a token for English prose.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct AlwaysOn {
    pub(crate) chars: u64,
    /// (path, chars), in load order, each file once.
    pub(crate) files: Vec<(String, u64)>,
}

pub(crate) const IMPORT_MAX_DEPTH: usize = 5;

pub(crate) fn always_on(project_dir: &str, home: &str) -> AlwaysOn {
    let mut out = AlwaysOn::default();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if !home.is_empty() {
        roots.push(
            std::path::PathBuf::from(home)
                .join(".claude")
                .join("CLAUDE.md"),
        );
    }
    if !project_dir.is_empty() {
        // Root first, project last: the order Claude Code shows in /memory.
        let mut dirs: Vec<std::path::PathBuf> = std::path::Path::new(project_dir)
            .ancestors()
            .map(|d| d.to_path_buf())
            .collect();
        dirs.reverse();
        for d in dirs {
            roots.push(d.join("CLAUDE.md"));
            roots.push(d.join(".claude").join("CLAUDE.md"));
            roots.push(d.join("CLAUDE.local.md"));
        }
        if !home.is_empty() {
            roots.push(
                std::path::PathBuf::from(home)
                    .join(".claude")
                    .join("projects")
                    .join(path_to_memory_slug(project_dir))
                    .join("memory")
                    .join("MEMORY.md"),
            );
        }
    }
    for r in roots {
        add_always_on_file(&r, home, 0, &mut seen, &mut out);
    }
    out
}

/// Largest file read for the always-on count; anything bigger is skipped so a
/// stray import of a log or a dump cannot stall the render.
pub(crate) const IMPORT_MAX_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn add_always_on_file(
    path: &std::path::Path,
    home: &str,
    depth: usize,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut AlwaysOn,
) {
    let Ok(real) = std::fs::canonicalize(path) else {
        return;
    };
    // Regular files only: a FIFO or a device would hang every render.
    match std::fs::metadata(&real) {
        Ok(m) if m.is_file() && m.len() <= IMPORT_MAX_BYTES => {}
        _ => return,
    }
    if !seen.insert(real) {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    // Count what the model sees; a stray invalid byte is one replacement char.
    let text = String::from_utf8_lossy(&bytes);
    let n = text.chars().count() as u64;
    out.chars += n;
    out.files.push((path.to_string_lossy().into_owned(), n));
    if depth >= IMPORT_MAX_DEPTH {
        return;
    }
    let base = path.parent().unwrap_or(std::path::Path::new("/"));
    for imp in claude_md_imports(&text) {
        let target = if let Some(rest) = imp.strip_prefix("~/") {
            if home.is_empty() {
                continue;
            }
            std::path::PathBuf::from(home).join(rest)
        } else if imp.starts_with('/') {
            std::path::PathBuf::from(&imp)
        } else {
            base.join(&imp)
        };
        add_always_on_file(&target, home, depth + 1, seen, out);
    }
}

/// `@path` imports in a CLAUDE.md: an `@` at line start or after whitespace,
/// then the path up to the next whitespace. A bare name (`@HOUSE.md`) is an
/// import too; a name that is not a file is skipped at read time. Fenced
/// code blocks and inline code are skipped, as Claude Code skips them.
pub(crate) fn claude_md_imports(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    // An open fence: (fence char, run length). Closed by a run of the same
    // char at least as long, alone on its line.
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if let Some((fc, n)) = fence {
            let run = t.chars().take_while(|&c| c == fc).count();
            if run >= n && t[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent <= 3 {
            let fc = t.chars().next().unwrap_or(' ');
            if fc == '`' || fc == '~' {
                let run = t.chars().take_while(|&c| c == fc).count();
                if run >= 3 {
                    fence = Some((fc, run));
                    continue;
                }
            }
        }
        // Indented code block: four spaces or a tab.
        if line.starts_with("    ") || line.starts_with('\t') {
            continue;
        }
        let plain = strip_code_spans(line);
        // `@` counts at line start or after whitespace, so `me@example.com`
        // is not an import. Byte offsets index `plain`, never a char count.
        let mut at_token_start = true;
        let mut rest = plain.as_str();
        while let Some(i) = rest.find('@') {
            let starts_token = if i == 0 {
                at_token_start
            } else {
                rest[..i].ends_with(char::is_whitespace)
            };
            let after = &rest[i + 1..];
            if !starts_token {
                rest = after;
                at_token_start = false;
                continue;
            }
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            let cand = after[..end].trim_end_matches([',', ';', ')', ']', '.', ':']);
            if !cand.is_empty() {
                found.push(cand.to_string());
            }
            rest = &after[end..];
            at_token_start = false;
        }
    }
    found
}

/// Replace inline code spans with a space, CommonMark style: a run of N
/// backticks opens a span that the next run of exactly N closes; a run with
/// no partner is literal text and hides nothing after it.
pub(crate) fn strip_code_spans(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut n = 0;
        while i + n < chars.len() && chars[i + n] == '`' {
            n += 1;
        }
        // Find a closing run of exactly n.
        let mut j = i + n;
        let mut close: Option<usize> = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let mut m = 0;
                while j + m < chars.len() && chars[j + m] == '`' {
                    m += 1;
                }
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(c) => {
                out.push(' ');
                i = c + n;
            }
            None => {
                for _ in 0..n {
                    out.push('`');
                }
                i += n;
            }
        }
    }
    out
}
