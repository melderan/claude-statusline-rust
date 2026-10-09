# Changelog

All notable changes to this project are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows [Semantic Versioning](https://semver.org); `VERSIONING.md` says what the numbers mean here.

## [Unreleased]

### Added

- `db:locked` marker on the misc line when a render skips its metrics row because the database is busy or locked, for both the default local file and a shared `metrics_db`. It is kept in one-line mode until the ctx tail would have to go.

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
