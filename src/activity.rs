use crate::*;
use std::io::{Seek, SeekFrom};

// ─────────────────────────────────────────────────────────────────────
// Activity line: what the current turn is doing, from the session
// transcript (`transcript_path` in the hook payload). Shows tool calls by
// name, sub-agents running and done, and the todo list's progress:
//
//   tools: Bash x4 Read x2 Edit x1 | agents: 1 running, 3 done | todo: 3/7 done, now: Write the tests
//
// Only the last ACTIVITY_TAIL bytes of the transcript are read, so the
// cost does not grow with the session. Anything unreadable or unexpected
// means no line (or a skipped transcript line), never an error.
//
// Transcript shapes this relies on (JSONL, one object per line, observed
// in Claude Code 2.1.28x transcripts on 2026-10-08; the format is not a
// documented contract, so every field is optional here):
//
// - Human prompt: `{"type":"user","message":{"content":"text"}}`. Content
//   may also be an array of blocks (text, image). Newer versions add
//   `"origin":{"kind":"human"}`; other kinds (`task-notification`,
//   `plugin`, ...) are injected messages, not prompts. Lines with
//   `isMeta`, `isSidechain` or `isCompactSummary` set are not prompts.
// - Tool call: `{"type":"assistant","message":{"content":[{"type":"tool_use",
//   "id":"toolu_..","name":"Bash","input":{..}}]}}`. One assistant
//   response may span several lines; calls are counted once per id.
// - Tool result: `{"type":"user","message":{"content":[{"type":"tool_result",
//   "tool_use_id":"toolu_..",..}]},"toolUseResult":{..}}`.
// - Sub-agent: a tool call named `Agent` (`Task` in older versions). A
//   background launch gets its tool result at once, with
//   `"toolUseResult":{"status":"async_launched",..}`; it finishes later
//   with a user line whose content is a string holding
//   `<task-notification>..<tool-use-id>toolu_..</tool-use-id>..<status>completed</status>`.
// - Todo list: a tool call named `TodoWrite` with input
//   `{"todos":[{"content":"..","status":"pending|in_progress|completed",
//   "activeForm":".."}]}`; each call replaces the whole list.
// ─────────────────────────────────────────────────────────────────────

/// Bytes read from the end of the transcript.
pub(crate) const ACTIVITY_TAIL: u64 = 512 * 1024;

/// Tool names shown before `+N more`.
const TOOLS_SHOWN: usize = 5;

/// Longest todo item shown after `now:`.
const TODO_NOW_CHARS: usize = 40;

