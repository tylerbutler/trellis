# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.5.0 - 2026-07-21


### Added

- Interactive commands now print a notice when a newer trellis has been published to crates.io. The check is cached for a day, runs only in a terminal, and is skipped in CI or when `DO_NOT_TRACK` / `TRELLIS_NO_UPDATE_CHECK` is set.
- Configless workspaces and member auto-discovery: `members` in `[tools.trellis]` is now optional — when omitted, every non-gitignored `gleam.toml` in the repository (outside `build/`) marks a member, and with no `[tools.trellis]` table anywhere the git repository root becomes the workspace root with an entirely defaulted configuration. A new reserved `exclude` key, `@members`, removes directories from workspace membership entirely (e.g. committed test fixtures), in both auto-discovered and explicit-members modes. `doctor` announces inferred roots and auto-discovered member counts.

### Fixed

- Recursive member globs now respect repository Git ignore rules, preventing ignored build artifacts and vendored dependencies from being discovered as workspace members.

## v0.4.1 - 2026-07-16


### Fixed

- Release PR title now uses a lowercase `release:` type so it passes conventional-commit / commitlint PR-title checks (matching the release commit message).

## v0.4.0 - 2026-07-14


### Added

- `trellis markdown-help` prints the full CLI command reference as Markdown, useful for generating up-to-date documentation from the command's own help output.
- `trellis doctor --fix` automatically fixes what it safely can — seeding a missing `CHANGELOG.md` with the canonical header, and rewriting `manifest.toml` locked versions that drifted from `gleam.toml` — then reports whatever issues remain. Use `--dry-run` to preview the fixes without writing anything. Findings that require a judgment call (path-dependency escapes, tag collisions, versions behind their changelog) are left for you to resolve.
- Add `Initial Release` (major) to the default changelog kinds.

### Changed

- `path-dep-requirement`'s `caret` option is renamed to `minor`; a new `patch` option (`>= X.Y.Z and < X.(Y+1).0`) allows finer-grained control over the Hex requirement generated for workspace path deps at publish time.
- Remove `ignore-release`; release exclusions now live only in `exclude.@release`. Special `exclude` keys are namespaced under a reserved `@` prefix so they can never collide with a task name — task names and `exclude` keys are validated against it.

## v0.3.0 - 2026-07-13


### Added

- Parse git dependencies (`{ git = "...", ref = "..." }`) in member manifests as external requirements instead of failing with "neither a version nor a path".

### Changed

- Wildcard member globs now skip directories without a gleam.toml (e.g. node_modules alongside packages); literal member paths still require one.

## v0.2.0 - 2026-07-11


### Added

- Add per-task member path exclusions, including a shared release exclusion for changelog, version, tag, and publish commands.

## v0.1.0 - 2026-07-10

### Added

- Workspace model: the root marked by a `[tools.trellis]` table in `gleam.toml`, members discovered from its globs, the dependency graph computed from `gleam.toml` path deps, deterministic topological ordering, cycle detection
- Introspection: `list` (with `--since`/`--with-dependents`/`--releasable`), `graph` (text/dot/mermaid/json), `info`
- Task running: `run` and `exec` with graph-parallel scheduling, prefixed streamed output, summary table, `--target all`, `--serial`, `--keep-going`; custom tasks via `[tools.trellis.tasks]`
- Validation: `doctor` checks every workspace invariant — including unreleased fragment validity — and reports all problems at once
- Changelog & versioning (native engine): TOML fragments, kind-driven version bumps, minijinja-templated rendering, generated per-package changelogs; `changelog new/check`, `version plan/apply` with surgical `gleam.toml` bumps and `manifest.toml` lockfile patching, zero Hex calls
- Release & publish: `tag plan/create` (with `--github-release`), `publish` with Hex idempotency checks, retry/backoff, graph-derived path-dep rewriting and guaranteed manifest restore; `lockfile refresh`; `ci matrix/outputs/tag-package` for GitHub Actions
- Release PR management: `release pr` runs `version apply` on a release branch and creates or updates the pull request via the gh CLI
- Scaffolding: `new <name>` creates a member with metadata copied from a sibling and a stub module and test; no registration step, everything is derived
- Doctor advisory: warns when the gleam on PATH differs from the `.tool-versions` pin

