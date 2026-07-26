# trellis

![Crates.io Version](https://img.shields.io/crates/v/trellis-gleam) ![GitHub Release Date](https://img.shields.io/github/release-date/tylerbutler/trellis?display_date=published_at)

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
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tylerbutler/trellis/releases/latest/download/trellis-gleam-installer.sh | sh
```

**PowerShell installer** (Windows):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/tylerbutler/trellis/releases/latest/download/trellis-gleam-installer.ps1 | iex"
```

**Homebrew:**

```sh
brew install tylerbutler/tap/trellis
```

**mise / asdf** (via the
[github](https://mise.jdx.dev/dev-tools/backends/github.html) backend), which
is how a consuming workspace pins trellis in `.tool-versions` alongside its
other tools:

```sh
mise use "github:tylerbutler/trellis@0.2.0"
```

**From source:**

```sh
cargo install --git https://github.com/tylerbutler/trellis
```

Prebuilt archives for every target are on the
[releases page](https://github.com/tylerbutler/trellis/releases). Pin a
specific version in CI by replacing `latest/download` with
`download/v0.1.0` in the installer URL.

### Shell completions

Add one line to your shell's startup file — `bash`, `zsh`, `fish`,
`powershell`, and `elvish` are supported:

```sh
eval "$(trellis completions zsh)"
```

Completions are computed by the binary as you type, so they offer real package
names, task names, and changelog kinds from the workspace you're in. Evaluate
the snippet on startup rather than saving it to a file: it talks to `trellis`
over an interface that can change between releases. See
[installation](https://trellis.tylerbutler.com/docs/installation) for the other
shells and for man pages, which ship in the release archives under `man/`.

### Update checks

Interactive commands print a one-line notice to stderr when a newer trellis
has been published to crates.io. The check is best-effort — cached for a day,
capped at a short timeout, and silent on any error — so it never slows a
command or changes its exit status. It runs only when stderr is a terminal, so
scripts and structured output are never touched. It is additionally skipped in
CI and when `DO_NOT_TRACK` or `TRELLIS_NO_UPDATE_CHECK` is set in the
environment, and `--no-update-check` suppresses it for a single invocation.

## Configuration

Configuration is optional. With no configuration at all, the git repository
root is the workspace root and every non-gitignored `gleam.toml` (outside
`build/`) marks a member — a fresh Gleam monorepo, or a single-package repo,
works with zero setup.

When you need to configure something, a `[tools.trellis]` table in a
`gleam.toml` marks the workspace root — no separate config file. The root
manifest may be config-only, or a regular gleam package that also anchors the
workspace. Every key is optional, including `members`: omit it to keep
auto-discovering members from git while configuring everything else:

```toml
# gleam.toml at the repo root
[tools.trellis]
# Optional: pin membership to explicit globs instead of auto-discovery.
members = ["packages/*", "examples/*"]
# Exclusions are globbed against member paths and scoped by task. The
# reserved `@release` key covers changelog, versioning, tagging, and
# publishing; `@members` removes directories from membership entirely
# (e.g. committed test fixtures that auto-discovery would sweep in); the
# `@` prefix keeps them from ever colliding with a task name.
exclude = { docs = ["examples/*"], "@release" = ["examples/*"] }

# Custom tasks for `trellis run <name>`. Built-in verbs (build, test, check,
# format, docs, deps, clean) need no declaration.
[tools.trellis.tasks.lint]
command = "gleam run -m glinter"
needs-deps = true            # run `gleam deps download` first if not cached

[tools.trellis.publish]
tag-format = "{name}-v{version}"
# A moving tag per release series, for consumers who pin a series instead of
# chasing patch tags. {series} is derived from the version: `0.Y` while the
# major is 0, `X` after. `tag-mode` is exact (default), series, or both.
series-tag-format = "{name}-v{series}"
tag-mode = "exact"
tag-mode-overrides = { both = ["packages/lat_cli"] }
```

Each member is a directory with a `gleam.toml`. Path dependencies between
members define the graph; cycles and path deps escaping the workspace are
rejected, and a `[tools.trellis]` table in a *member* manifest is a doctor
error (it would hijack root discovery).

Member entries containing `*`, `?`, or `[` are wildcard patterns. In a Git
repository, wildcard discovery honors repository `.gitignore` files at every
level and `.git/info/exclude`. It does not read global `core.excludesFile`
rules, generic `.ignore` files, or automatically exclude hidden paths, so
results do not depend on machine-local Git configuration. Outside a Git
repository, Git ignore rules do not apply. Traversal follows symlinks but never
enters `.git`, and only matching directories with a `gleam.toml` become members.

Entries without wildcard metacharacters are resolved directly, so an explicit
literal path remains included even when Git ignores it. `[tools.trellis.exclude]`
is a separate post-discovery filter: task and `@release` exclusions do not
control traversal.

## Commands

Every command works from anywhere inside the workspace (the root is found by
walking up to the first `gleam.toml` with a `[tools.trellis]` table, like
`git` or `cargo` — member manifests along the way are skipped). Without a
`[tools.trellis]` table anywhere, the git repository root is the workspace
root and members are auto-discovered.

### Global flags

```
-C, --directory <DIR>    Run as if started in this directory
    --color <WHEN>       auto (default), always, or never
-q, --quiet              Drop the per-package stream and the summary table
-v, --verbose            Trace every command trellis shells out to, on stderr
    --no-update-check    Skip the release check for this invocation
```

These accept placement anywhere, before or after the subcommand.

`--color auto` follows the terminal and respects `NO_COLOR` (at any value) and
`CLICOLOR=0`; `always` and `never` override that detection in either direction,
so `--color always` survives a pipe. Live progress rows stay tied to terminal
detection either way — forcing color into a pipe does not start drawing
spinners into it.

`-q` and `-v` are mutually exclusive. `-q` changes nothing about exit codes:
errors still go to stderr and a failing task still fails the command. `-v`
writes a `+ ` trace to stderr for each `gleam`, `git`, and `gh` invocation,
naming the directory it ran in.

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

#### JSON output

Every `--json` payload carries a `schema` field naming the payload and its
major version (`"schema": "trellis.list/1"`), so a workflow can assert on the
shape it was built against and detect a breaking change instead of silently
producing wrong results. Fields may be added without a bump; renaming,
removing, or retyping one bumps the major. Human-readable text output carries
no such guarantee. `trellis ci matrix` and `ci outputs` are the two
exceptions — their shapes are dictated by GitHub Actions.

Full details: [docs/json-output](https://trellis.tylerbutler.com/docs/json-output/).

### Task running

```
trellis run <task> [pkgs...] [--since <ref>] [--with-dependents]
                   [--target erlang|javascript|all] [--strict] [--check]
                   [--serial] [--keep-going] [--jobs N] [--json]
trellis exec [pkgs...] [--since <ref>] [--serial] [--keep-going] [--json]
             -- <command...>
```

Scheduling is graph-parallel by default: a package runs as soon as its
workspace dependencies have finished, up to `--jobs N` at once. Output is
streamed with a `pkg ▏` prefix and a summary table names any failures. In an
interactive terminal, active packages remain visible as live progress rows and
package names use stable, hash-derived colors. Pipes and CI receive plain text
without terminal control sequences.
`--target all` runs the task once per compile target. `--serial` runs one
package at a time in dependency order.

`--json` reports per-package results — status, exit code, duration, and the
command that failed — so a workflow can record what passed without parsing
output written for a person. Package output moves to stderr so stdout carries
nothing but the payload, and the summary table is replaced by it. This is what
makes "only test what a PR touched" reportable as well as doable:

```
trellis run test --since origin/main --json | jq -r '.results[] | select(.status != "success") | .package'
```

Built-in tasks map 1:1 onto gleam verbs: `build`, `test`, `check`, `format`
(`--check` variant), `docs`, `deps`, `clean`. A `[tools.trellis.tasks]` entry with the same
name overrides a built-in. Any built-in or custom task can exclude member paths
through its key in `exclude`; exclusions still apply when packages are named
explicitly. For larger maps, use a table:

```toml
[tools.trellis.exclude]
docs = ["examples/*", "packages/internal-*"]
"@release" = ["examples/*"]
```

The reserved `@release` key defines the package set used by changelog,
version, tag, and publish commands, and `@members` removes directories from
workspace membership entirely (in both auto-discovered and explicit-members
modes). Special keys use a `@` prefix so they can never collide with a task
name — trellis rejects any task named with that prefix.

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

A bump ripples to the bumping package's workspace dependents. When `lat_core`
goes 1.2.0 → 1.3.0, every package that path-depends on it is released too —
by a patch, and with a generated changelog entry saying why:

```
lat_core: 1.2.0 -> 1.3.0 (1 fragment(s))
lat_mid: 0.5.0 -> 0.5.1 (dependencies: lat_core)
lat_cli: 0.3.1 -> 0.3.2 (dependencies: lat_core, lat_mid)
```

This is not a convenience. A path dep's Hex requirement is derived from the
dependency's version at publish time, so leaving `lat_mid` at 0.5.0 would let
one published version resolve two different ways depending on whether it was
fetched before or after `lat_core`'s release. A dependent needs no fragment of
its own to ripple; a package that has one keeps its own, larger bump. Packages
excluded by `@release` never bump, and a ripple stops at one rather than
skipping past it to its dependents.

Rendering is controlled by minijinja templates in `[tools.trellis.changelog]`, each with a
small context (`name`, `version`, `date`, `tag`, `kind`, `body` as applicable):

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

# Generated ripple entries are ordinary entries of one configured kind, so
# they sort and render like any other. `dependency-kind` must name one of
# `kinds`; its bump is what a package bumps by when a dependency bump is the
# only reason it is being released.
dependency-kind = "Dependencies"                                  # default
dependency-body = "Updated {{ dependency }} to {{ dependency_version }}"  # default
```

Note that each package's CHANGELOG.md is a generated file: the source of
truth is the version sections under `.changes/<package>/`, and `apply`
reassembles the changelog from them. A package that already had a changelog
when it adopted trellis keeps it: on its first release, whatever sits below
the header is captured verbatim as one section under `.changes/<package>/`, so
regenerating preserves the history rather than replacing it. `doctor` reports
this before release day, and `doctor --fix` does the capture up front.

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

`tag plan` lists the tags the current versions call for and don't have yet;
`tag create` reconciles them in topological order, optionally pushing them and
creating GitHub Releases (via the `gh` CLI) with the matching CHANGELOG
section as the body.

A package tags in one of two lifecycles, per `tag-mode`. Exact tags
(`{name}-v{version}`) are immutable — created once, never rewritten. Series
tags (`{name}-v{series}`) move: each release force-moves the tag to the
release commit and force-pushes it. Because a moving tag names no particular
version, it never carries a GitHub Release and `publish --tag` refuses it;
`ci tag-package` still resolves it, and a `series`-only workspace publishes
with `--all-untagged`.

`publish` runs, per package and in dependency order: an idempotency check
against the Hex API (already-published versions are skipped, so re-running a
partially failed release is safe), validation (`gleam format --check`,
`build --warnings-as-errors`, `test`), then a path-dep rewrite computed from
the graph — each workspace path dep becomes the Hex requirement derived from
that dep's current version (`minor`, `patch`, or `exact`, per `path-dep-requirement`) —
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
trellis doctor [--fix] [--dry-run] [--format text|json|github]
```

Checks every workspace invariant and reports all problems at once: member
globs resolve and parse, path deps stay inside the workspace, the graph is
acyclic, task exclusion globs match real members, no releasable package
depends on an unreleasable one, tag formats don't collide, `manifest.toml`
locked versions match workspace-internal `gleam.toml` versions, no package's
version is behind its CHANGELOG, and every unreleased changelog fragment
parses and references a valid package and kind. When `.tool-versions` pins
gleam, a mismatched gleam on PATH is reported as an advisory warning
(enforcing toolchains stays mise/asdf's job). Non-zero exit on any error —
run it on every PR.

`--fix` applies the mechanical remedies (seed a missing CHANGELOG, patch stale
locked versions) and re-checks; `--dry-run` lists them without writing.
`--format json` emits each finding as `{check, severity, message, file,
package, fixable}`, and `--format github` emits the same findings as workflow
commands, so they annotate the changed lines in a PR instead of scrolling past
in a log:

```yaml
- run: trellis doctor --format github
```

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

`assets/man/` and `website/src/content/docs/docs/reference.md` are generated
from the clap definitions — regenerate both with `just docs` after changing any
command, flag, or help string. `cargo test` fails if they're stale.

## Releasing trellis

Releases are fully automated, fragment-driven, and hands-off after merge —
the same pipeline as [repoverlay](https://github.com/tylerbutler/repoverlay):

1. Every user-facing change lands with a changie fragment (`changie new`);
   fragments accumulate in `.changes/unreleased/`.
2. On each push to `main`, `changie-release.yml` batches the fragments into a
   release PR that bumps `Cargo.toml`, regenerates `Cargo.lock`, and updates
   `CHANGELOG.md`.
3. Merging the release PR triggers `release-plz.yml`, which creates the
   `v{version}` tag and publishes the crate to crates.io as `trellis-gleam`
   (the `trellis` name itself is taken by an unrelated project — the `[[bin]]`
   in `Cargo.toml` keeps the installed binary named `trellis`).
4. The tag triggers the dist-generated `release.yml`: cargo-dist builds
   binaries for five targets (Linux gnu, macOS, Windows; x86_64 and aarch64),
   generates the shell/PowerShell installers and the Homebrew formula,
   attaches SLSA provenance attestations, and creates the GitHub Release.
   `publish-homebrew-tap.yml` then pushes the formula to
   `tylerbutler/homebrew-tap` using a GitHub App token.

The release workflows expect the `RELEASE_APP_ID` / `RELEASE_APP_PRIVATE_KEY`
secrets (a GitHub App with `contents:write` here and on the tap), plus a
`CARGO_REGISTRY_TOKEN` secret (a crates.io API token with publish access to
`trellis-gleam`) for `release-plz.yml` to publish the crate. After changing
`dist-workspace.toml`, regenerate the release workflow with `dist generate`
and validate with `dist plan`.

## License

MIT — see [LICENSE](LICENSE).
