# claude-statusline-rust

A status line for Claude Code, written in Rust. Claude Code runs it after each update, passes it a JSON description of the session on stdin, and shows whatever it prints. This program reads only that JSON, the `CLAUDE.md` and memory files under your project and `~/.claude`, its own metrics file, and the git repository you are working in. It makes no network requests, needs no credentials and sends no telemetry.

Example output, rendered from a captured hook payload with neutral names:

```
~/code/my-app | cd:src | Fable 5.1 | CC:2.1.292 | dur:1h12m | mem:2KB+9KB | on:4.8kch
ctx 14% (140k/1000k) | last in:117705 out:4 | $1.87 | cache 93% warm 1h, cold in 38m (00:36Z) miss:2 (ttl_expired_1h)
res: +38k +15k +65k
git: main (3m) * ahead:1 | PR#123 approved
5h window: 42% used, resets 2h09m @ Fri Oct 9 02:07 UTC pace 0.7x
7d window: 18% used, resets 4d23h @ Tue Oct 13 23:54 UTC pace 0.6x
effort:high | "refactor auth" | voice: narrator (en_US-demo-medium) 2.0x
```

Lines and segments appear only when their data exists, so a fresh session shows fewer. The `res:` line is off by default and the 5h/7d lines appear only for accounts whose hook payload carries rate limits.

## Install

### Prebuilt binary

Each GitHub release carries one archive per target, named `claude-statusline-rust-<target>.tar.gz`, and a `SHA256SUMS` file. The targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-apple-darwin`.

```
T=aarch64-apple-darwin
BASE=https://github.com/melderan/claude-statusline-rust/releases/latest/download
curl -fLO "$BASE/claude-statusline-rust-$T.tar.gz"
curl -fLO "$BASE/SHA256SUMS"
grep "claude-statusline-rust-$T.tar.gz" SHA256SUMS | shasum -a 256 -c -
tar -xzf "claude-statusline-rust-$T.tar.gz"
```

On Linux, `sha256sum -c -` replaces `shasum -a 256 -c -`. The check must print `OK` before you run the binary. The archive unpacks to a directory named after it, `claude-statusline-rust-<target>/`, holding the binary, this README and the LICENSE. Put the binary somewhere stable, such as `~/.local/bin`.

### cargo install

```
cargo install --git https://github.com/melderan/claude-statusline-rust
```

This puts the binary in `~/.cargo/bin`.

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) installed, this fetches the release archive for your machine instead of compiling:

```
cargo binstall claude-statusline-rust --git https://github.com/melderan/claude-statusline-rust
```

### From source

```
git clone https://github.com/melderan/claude-statusline-rust
cd claude-statusline-rust
cargo build --release
```

The binary is `target/release/claude-statusline-rust`. The crate uses the 2024 edition and let chains, so it needs Rust 1.88 or newer. SQLite is compiled in, so you also need a C compiler.

To see any build render before wiring it into Claude Code, `scripts/dev-payload` prints a payload in the shape Claude Code sends, with neutral names:

```
scripts/dev-payload | target/release/claude-statusline-rust
```

## Configure Claude Code

Add this to `~/.claude/settings.json`, with the real path:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/path/to/claude-statusline-rust"
  }
}
```

Restart Claude Code. The status line stays blank until you have accepted the workspace trust dialog for the folder. Claude Code documents the mechanism at https://code.claude.com/docs/en/statusline.

## What each line shows

The program reads `COLUMNS` from its environment (80 when unset). Below 60 columns it switches to a compact layout: the optional bar is 8 cells instead of 16 and the `last in/out` segment is dropped.

### Project line

```
~/code/my-app | cd:src | Fable 5.1 | CC:2.1.292 | dur:1h12m | +288 -47 | api:11% | mem:2KB+9KB | on:4.8kch
```

