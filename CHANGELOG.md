# Changelog

All notable changes to this project are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows [Semantic Versioning](https://semver.org); `VERSIONING.md` says what the numbers mean here.

## [Unreleased]

### Added

- `cargo binstall` metadata in `Cargo.toml`, so `cargo binstall claude-statusline-rust --git <repo>` fetches the release archive for the host instead of compiling.

## [1.1.0] - 2026-10-09

### Added

- `db:locked` marker on the misc line when a render skips its metrics row because the database is busy or locked, for both the default local file and a shared `metrics_db`. It is kept in one-line mode until the ctx tail would have to go.

### Fixed

- The first renders of a brand-new shared metrics file (`CSR_METRICS_DB`) no longer skip rows while the first of them creates the table: a metrics file that does not exist yet, or is still empty and under 10 seconds old, gets up to 1 second per lock instead of 50 ms. A file that exists keeps the 50 ms. The default local file gets the same first-life wait; it was not losing rows.
- A `--flush` stopped by a local value of the wrong type names the value's real type, without the column index.
- A `--flush` that finds the recorder locked no longer suggests the file is in WAL mode because of an empty `-wal` file beside it.
- Always-on count: an indented line directly after a paragraph line is a continuation of the paragraph, not an indented code block.
- Always-on count: a closing code fence may be indented at most three spaces and must be at least as long as the opening fence, with the same character.
- Always-on count: `always_on_files` lists cleaned paths, with no `./` segments or doubled slashes.

### Changed

- README: the flush's 3-second patience is per lock, so a flush that meets busy files at two steps can take about 6 seconds.

## [1.0.0] - 2026-10-09

### Added

- First release of the status line as a project other people can install: README, Apache-2.0 licence, prebuilt archives with checksums from a tag-triggered workflow.
- Activity line from the session transcript: tool calls this turn, sub-agents running and done, task-list progress.
- One-line mode (`lines: "one"`, `CSR_LINES=one`) with a fixed order in which pieces are dropped to fit the terminal width.
- `compact in Nk` marker on the ctx line as the context nears the auto-compaction point (`compact_reserve`, default 33000 tokens).
- Prompt cache segment: hit ratio, warm or cold, expiry as a countdown and a clock time, the last miss cause.
- Pace on the 5h and 7d windows, the `200k+` marker, pull request state on the git line, and effort, fast mode, thinking, session name and worktree on the misc line.
- Voice segment read from an optional per-session card written by claude-code-tts.
- `--version` prints the program name and version.
- `--flush` copies local metrics rows into a shared recorder database.

### Changed

- "session in/out" on the ctx line is now "last in/out": the numbers are the most recent API call, not the session total.

### Removed

- The subagents segment. The hook payload never carries that field.

### Fixed

- The release script no longer leaves `CHANGELOG.md` and `Cargo.toml` with mode 600: it writes them in place instead of moving a temp file over them.
- Ahead/behind counts across merge commits now match `git rev-list --left-right --count`.
- The metrics duplicate check compares against this session's last row, not the last row of any session.
- The memory directory slug uses the same rule as Claude Code, so project paths with a dot or an underscore find their memory directory.
- A hook sub-object of an unexpected shape costs one segment instead of blanking the whole status line.
