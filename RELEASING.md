# Releasing

To publish a release, bump `version` in `Cargo.toml`, commit the change, tag the commit `vX.Y.Z` with the same version number, and push the tag with `git push origin vX.Y.Z`. The release workflow refuses to run if the tag and the `Cargo.toml` version differ, so a mismatch fails early instead of publishing the wrong number.

Pushing the tag builds the program for Linux (x86_64 and aarch64) and macOS (Intel and Apple silicon). The workflow then creates a GitHub Release for the tag with one `claude-statusline-rust-<target>.tar.gz` archive per platform, a `SHA256SUMS` file listing the checksum of each archive, and release notes generated from the commits since the previous tag.

To verify a download, put the archive and `SHA256SUMS` in the same directory and run `sha256sum --check --ignore-missing SHA256SUMS` (on macOS, `grep <archive name> SHA256SUMS | shasum -a 256 --check`). The line for your archive should print `OK`.
