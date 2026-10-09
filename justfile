# claude-statusline-rust -- fast Rust status line for Claude Code
# Run `just` to see available recipes

# Default recipe - show help
default:
    @just --list

install_dir := env("HOME") / ".local/bin"
binary_name := "claude-statusline-rust"

# Preflight check - ensure build environment is ready
preflight:
    @command -v cargo >/dev/null 2>&1 || { echo "Error: Rust is not installed. Run: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; exit 1; }
    @mkdir -p ~/.local/bin
    @if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then \
        echo "Warning: ~/.local/bin is not in PATH. Add it with:"; \
        echo '  export PATH="$HOME/.local/bin:$PATH"'; \
    fi

# Build debug binary
build: preflight
    cargo build

# Build release binary (optimized, stripped)
release: preflight
    cargo build --release

# Build debug and run with sample JSON (dev loop)
dev:
    echo '{"model":{"id":"claude-opus-4-6","display_name":"Claude Opus 4.6"},"workspace":{"project_dir":"/Users/dev/projects/myapp","git_worktree":"fix-auth-bug"},"context_window":{"total_input_tokens":154200,"total_output_tokens":42100,"context_window_size":1000000,"used_percentage":19.6},"cost":{"total_cost_usd":1.87},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":'"$(date -v+2H -v+13M +%s)"'},"seven_day":{"used_percentage":41.2,"resets_at":'"$(date -v+5d +%s)"'}},"subagents":{"count":3}}' | cargo run

# Quick build and install (skip tests)
install: release
    cp target/release/{{ binary_name }} {{ install_dir }}/{{ binary_name }}
    codesign -s - --force {{ install_dir }}/{{ binary_name }}
    @echo "Installed {{ binary_name }} to {{ install_dir }}/{{ binary_name }}"

# Full build, test, and install to ~/.local/bin
install-full: release test
    cp target/release/{{ binary_name }} {{ install_dir }}/{{ binary_name }}
    codesign -s - --force {{ install_dir }}/{{ binary_name }}
    @echo "Installed {{ binary_name }} to {{ install_dir }}/{{ binary_name }}"

# Remove from ~/.local/bin
uninstall:
    rm -f {{ install_dir }}/{{ binary_name }}
    @echo "Removed {{ install_dir }}/{{ binary_name }}"

# Suggest the next version from the [Unreleased] changelog section
next-version:
    scripts/next-version

# Cut a release: just tag X.Y.Z [--push] (see RELEASING.md)
tag version *flags:
    scripts/release {{ version }} {{ flags }}

# Check a release tag the way the release workflow will: just verify-tag vX.Y.Z
verify-tag tag *flags:
    scripts/verify-tag {{ tag }} {{ flags }}

# Run tests
test:
    cargo test

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Run tests and lint
check: test lint

# Clean build artifacts
clean:
    cargo clean
