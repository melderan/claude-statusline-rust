# Versioning

This project follows [Semantic Versioning 2.0.0](https://semver.org) strictly, and it welcomes major versions. Most projects treat a major bump as a failure to avoid. Here a new major is a good day: it means a better shape was found for the output, the configuration or the data, and it was taken instead of being bolted on beside the old one. The number is the record of that, not a cost.

## The public interface

Semantic Versioning only means something once the public interface is written down. For this program it is:

- **The command line.** The flags (`--version`, `--flush`), the hook JSON read from stdin, and the exit code (always 0 for a render, always 0 for `--flush`).
- **Configuration.** Every key in `~/.config/claude-statusline-rust/config.json`, every `CSR_*` environment variable, their defaults and what they mean. `NO_COLOR`.
- **The output.** The set of lines, their order, and the words and shape of each segment in the default configuration: `ctx 43% (86k/200k)`, `git: main (3h) * ahead:1`, `5h window: 42% used`. Someone who reads the status line every day has learned these.
- **The metrics database.** Table and column names and their meaning, the location of the default file, and the `--flush` recorder contract (table name, columns, unique key).
- **What it runs on.** The oldest Claude Code version whose hook payload it reads correctly.

Not part of the interface, and free to change in any release: colours, the exact spacing inside a segment, which 24-bit values a gradient uses, glyph choices behind the `glyphs` option, log lines on stderr, and anything in `src/` that is not one of the items above.

## What each number means

**MAJOR** goes up when any part of the interface changes in a way that an existing user would notice without changing anything on their side. Removing or renaming a configuration key or variable, changing a default, reordering the lines, renaming a segment's label, changing what a number measures, dropping or renaming a database column, changing the recorder key, requiring a newer Claude Code version. A major release carries a **Migration** section in the changelog that says what to change and why the new shape is better.

**MINOR** goes up when the interface grows and nothing that existed changed. A new line or segment, on or off by default. A new configuration key with a default that keeps the old behaviour. A new column. A new flag. A higher minimum Rust version for building from source (that is a build-time matter, not a runtime one).

**PATCH** goes up when behaviour that was wrong becomes right and nothing else moves: a bug fix, a dependency update, a performance change, a documentation change that ships in the archive.

If a change could be read as two of these, it is the higher one. If in doubt, it is MAJOR, and the doubt is written into the changelog entry.

## Version numbers

- Releases start at **1.0.0**. There is no `0.x` line: under Semantic Versioning a `0.x` version promises nothing, and this project would rather promise something and bump the major when it breaks that promise.
- The only pre-release form is `X.Y.Z-rc.N`, with `N` starting at 1. No other suffixes, no build metadata.
- Exactly one component goes up per release and the ones below it return to zero. `1.4.2` is followed by `1.4.3`, `1.5.0` or `2.0.0`, never `1.6.0` or `2.1.0`.
- No version is ever released twice. A bad release is followed by a fixed one; its GitHub release gets a note at the top saying so, and it stays.

## Tags

- One tag per release, named `vX.Y.Z` exactly, on a commit that is on `main`.
- Tags are annotated and signed (`git tag -s`). The release workflow refuses a lightweight or unsigned tag.
- A pushed tag is never moved or deleted. If the tag was wrong, the next version is the fix.
- `Cargo.toml`, `Cargo.lock`, the tag and the changelog heading all carry the same number, and the release workflow checks all four.

## The changelog

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Every pull request that changes `src/`, `tests/` or `Cargo.toml` adds its lines under `[Unreleased]`, in the right group (`Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`); CI fails a pull request that forgets, unless it carries the `no-changelog` label. A release moves the `[Unreleased]` block under a dated version heading. The GitHub release notes are that block, nothing generated.

## Cutting a release

`RELEASING.md` has the steps. In short: `just tag X.Y.Z` checks the tree, the branch and the number, moves the changelog block, bumps `Cargo.toml` and `Cargo.lock`, runs the tests, commits, and makes the signed tag; pushing the tag publishes.