/// The last `max` bytes of the file at `path`, starting at the first whole
/// line. When the file is shorter than `max` it is read whole. None when
/// the file cannot be opened, sized, seeked or read.
pub(crate) fn read_tail(path: &str, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.take(max).read_to_end(&mut buf).ok()?;
    let body: &[u8] = if start > 0 {
        // The first line is cut; drop it, newline included.
        match buf.iter().position(|&b| b == b'\n') {
            Some(i) => &buf[i + 1..],
            None => &[],
        }
    } else {
        &buf
    };
    Some(String::from_utf8_lossy(body).into_owned())
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Activity {
    /// Tool calls this turn by name, most used first, ties by name.
    pub(crate) tools: Vec<(String, usize)>,
    pub(crate) agents_running: usize,
    pub(crate) agents_done: usize,
    pub(crate) todo: Option<Todo>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Todo {
    pub(crate) done: usize,
    pub(crate) total: usize,
    /// The first item in progress, if any.
    pub(crate) now: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum AgentState {
    /// Called, no result yet.
    Waiting,
    /// Launched in the background, no completion notice yet.
    Background,
    Done,
}

/// Scans transcript text (whole JSONL lines) for the current turn: every
/// line after the last human prompt, or the whole text when it holds no
/// prompt. The todo list comes from the last `TodoWrite` call anywhere in
/// the text. Lines that do not parse are skipped.
pub(crate) fn scan_activity(text: &str) -> Activity {
    let mut seen_ids: std::collections::HashSet<String> = Default::default();
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut agents: Vec<(String, AgentState)> = Vec::new();
    let mut last_todo_line: Option<&str> = None;

    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(line) = serde_json::from_str::<Line>(raw) else {
            continue;
        };
        if line.is_sidechain == Some(true) {
            continue;
        }
        let Some(kind) = line.kind.as_deref() else {
            continue;
        };
        let content = line.message.map(|m| m.content).unwrap_or(Content::Other);
        match kind {
            "user" if is_human_prompt(&line.origin, line.is_meta, line.is_compact, &content) => {
                // A new turn: everything before it belongs to an older one.
                seen_ids.clear();
                counts.clear();
                agents.clear();
            }
            "user" => match &content {
                Content::Blocks(blocks) => {
                    for b in blocks {
                        if b.kind.as_deref() != Some("tool_result") {
                            continue;
                        }
                        let Some(id) = b.tool_use_id.as_deref() else {
                            continue;
                        };
                        if let Some(a) = agents.iter_mut().find(|(i, _)| i == id) {
                            a.1 = if line.async_launched {
                                AgentState::Background
                            } else {
                                AgentState::Done
                            };
                        }
                    }
                }
                Content::Text(t) => {
                    if let Some(id) = notified_tool_use(t)
                        && let Some(a) = agents.iter_mut().find(|(i, _)| i == id)
                    {
                        a.1 = AgentState::Done;
                    }
                }
                Content::Other => {}
            },
            "assistant" => {
                let Content::Blocks(blocks) = content else {
                    continue;
                };
                for b in blocks {
                    if b.kind.as_deref() != Some("tool_use") {
                        continue;
                    }
                    let Some(name) = b.name else {
                        continue;
                    };
                    if let Some(id) = &b.id
                        && !seen_ids.insert(id.clone())
                    {
                        continue;
                    }
                    if name == "TodoWrite" {
                        last_todo_line = Some(raw);
                    }
                    if (name == "Agent" || name == "Task")
                        && let Some(id) = b.id
                    {
                        agents.push((id, AgentState::Waiting));
                    }
                    match counts.iter_mut().find(|(n, _)| *n == name) {
                        Some(e) => e.1 += 1,
                        None => counts.push((name, 1)),
                    }
                }
            }
            _ => {}
        }
    }

    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let agents_done = agents.iter().filter(|a| a.1 == AgentState::Done).count();
    Activity {
        tools: counts,
        agents_running: agents.len() - agents_done,
        agents_done,
        todo: last_todo_line.and_then(todo_from_line),
    }
}

/// A user line typed by a person, as opposed to a tool result, a
/// notification or another injected message.
fn is_human_prompt(
    origin: &Option<Origin>,
    is_meta: Option<bool>,
    is_compact: Option<bool>,
    content: &Content,
) -> bool {
    if is_meta == Some(true) || is_compact == Some(true) {
        return false;
    }
    if let Some(kind) = origin.as_ref().and_then(|o| o.kind.as_deref()) {
        return kind == "human";
    }
    match content {
        Content::Text(t) => !t.trim_start().starts_with("<task-notification>"),
        Content::Blocks(blocks) => {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|b| b.kind.as_deref() != Some("tool_result"))
        }
        Content::Other => false,
    }
}

/// The tool call a `<task-notification>` reports as finished; None for
/// any other text, or a notice whose status is still `running`.
fn notified_tool_use(text: &str) -> Option<&str> {
    if !text.trim_start().starts_with("<task-notification>") {
        return None;
    }
    if tag_value(text, "status") == Some("running") {
        return None;
    }
    tag_value(text, "tool-use-id")
}

fn tag_value<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let from = text.find(&open)? + open.len();
    let len = text[from..].find(&close)?;
    Some(text[from..from + len].trim())
}

/// The todo list from the last `TodoWrite` call on one transcript line.
/// None when the list is empty or does not parse.
fn todo_from_line(raw: &str) -> Option<Todo> {
    let line: TodoLine = serde_json::from_str(raw).ok()?;
    let todos = line
        .message?
        .content
        .into_iter()
        .rev()
        .find(|b| b.kind.as_deref() == Some("tool_use") && b.name.as_deref() == Some("TodoWrite"))?
        .input?
        .todos?;
    if todos.is_empty() {
        return None;
    }
    let done = todos
        .iter()
        .filter(|t| t.status.as_deref() == Some("completed"))
        .count();
    let now = todos
        .iter()
        .find(|t| t.status.as_deref() == Some("in_progress"))
        .and_then(|t| t.content.as_deref())
        .map(|s| {
            // A cut never ends on a space before the ellipsis.
            let t = truncate(s.trim(), TODO_NOW_CHARS);
            match t.strip_suffix('\u{2026}') {
                Some(head) => format!("{}\u{2026}", head.trim_end()),
                None => t,
            }
        });
    Some(Todo {
        done,
        total: todos.len(),
        now,
    })
}