- `~/code/my-app`: `workspace.project_dir`, with your home directory written as `~`.
- `cd:src`: where `workspace.current_dir` sits inside the project. Outside the project it shows the full path. Hidden when the two are equal.
- `Fable 5.1`: `model.display_name` without a leading "Claude ".
- `CC:2.1.292`: the Claude Code `version`.
- `dur:1h12m`: `cost.total_duration_ms`, the wall-clock time the session has run.
- `+288 -47`: `cost.total_lines_added` and `cost.total_lines_removed`, the lines of code added and removed this session; the plus is green and the minus red. Hidden when neither count is present or both are zero, so a fresh session shows nothing. A count missing beside a present one reads as zero.
- `api:11%`: `cost.total_api_duration_ms` as a share of `cost.total_duration_ms`, rounded: how much of the session was spent waiting on the model. Hidden unless both are present and the wall-clock time is above zero. It can pass 100% when calls overlap.
- `mem:2KB+9KB`: bytes on disk under `~/.claude/projects/<project path with / replaced by ->/memory`. The first number is the top-level `MEMORY.md`, which Claude Code loads at session start. The second is every other `.md` file below it, which Claude Code reaches only by search. Units are bytes so they cannot be mistaken for tokens.
- `on:4.8kch`: characters (`ch`, not tokens) in the text Claude Code loads into every turn: `~/.claude/CLAUDE.md`, each `CLAUDE.md`, `.claude/CLAUDE.md` and `CLAUDE.local.md` from the project directory up to the filesystem root, their `@path` imports (five levels deep at most), and that `MEMORY.md`. Files over 4 MiB and non-regular files are skipped. Roughly four characters make a token in English text.

### ctx line

```
ctx 14% (140k/1000k) | last in:117705 out:4 | $1.87 | cache 93% warm 1h, cold in 38m (00:36Z) miss:2 (ttl_expired_1h)
```

- `14% (140k/1000k)`: context used, as a percentage and as thousands of tokens over the window size (`context_window.context_window_size`). The used tokens are the sum of the four counters in `context_window.current_usage` (input, output, cache read, cache creation) plus a fixed baseline of 22,600 tokens. The baseline stands in for the system prompt, tool definitions and MCP schemas, which the hook payload does not break out; it is an estimate, and it makes the figure land closer to what `/context` reports than the payload's own `used_percentage`. Without `current_usage` the payload's `used_percentage` is used instead. The percentage is green up to 33, yellow up to 66, rose above. With `bar` enabled a bar precedes it.
- `200k+` (amber, not shown above): `exceeds_200k_tokens` is true, meaning the latest response crossed the 200k-token threshold.
- `last in:117705 out:4`: `context_window.total_input_tokens` and `total_output_tokens`, taken from the most recent API response. Hidden in compact layout.
- `$1.87`: `cost.total_cost_usd`, shown above one tenth of a cent.
- `cache 93% warm 1h, cold in 38m (00:36Z)`: from `prompt_cache`. The percentage is `hit_ratio`, `1h` is `ttl`, and the countdown and UTC clock time come from `expires_at`. A cold cache reads `cache cold 93% (+110k to rewarm)`, the number being `recache_tokens_if_cold`. `miss:2 (ttl_expired_1h)` is `misses` and the cause from `last_miss_cause`. Hidden until `caching_observed` is true.

When the context is within 20 percent of the window of the point where Claude Code compacts it on its own, the ctx line adds `compact in 12k` in amber, and `compact!` at or past that point. The hook payload does not announce the compaction point, so it is taken as the window minus `compact_reserve` tokens (default 33000). A negative `compact_reserve` turns the marker off.

### Residue line

```
res: +38k +15k +65k
```

Off by default; set `residue` to a number of turns from 1 to 10. Each number is how much the context grew during one of your recent prompts, oldest first, measured from the metrics database. A turn is one `prompt_id`. When the session start falls inside the window, the first number is measured from zero and so includes the launch cost. The numbers sum to less than the ctx figure by the baseline above. The line needs the metrics database.

### Git line

```
git: main (3m) * ahead:1 | PR#123 approved
```

