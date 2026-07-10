# trellis

A workspace CLI for Gleam monorepos. A trellis is the frame a lattice grows on.

Gleam has no native workspace concept — `gleam build`, `gleam test`, and
`gleam publish` operate on a single package directory. Multi-package repos end
up hand-building workspace features out of bash loops, YAML glue, and
duplicated config. Trellis replaces that glue with one binary that runs
identically locally and in CI.

The design principle:

> **Configure nothing that can be derived. Verify anything that must be duplicated.**

Everything trellis knows comes from two sources that already exist:
`workspace.toml` (member globs) and each member's `gleam.toml` (name, version,
path dependencies). The dependency graph — topological order, publish order,
change impact, path-dep rewrite maps — is computed, never declared.

See [docs/DESIGN.md](docs/DESIGN.md) for the full design, including the
release/publish layers that are not implemented yet.

## Status

Phase 1 of the [rollout plan](docs/DESIGN.md#10-rollout-in-lattice) is
implemented: the workspace model plus `list`, `graph`, `info`, `run`, `exec`,
`doctor`, and `ci`. The changelog/version layer (phase 2) and the
tag/publish layer (phase 3) are designed but not built.

## Configuration

`workspace.toml` at the repo root marks the workspace. Only `members` is
required:

```toml
[workspace]
members = ["packages/*", "examples"]
# Matching members participate in task fan-out but are excluded from
# changelog, versioning, tagging, and publishing.
ignore-release = ["examples"]

# Custom tasks for `trellis run <name>`. Built-in verbs (build, test, check,
# format, docs, deps, clean) need no declaration.
[tasks.lint]
command = "gleam run -m glinter"
needs-deps = true            # run `gleam deps download` first if not cached

[publish]
tag-format = "{name}-v{version}"
```

Each member is a directory with a `gleam.toml`. Path dependencies between
members define the graph; cycles and path deps escaping the workspace are
rejected.

## Commands

Every command works from anywhere inside the workspace (the root is found by
walking up, like `git` or `cargo`).

### Introspection

```
trellis list [--json] [--since <ref>] [--with-dependents] [--releasable]
trellis graph [--format text|dot|mermaid|json]
trellis info <package> [--json]
```

`list` prints members in topological order — dependencies first. `--since
origin/main` filters to packages owning changed files (committed, uncommitted,
and untracked); `--with-dependents` adds the reverse-dependency closure. This
is the primitive behind "only test what a PR touched."

### Task running

```
trellis run <task> [pkgs...] [--since <ref>] [--with-dependents]
                   [--target erlang|javascript|all] [--strict] [--check]
                   [--serial] [--keep-going] [--jobs N]
trellis exec [pkgs...] [--since <ref>] [--serial] [--keep-going] -- <command...>
```

Scheduling is graph-parallel by default: a package runs as soon as its
workspace dependencies have finished, up to `--jobs N` at once. Output is
streamed with a `pkg ▏` prefix and a summary table names any failures.
`--target all` runs the task once per compile target. `--serial` runs one
package at a time in dependency order.

Built-in tasks map 1:1 onto gleam verbs: `build`, `test`, `check`, `format`
(`--check` variant), `docs`, `deps`, `clean`. A `[tasks]` entry with the same
name overrides a built-in.

### Validation

```
trellis doctor
```

Checks every workspace invariant and reports all problems at once: member
globs resolve and parse, path deps stay inside the workspace, the graph is
acyclic, `ignore-release` globs match real members, no releasable package
depends on an unreleasable one, tag formats don't collide, `manifest.toml`
locked versions match workspace-internal `gleam.toml` versions, and no
package's version is behind its CHANGELOG. Non-zero exit on any error — run it
on every PR.

### CI glue

```
trellis ci matrix [--since <ref>] [--releasable]
trellis ci outputs
```

`matrix` emits a GitHub Actions strategy matrix
(`{"include":[{"name","path","version"},…]}`); with `--since` it covers only
affected packages, dependents included. `outputs` emits workspace facts as
`key=value` lines for `$GITHUB_OUTPUT`:

```yaml
- id: plan
  run: echo "matrix=$(trellis ci matrix --since origin/main)" >> "$GITHUB_OUTPUT"
```

## Development

Standard Rust project: `cargo test` runs unit tests plus an end-to-end suite
against the fixture workspace in `tests/fixtures/`. `cargo fmt` and
`cargo clippy --all-targets` are enforced in CI.

## License

MIT — see [LICENSE](LICENSE).
