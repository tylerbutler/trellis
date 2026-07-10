# Changelog

## v0.1.0 - 2026-07-10

Initial release, covering all three phases of the design's rollout plan:

- **Workspace model**: members discovered from `workspace.toml` globs, the
  dependency graph computed from `gleam.toml` path deps, deterministic
  topological ordering, cycle detection.
- **Introspection**: `list` (with `--since`/`--with-dependents`/`--releasable`),
  `graph` (text/dot/mermaid/json), `info`.
- **Task running**: `run` and `exec` with graph-parallel scheduling,
  `pkg ▏`-prefixed streamed output, summary table, `--target all`,
  `--serial`, `--keep-going`; custom tasks via `[tasks]`.
- **Validation**: `doctor` checks every workspace invariant (globs, graph,
  ignore-release consistency, tag collisions, lockfile drift, changelog
  versions, generated `.changie.yaml`) and reports all problems at once;
  `--fix` regenerates generated files.
- **Changelog & versioning** (wrapping changie): `changelog sync/check/new`,
  `version plan/apply` with surgical `manifest.toml` lockfile patching and
  zero Hex calls.
- **Release & publish**: `tag plan/create` (with `--github-release`),
  `publish` with Hex idempotency checks, retry/backoff, graph-derived
  path-dep rewriting and guaranteed manifest restore; `lockfile refresh`;
  `ci matrix/outputs/tag-package` for GitHub Actions.