- `main`: the branch of the repository found from `current_dir` (or `project_dir`), or `detached`.
- `(3m)`: age of the HEAD commit, coloured from light blue (under an hour) to indigo (a week or more).
- `*`: the index or working tree has changes, untracked files included.
- `ahead:1` / `behind:2`: commits ahead of or behind the upstream tracking branch. Absent without an upstream.
- `PR#123 approved`: `pr.number` and `pr.review_state`. A GitLab merge request, `pr.kind` = `mr`, reads `MR!45`.

If the directory is not a repository but the payload names a linked worktree (`workspace.git_worktree`), the line shows that name alone.

### Activity line

```
tools: Bash x4 Read x2 Edit x1 | agents: 1 running, 3 done | todo: 3/7 done, now: Write the tests
```

Read from the tail of the session transcript named by `transcript_path`: the last 512 KiB only, so the cost stays flat however long the session runs. Each part appears only when it has something to say.

- `tools`: tool calls since your last prompt, by name, most used first, five names then `+N more`. An MCP tool `mcp__server__name` shows as `name`.
- `agents`: sub-agents launched with the Agent tool. A background agent counts as running until its completion notice arrives, even if it was started several prompts ago; `done` counts those that finished during the current prompt.
- `todo`: progress of the task list (`TaskCreate` and `TaskUpdate`, or `TodoWrite` in older versions): completed over total, and the first item in progress, cut at 40 characters.

The transcript format is not a documented contract. The shapes the parser relies on are listed, with dates, at the top of `src/activity.rs`; a line it does not recognise is skipped. Hidden with `activity: false` or `CSR_ACTIVITY=0`.

### 5h and 7d windows

```
5h window: 42% used, resets 2h09m @ Fri Oct 9 02:07 UTC pace 0.7x
7d window: 18% used, resets 4d23h @ Tue Oct 13 23:54 UTC pace 0.6x
```

From `rate_limits.five_hour` and `rate_limits.seven_day`: percentage used and the reset time from `resets_at`. The 5h line adds `!` above 50 percent and `!!` above 80. The 7d line is hidden at zero. `pace` is the fraction of the window used divided by the fraction of the window elapsed: 1.0x means an even spend lands exactly at the reset, and above it the limit runs out early. Green up to 1.0, amber up to 1.2, rose above; hidden during the first 5 percent of a window.

### Misc line

```
effort:high | "refactor auth" | voice: narrator (en_US-demo-medium) 2.0x
```

Joined with ` | `, in this order, each only when present: `effort:<level>` (`effort.level`), `fast` (`fast_mode`), `think:off` (`thinking.enabled` is false), the session name in quotes (`session_name`, cut at 32 characters), `wt:<name>` (`worktree.name`), the voice segment (see Compatibility), `[NORMAL]` (`vim.mode`) and `{name}` (`agent.name`).

When this render's metrics row was skipped because the database was busy or locked, `db:locked` (amber) comes first on this line, ahead of the tags; see Metrics database.

## One-line mode

```
~/code/my-app | Opus 4.6 | CC:2.1.0 | dur:1h02m | ctx 43% (86k/200k) | $1.23 | git: main (3h) *
```

Set `lines` to `one` (or `CSR_LINES=one`) and every line above is joined into a single row with ` | `, in the same order. When the row is wider than `COLUMNS`, pieces are dropped from the least important up until it fits: the residue numbers first, then the 7d and 5h windows, the activity line, the misc tags, the `db:locked` marker, the tail of the ctx line (cache, then `last in/out`, then cost), the tail of the project line (version, memory, always-on size, duration, lines changed, API share), and the tail of the git line (age, then PR state). The project path and model, the ctx percentage and the branch stay the longest; the project path is never dropped, so on a very narrow terminal it is clipped rather than lost. The default `multi` prints one line per kind of information as shown above.

## Configuration

Settings come from `~/.config/claude-statusline-rust/config.json`, then environment variables override them. An unreadable file or one that fails to parse means all defaults. Boolean variables accept `1`, `true`, `yes`, `on` (any case); anything else is false.