/// The activity line, or None when the turn has nothing to show.
pub(crate) fn activity_line(a: &Activity, cfg: &Config) -> Option<String> {
    let rst = reset(cfg);
    let mut parts: Vec<String> = Vec::new();
    if !a.tools.is_empty() {
        let mut s = String::from("tools:");
        for (name, n) in a.tools.iter().take(TOOLS_SHOWN) {
            let _ = write!(s, " {} x{}", name, n);
        }
        if a.tools.len() > TOOLS_SHOWN {
            let _ = write!(s, " +{} more", a.tools.len() - TOOLS_SHOWN);
        }
        parts.push(s);
    }
    match (a.agents_running, a.agents_done) {
        (0, 0) => {}
        (r, 0) => parts.push(format!("agents: {}{} running{}", c(cfg, AMBER), r, rst)),
        (0, d) => parts.push(format!("agents: {} done", d)),
        (r, d) => parts.push(format!(
            "agents: {}{} running{}, {} done",
            c(cfg, AMBER),
            r,
            rst,
            d
        )),
    }
    if let Some(t) = &a.todo {
        let mut s = format!("todo: {}/{} done", t.done, t.total);
        if let Some(now) = &t.now {
            let _ = write!(s, ", now: {}", now);
        }
        parts.push(s);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(&format!(" {}|{} ", c(cfg, DIM), rst)))
}

/// The activity line for the transcript at `path`; None on any failure.
pub(crate) fn activity_from_path(path: &str, cfg: &Config) -> Option<String> {
    let text = read_tail(path, ACTIVITY_TAIL)?;
    activity_line(&scan_activity(&text), cfg)
}

// ── Transcript line shapes: only the fields read, all optional ──

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient")]
    kind: Option<String>,
    #[serde(rename = "isSidechain", default, deserialize_with = "lenient")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta", default, deserialize_with = "lenient")]
    is_meta: Option<bool>,
    #[serde(rename = "isCompactSummary", default, deserialize_with = "lenient")]
    is_compact: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    origin: Option<Origin>,
    #[serde(default)]
    message: Option<Message>,
    /// True when `toolUseResult.status` is `async_launched`.
    #[serde(rename = "toolUseResult", default, deserialize_with = "async_launched")]
    async_launched: bool,
}

#[derive(Deserialize)]
struct Origin {
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Content,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type", default, deserialize_with = "lenient")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    tool_use_id: Option<String>,
}

/// Message content: a string, an array of blocks, or anything else
/// (ignored). Read with a visitor so large tool inputs and outputs are
/// skipped in place rather than buffered.
#[derive(Default)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
    #[default]
    Other,
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Content;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("message content")
            }
            fn visit_str<E>(self, s: &str) -> Result<Content, E> {
                Ok(Content::Text(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<Content, E> {
                Ok(Content::Text(s))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Content, A::Error> {
                let mut blocks = Vec::new();
                while let Some(b) = seq.next_element::<Block>()? {
                    blocks.push(b);
                }
                Ok(Content::Blocks(blocks))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Content, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(Content::Other)
            }
            fn visit_unit<E>(self) -> Result<Content, E> {
                Ok(Content::Other)
            }
            fn visit_bool<E>(self, _: bool) -> Result<Content, E> {
                Ok(Content::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Content, E> {
                Ok(Content::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Content, E> {
                Ok(Content::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Content, E> {
                Ok(Content::Other)
            }
        }
        d.deserialize_any(V)
    }
}

/// Reads only `status` from a `toolUseResult` of any shape, skipping the
/// rest (it can hold a whole file's contents).
fn async_launched<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a tool result")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
            let mut launched = false;
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                if key == "status" {
                    let v: serde_json::Value = map.next_value()?;
                    launched = v.as_str() == Some("async_launched");
                } else {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
            Ok(launched)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(false)
        }
        fn visit_str<E>(self, _: &str) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_unit<E>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_bool<E>(self, _: bool) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_i64<E>(self, _: i64) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_u64<E>(self, _: u64) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_f64<E>(self, _: f64) -> Result<bool, E> {
            Ok(false)
        }
    }
    d.deserialize_any(V)
}

#[derive(Deserialize)]
struct TodoLine {
    #[serde(default, deserialize_with = "lenient")]
    message: Option<TodoMessage>,
}

#[derive(Deserialize)]
struct TodoMessage {
    #[serde(default)]
    content: Vec<TodoBlock>,
}

#[derive(Deserialize)]
struct TodoBlock {
    #[serde(rename = "type", default, deserialize_with = "lenient")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    input: Option<TodoInput>,
}

#[derive(Deserialize)]
struct TodoInput {
    #[serde(default, deserialize_with = "lenient")]
    todos: Option<Vec<TodoItem>>,
}

#[derive(Deserialize)]
struct TodoItem {
    #[serde(default, deserialize_with = "lenient")]
    content: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    status: Option<String>,
}
