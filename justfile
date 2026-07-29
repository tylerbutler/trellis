# trellis - a workspace CLI for Gleam monorepos

# === ALIASES ===
alias b := build
alias t := test
alias f := format
alias l := lint
alias c := clean

# Default recipe
default:
    @just --list

# === STANDARD RECIPES ===

# Compile the project
build:
    cargo build

# Run tests
test:
    cargo test

# Format code
format:
    cargo fmt

# Run linter
lint:
    cargo clippy -- -D warnings

# Remove build artifacts
clean:
    cargo clean

# Regenerate every checked-in generated artifact (CLI reference, man pages)
#
# Completion scripts are not generated here: they're produced at runtime by
# `trellis completions <shell>`, and clap_complete warns against saving them to
# disk (the shim talks to the binary over an interface that changes between
# releases).
docs:
    cargo run --quiet -- markdown-help > website/src/content/docs/docs/reference.md
    # Rebuilt from scratch so a removed subcommand can't leave a stale page behind.
    rm -rf assets/man
    cargo run --quiet -- man --out assets/man

# Full validation workflow
ci: format lint test build

alias pr := ci