| config.json key | Environment | Default | Meaning |
|---|---|---|---|
| `bar` | `CSR_BAR` | `false` | Draw a bar before the ctx percentage. |
| `glyphs` | `CSR_GLYPHS` | `false` | Use arrows and a stopwatch instead of the ASCII words `cd:`, `dur:`, `ahead:`, `behind:`. |
| `color` | `CSR_COLOR` | `true` | ANSI colour. `NO_COLOR`, when set to any value, turns colour off; `CSR_COLOR` is read after it and wins. |
| `residue` | `CSR_RESIDUE` | `0` | Residue line length in turns, clamped to 0-10. A number or a numeric string; anything else means 0. |
| `metrics_db` | `CSR_METRICS_DB` | unset | Path of a shared metrics file; see below. A blank value means unset. |
| `cache` | `CSR_CACHE` | `true` | The prompt cache segment. |
| `extras` | `CSR_EXTRAS` | `true` | Pace, `200k+`, PR state, and the effort, fast, thinking, session and worktree tags. |
| `voice` | `CSR_VOICE` | `true` | The voice segment. |
| `activity` | `CSR_ACTIVITY` | `true` | The activity line from the transcript. |
| `lines` | `CSR_LINES` | `multi` | `multi` or `one`; see One-line mode. Anything else is ignored. |
| `compact_reserve` | `CSR_COMPACT_RESERVE` | `33000` | Tokens kept free at the compaction point; negative turns the `compact in` marker off. |

## Metrics database

Each render writes one row to a local SQLite file, so the residue line has history to read and you can chart your own usage. A row holds the timestamp, project directory, branch, model, session and prompt ids, context tokens, the hook's input and output token counts, window size and percentage, session cost, both rate-limit percentages and reset times, and the always-on character count with the list of files behind it. A render that changes none of the token counts, rate-limit percentages or always-on count writes nothing. The file stays on your machine; nothing reads it except this program and tools you point at it.

By default the file is `~/.config/dbg/statusline-metrics.db`, in WAL mode. A SQLite failure leaves the displayed line as it was, with one exception: when the database is busy or locked, at open or at insert, the row is skipped and the misc line starts with `db:locked`, so a stuck lock does not go unnoticed. The marker clears on the first render that can write. It does not appear for other failures, such as a read-only file or a full disk; those only print to stderr when `metrics_db` is set.

Set `metrics_db` (or `CSR_METRICS_DB`) to use another file, for example one on a network or virtual-filesystem mount that rejects SQLite's default locks. `~`, `~/x` and relative paths resolve from `$HOME`, never from the working directory. That file is opened with SQLite's `unix-dotfile` VFS and a rollback journal, so locking is a `<file>.lock` directory. A render waits 50 ms for it; on timeout it shows `db:locked`, prints one line to stderr and keeps its row in a file of its own, named by time and session, under the local metrics directory (`~/.config/dbg/spill/`); writing it takes no lock. The next render that gets the lock writes the kept rows first, oldest first with their original timestamps, for at most 20 ms or 100 rows, and then its own, so a busy shared file delays rows instead of losing them. A kept row's file is deleted only after the shared file has committed it, and a unique `spill_key` column in the shared file stops a row written twice from appearing twice. A kept row the shared file refuses for a reason other than a lock is never retried or deleted: it is moved to a `failed/` directory beside it together with the error, one stderr line names it, and the rows behind it drain as usual. It never removes a lock, since a lock that looks stale may belong to a slow writer. The exception to the 50 ms is a file that does not exist yet (or is still empty and less than 10 seconds old): the renders that create it wait up to 1 second per lock, so the first of them can create the table while the others queue behind it instead of skipping their rows. Once the file exists, every render is back to 50 ms.

