# trellis

A workspace CLI for Gleam monorepos. A trellis is the frame a lattice grows on.

Gleam has no native workspace concept — `gleam build`, `gleam test`, and
`gleam publish` operate on a single package directory. Multi-package repos end
up hand-building workspace features out of bash loops, YAML glue, and
duplicated config. Trellis replaces that glue with one binary that runs
identically locally and in CI.

The design principle:

> **Configure nothing that can be derived. Verify anything that must be duplicated.**

Everything trellis knows comes from one file format the ecosystem already
uses: `gleam.toml`. The workspace root's manifest carries a `[tools.trellis]`
table (member globs and options); each member's manifest supplies its name,
version, and path dependencies. The dependency graph — topological order,
publish order, change impact, path-dep rewrite maps — is computed, never
declared.

See [docs/DESIGN.md](docs/DESIGN.md) for the full design.

## Status

The full [rollout plan](docs/DESIGN.md#10-rollout-in-lattice) is implemented:
the workspace model plus `list`, `graph`, `info`, `run`, `exec`, `doctor`,
`ci`, `changelog`, `version`, `tag`, `publish`, and `lockfile`, with prebuilt
release binaries for distribution.

## Installation

Trellis ships as a single prebuilt binary — the same distribution model as
`just`, `changie`, and `ratchet` — so it installs in CI in about a second
with zero runtime dependencies. Releases are built and published by
[cargo-dist](https://opensource.axo.dev/cargo-dist/), with SLSA build
provenance attestations.

**Shell installer** (Linux and macOS):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tylerbutler/trellis/releases/latest/download/trellis-installer.sh | sh
```

**PowerShell installer** (Windows):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/tylerbutler/trellis/releases/latest/download/trellis-installer.ps1 | iex"
```

**Homebrew:**

```sh
brew install tylerbutler/tap/trellis
```

**mise / asdf** (via the [ubi](https://mise.jdx.dev/dev-tools/backends/ubi.html)
backend), which is how a consuming workspace pins trellis in `.tool-versions`
alongside its other tools:

```sh
mise use "ubi:tylerbutler/trellis@0.1.0"
```

**From source:**

```sh
cargo install --git https://github.com/tylerbutler/trellis
```

Prebuilt archives for every target are on the
[releases page](https://github.com/tylerbutler/trellis/releases). Pin a
specific version in CI by replacing `latest/download` with
`download/v0.1.0` in the installer URL.

## Configuration

A `[tools.trellis]` table in a `gleam.toml` marks the workspace root — no
separate config file. The root manifest may be config-only, or a regular
gleam package that also anchors the workspace. Only `members` is required:

```toml
# gleam.toml at the repo root
[tools.trellis]
members = ["packages/*", "examples"]
# Matching members participate in task fan-out but are excluded from
# changelog, versioning, tagging, and publishing.
ignore-release = ["examples"]

# Custom tasks for `trellis run <name>`. Built-in verbs (build, test, check,
# format, docs, deps, clean) need no declaration.
[tools.trellis.tasks.lint]
command = "gleam run -m glinter"
needs-deps = true            # run `gleam deps download` first if not cached

[tools.trellis.publish]
tag-format = "{name}-v{version}"
```

Each member is a directory with a `gleam.toml`. Path dependencies between
members define the graph; cycles and path deps escaping the workspace are
rejected, and a `[tools.trellis]` table in a *member* manifest is a doctor
error (it would hijack root discovery).

## Commands

Every command works from anywhere inside the workspace (the root is found by
walking up to the first `gleam.toml` with a `[tools.trellis]` table, like
`git` or `cargo` — member manifests along the way are skipped).

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
(`--check` variant), `docs`, `deps`, `clean`. A `[tools.trellis.tasks]` entry with the same
name overrides a built-in.

### Changelog & versioning

```
trellis changelog new [--package <pkg>] --kind <kind> --body <text>
trellis changelog check --base <ref> [--head <ref>] [--json]
trellis version plan [--json]
trellis version apply [--json]
```

The changelog engine is native — no second tool to install, no config file
to keep in sync. Changes are recorded as TOML fragments in
`.changes/unreleased/` (`project`, `kind`, `body`); `changelog new` writes
one, non-interactively, which suits CI and agents as well as shells.
`changelog check` maps a `base...head` diff to packages and fails if a
changed releasable package has no unreleased fragment, emitting JSON
(including a markdown `preview`) for a PR sticky comment.

`version plan` computes each pending package's next version from its
fragments' kinds (the largest bump wins; kinds and their bumps are
configurable under `[tools.trellis.changelog]`). `version apply` renders each package's
version section (minijinja templates, see below), stores it under
`.changes/<package>/`, reassembles the package's CHANGELOG.md newest-first,
bumps `gleam.toml` with a surgical TOML edit — no regex — and finally patches
each member's `manifest.toml` so locked workspace-internal deps match. Zero
Hex network calls throughout. Invalid fragments (unknown package or kind,
empty body, unparseable TOML) are hard errors for `plan`/`apply`: silently
dropping a change is exactly the drift trellis exists to prevent.

Rendering is controlled by minijinja templates in `[tools.trellis.changelog]`, each with a
small context (`name`, `version`, `date`, `tag`, `kind`, `body` as
applicable):

```toml
[tools.trellis.changelog]
version-format = "## v{{ version }} - {{ date }}"     # default
kind-format = "### {{ kind }}"                         # default
change-format = "- {{ body }}"                         # default
kinds = [
  { label = "Breaking", bump = "major" },
  { label = "Added", bump = "minor" },
  { label = "Fixed", bump = "patch" },
]
```

Note that each package's CHANGELOG.md is a generated file: the source of
truth is the version sections under `.changes/<package>/`, and `apply`
reassembles the changelog from them.

### Release & publish

```
trellis release pr [--base <branch>] [--branch <branch>]
trellis tag plan [--json]
trellis tag create [--push] [--github-release]
trellis publish <pkg | --tag <tag> | --all-untagged> [--dry-run]
trellis lockfile refresh [--package <pkg>]
```

`release pr` turns pending changelog fragments into a release pull request:
it runs `version apply` on a release branch, commits the bumps, force-pushes
(so the branch is regenerated each run), and creates — or, when one is
already open, updates — the PR via the `gh` CLI. The body carries the bump
table and each package's new CHANGELOG section. Requires a clean working
tree; a no-op when there are no fragments.

`tag plan` lists releasable packages whose `gleam.toml` version has no
`{name}-v{version}` tag yet; `tag create` creates the missing tags in
topological order, optionally pushing them and creating GitHub Releases (via
the `gh` CLI) with the matching CHANGELOG section as the body.

`publish` runs, per package and in dependency order: an idempotency check
against the Hex API (already-published versions are skipped, so re-running a
partially failed release is safe), validation (`gleam format --check`,
`build --warnings-as-errors`, `test`), then a path-dep rewrite computed from
the graph — each workspace path dep becomes the Hex requirement derived from
that dep's current version (`caret` or `exact`, per `path-dep-requirement`) —
followed by `gleam publish --yes`, and finally restoration of the original
`gleam.toml` (the repo never shows rewritten files, even on failure). Every
Hex-touching step runs under the configured `[tools.trellis.publish] retry` backoff policy.
`--tag lat_core-v1.2.0` resolves a pushed tag to its package and refuses to
publish if the tag version doesn't match `gleam.toml`; `--all-untagged`
publishes everything not yet on Hex, enabling a single publish run per release
instead of one per tag.

`lockfile refresh` scopes `gleam deps download` to one package (with retry),
encoding the "don't refresh the whole workspace or you'll get rate-limited"
rule as behavior. `trellis ci tag-package <tag>` resolves `$GITHUB_REF_NAME`
to a package name for shell substitution.

### Validation

```
trellis doctor
```

Checks every workspace invariant and reports all problems at once: member
globs resolve and parse, path deps stay inside the workspace, the graph is
acyclic, `ignore-release` globs match real members, no releasable package
depends on an unreleasable one, tag formats don't collide, `manifest.toml`
locked versions match workspace-internal `gleam.toml` versions, no package's
version is behind its CHANGELOG, and every unreleased changelog fragment
parses and references a valid package and kind. When `.tool-versions` pins
gleam, a mismatched gleam on PATH is reported as an advisory warning
(enforcing toolchains stays mise/asdf's job). Non-zero exit on any error —
run it on every PR.

### Scaffolding

```
trellis new <name> [--template lib] [--path <dir>]
```

Creates the member directory (derived from where existing members live, e.g.
`packages/<name>`), a `gleam.toml` pre-filled from a sibling's metadata
(gleam constraint, licences, repository, gleam_stdlib/gleeunit
requirements), a stub module and gleeunit test, a CHANGELOG, and a README.
There is no registration step anywhere: membership, the dependency graph,
and the changelog engine all derive from the files just written. It refuses
names that don't match any members glob, so a new package can never be
silently invisible to the workspace.

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

## Releasing trellis

Releases are fully automated, fragment-driven, and hands-off after merge —
the same pipeline as [repoverlay](https://github.com/tylerbutler/repoverlay):

1. Every user-facing change lands with a changie fragment (`changie new`);
   fragments accumulate in `.changes/unreleased/`.
2. On each push to `main`, `changie-release.yml` batches the fragments into a
   release PR that bumps `Cargo.toml`, regenerates `Cargo.lock`, and updates
   `CHANGELOG.md`.
3. Merging the release PR triggers `release-plz.yml`, which creates the
   `v{version}` tag (crates.io publishing is disabled — the `trellis` crate
   name is taken by an unrelated project).
4. The tag triggers the dist-generated `release.yml`: cargo-dist builds
   binaries for five targets (Linux gnu, macOS, Windows; x86_64 and aarch64),
   generates the shell/PowerShell installers and the Homebrew formula,
   attaches SLSA provenance attestations, and creates the GitHub Release.
   `publish-homebrew-tap.yml` then pushes the formula to
   `tylerbutler/homebrew-tap` using a GitHub App token.

The release workflows expect the `RELEASE_APP_ID` / `RELEASE_APP_PRIVATE_KEY`
secrets (a GitHub App with `contents:write` here and on the tap). After
changing `dist-workspace.toml`, regenerate the release workflow with
`dist generate` and validate with `dist plan`.

## License

MIT — see [LICENSE](LICENSE).
