# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.1.0 - 2026-07-10

### Added

- Workspace model: members discovered from `workspace.toml` globs, the dependency graph computed from `gleam.toml` path deps, deterministic topological ordering, cycle detection
- Introspection: `list` (with `--since`/`--with-dependents`/`--releasable`), `graph` (text/dot/mermaid/json), `info`
- Task running: `run` and `exec` with graph-parallel scheduling, prefixed streamed output, summary table, `--target all`, `--serial`, `--keep-going`; custom tasks via `[tasks]`
- Validation: `doctor` checks every workspace invariant and reports all problems at once; `--fix` regenerates generated files
- Changelog & versioning (wrapping changie): `changelog sync/check/new`, `version plan/apply` with surgical `manifest.toml` lockfile patching and zero Hex calls
- Release & publish: `tag plan/create` (with `--github-release`), `publish` with Hex idempotency checks, retry/backoff, graph-derived path-dep rewriting and guaranteed manifest restore; `lockfile refresh`; `ci matrix/outputs/tag-package` for GitHub Actions
- Release PR management: `release pr` runs `version apply` on a release branch and creates or updates the pull request via the gh CLI
- Scaffolding: `new <name>` creates a member with metadata copied from a sibling, stub module and test, and regenerated `.changie.yaml` projects
- Doctor advisory: warns when the gleam on PATH differs from the `.tool-versions` pin
