# Releasing

Versions follow `VERSIONING.md`: strict Semantic Versioning, one annotated signed tag per release, the changelog as the release notes.

## Steps

1. Make sure every change since the last release has its lines under `[Unreleased]` in `CHANGELOG.md`. Read them and pick the number: a `Removed` entry, a changed default or any other break means MAJOR; `Added` means MINOR; only `Fixed` means PATCH.
2. On an up-to-date `main` with a clean tree, run:

   ```
   just tag X.Y.Z
   ```

   The script (`scripts/release`) refuses to continue unless the branch is `main`, the tree is clean, `HEAD` matches the upstream, `X.Y.Z` is a valid single-step increment of the current version, and `[Unreleased]` has at least one entry. It then moves the `[Unreleased]` block under `## [X.Y.Z] - <today>`, sets the version in `Cargo.toml` and `Cargo.lock`, runs `cargo test --release --locked`, commits `release: vX.Y.Z`, and creates the annotated, signed tag `vX.Y.Z`. Nothing is pushed.

3. Push the commit and the tag:

   ```
   git push origin main
   git push origin vX.Y.Z
   ```

   `just tag X.Y.Z --push` does both at the end of the script instead.

A release candidate is `just tag X.Y.Z-rc.1`; the workflow marks its GitHub release as a pre-release.

## What the tag triggers

The release workflow first verifies the tag: it matches `vX.Y.Z` exactly, it is annotated and signed, its commit is on `main`, the version in `Cargo.toml` and `Cargo.lock` is the same, and `CHANGELOG.md` has a `## [X.Y.Z]` heading. Any of those failing stops the release before anything is built.

It then builds the program for Linux (x86_64 and aarch64) and macOS (Intel and Apple silicon) and creates a GitHub Release for the tag with one `claude-statusline-rust-<target>.tar.gz` archive per platform, a `SHA256SUMS` file listing the checksum of each archive, and the changelog block for that version as the release notes.

## Verifying a download

Put the archive and `SHA256SUMS` in the same directory and run `sha256sum --check --ignore-missing SHA256SUMS` (on macOS, `grep <archive name> SHA256SUMS | shasum -a 256 --check`). The line for your archive should print `OK`.

## If a release was wrong

Do not move or delete the tag. Fix the problem, add the entry to the changelog, and release the next number. Edit the bad release's notes on GitHub to say at the top which version replaces it.
