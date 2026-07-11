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

# Full validation workflow
ci: format lint test build

alias pr := ci