`claude-statusline-rust --flush` copies local rows into a shared recorder database: a SQLite file that several machines or containers write into. It copies only rows newer than the last flush, in batches, and is safe to repeat. It needs `CSR_RECORDER_DB` (the recorder path) and an instance name in `CSR_ROOM`, falling back to `SANDBOX_NAME`; without either it prints a message and does nothing. It always exits 0 so it can run from a Stop hook. It creates a `measures` table in the recorder and refuses an existing one whose unique key differs. Each lock the flush takes, on the local file or the recorder, waits up to 3 seconds; a lock held throughout makes it give up after about 3 seconds. The flush takes those locks one step after another, so one that meets a busy file at more than one step can take a multiple of that, about 6 seconds when the local file and then the recorder are each busy for most of their wait.

## Usage report

`claude-statusline-rust --report [WINDOW]` prints a plain-text report from the metrics file. `WINDOW` is `24h` (the default), `7d` or `30d`; anything else prints a usage line to stderr and exits 2. It reads the same file the renders write (`metrics_db` or `CSR_METRICS_DB` included), writes nothing to it, and uses no colour.

```
$ claude-statusline-rust --report 7d
Usage, last 7d (times UTC)
start        end            dur  project  model      turns        peak ctx   cost  last in/out
10-06 22:21  10-07 00:50  2h28m  my-app   Fable 5.1      3  38% 380k/1000k  $6.20       240k/4
10-08 22:55  10-09 00:21  1h26m  notes    Fable 5.1      2  12% 120k/1000k  $0.85       120k/4
23:21        00:50        1h29m  my-app   Fable 5.1      2  24% 236k/1000k  $3.10       236k/4
Total: 3 sessions, 7 turns, $10.15, 10-06 22:21 to 00:50 (3d02h)
Rate limits: 5h 42% (resets 03:04), 7d 18% (resets 10-15 00:51); 5h peak in window 42%
```

There is one line per session, the one that started last at the bottom. Times are UTC: `HH:MM` within the last 24 hours, `MM-DD HH:MM` otherwise. `turns` counts distinct prompt ids (a row with no prompt id counts as one turn). `peak ctx` is the highest context percentage with the input tokens of that row and the window size. `cost` is the session's last recorded cost: the hook reports a running total, so rows are never summed, and the totals line adds one value per session. A session that began before the window is listed from its first row inside it but keeps its full running cost. `last in/out` are the last row's token counts. The rate-limit line appears when a row in the window has a five-hour percentage: it shows the latest readings and the highest five-hour percentage in the window.

Lines stay within 100 columns by shortening the project name, then the model name; times, counts and costs are never cut. An empty window prints one line saying so. A missing metrics file prints one line naming its path and exits 0, and a file that stays busy or locked for the 3-second wait prints one line to stderr and exits 0; the report never creates the file or removes a lock. Any other failure prints one line to stderr and exits 1.

## Compatibility

The voice segment reads an optional per-session JSON card written by the separate project [claude-code-tts](https://github.com/melderan/claude-code-tts), at `~/.claude-tts/voice.d/<session>.json`. The session is `$CLAUDE_TTS_SESSION` if set, otherwise the project path with every character other than an ASCII letter or digit turned into `-`. Only card schema 1 with a persona is used. It renders as `voice: <persona> (<voice>) <speed>x`, with ` muted` appended when muted. No card means no segment, and nothing else in this program depends on that project.

## Development

Versions follow strict Semantic Versioning, and a major bump is welcome when a better shape is found; `VERSIONING.md` says what the numbers promise. Releases are cut by tag with `just tag X.Y.Z`; `RELEASING.md` has the steps and what the workflow publishes. `CHANGELOG.md` is the record, and a pull request that changes behaviour adds its lines there.

```
cargo test
cargo clippy --all-targets
```

To report a rendering bug, capture the payload Claude Code sends. Save this wrapper as `capture.sh`, make it executable, and point `statusLine.command` at it:

```
#!/bin/sh
tee "$HOME/statusline-sample-$(date +%Y%m%dT%H%M%S).json" | /path/to/claude-statusline-rust
```

Each update writes one JSON file. The payload contains paths, the session id and the session name; replace those before you attach it to an issue.

## License

Apache-2.0. See `LICENSE`.
