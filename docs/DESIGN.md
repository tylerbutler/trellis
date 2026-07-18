# Trellis — a workspace CLI for Gleam monorepos

**Status:** Phases 1–4 implemented (see §10)
**Working name:** `trellis` — a trellis is the frame a lattice grows on. Subject to bikeshedding.

## 1. Background

Gleam has no native workspace concept. `gleam build`, `gleam test`, `gleam publish`
all operate on a single package directory. A multi-package monorepo like lattice
therefore hand-builds every workspace capability out of bash loops, YAML glue, and
duplicated config. This repo is the reference example of what that costs.

### Inventory of manual wiring in lattice

| Where | What is hand-maintained | Failure mode |
|---|---|---|
| `justfile:18` | Package list, in topological order, as a space-separated string | New package silently excluded from every recipe; ordering rots |
| `justfile` (throughout) | ~15 near-identical `for pkg in {{ packages }}` bash loops | Copy-paste drift between recipes; strictly serial execution |
| `.changie.yaml:50-137` | One project block per package (label, key, changelog path, version-replacement regex) | Forgotten block means a package cannot be versioned or released |
| `.github/workflows/publish.yml:83-92` | `replace-path-deps` name→`gleam.toml` map — a hand-written mirror of the path-dependency graph | Must be updated whenever any package gains a path dep; nothing verifies it |
| `.github/workflows/release.yml:38-62` | 25 lines of inline `sed`/`grep` that patch `manifest.toml` locked versions after version bumps (to avoid `gleam update` tripping Hex rate limits) | Untestable regex logic living inside YAML |
| `.github/workflows/publish.yml:41-65` | Inline retry-with-backoff shell function for Hex rate limits, plus comments encoding *which* gleam commands are safe to run | Institutional knowledge stored in workflow comments |
| `tylerbutler/actions/read-gleam-workspace` | External action that parses `workspace.toml` into projects / version-files / tag→path outputs | Workspace semantics live in a second repo, pinned by SHA in four workflows |
| `DEV.md`, `justfile` header comment | Dependency order documented as prose | Already stale relative to the real graph |
| `examples/` | Special-cased by hand in `format`, `lint`, and its own recipes | Every new "non-package project" needs bespoke recipe edits |

Everything in that table is *derivable* from one file format that already
exists: `gleam.toml` — a `[tools.trellis]` table in the workspace root's
manifest (member globs), plus each member's manifest (name, version, path
dependencies). The design principle of this tool is therefore:

> **Configure nothing that can be derived. Verify anything that must be duplicated.**

## 2. Goals

1. **One binary replaces the glue.** Task fan-out (justfile loops), workspace
   introspection (`read-gleam-workspace`), publish orchestration (`gleam-publish`
   action's path-dep rewriting), and lockfile patching (release.yml bash) become
   subcommands of a single tool that runs identically locally and in CI.
2. **The dependency graph is computed, never declared.** Topological order, publish
   order, `--since` change impact, and path-dep rewrite maps all come from parsing
   `gleam.toml` files.
3. **Generic.** Nothing lattice-specific: any repo with a `[tools.trellis]`
   table in its root `gleam.toml` and `packages/*/gleam.toml` members gets the
   same behavior. Lattice is the first consumer, not the target.
4. **CI-native.** Structured (JSON) output for GitHub Actions matrices and outputs,
   so the four workflows shrink to thin triggers.
5. **Fail loudly on drift.** A `doctor` command validates every invariant that today
   is enforced only by hope.

### Non-goals

- **Not a build system.** No caching, no incremental compilation, no artifact
  hashing. `gleam` does the building; trellis decides *where* and *in what order*
  to run it. (If Gleam ever grows native workspaces, trellis's task layer should
  become obsolete and its release layer should still work.)
- **Not a changelog engine (initially).** changie's project mode works well; we
  wrap and generate for it rather than reimplement it. See §7.
  *(Revised pre-release: trellis now IS the changelog engine — §7 explains
  why the wrap was retired before it shipped.)*
- **Not a general task runner.** `just` remains fine for repo chores unrelated to
  the workspace (the justfile shrinks; it doesn't have to die).

## 3. Design overview

```
      gleam.toml [tools.trellis]       packages/*/gleam.toml
                  │                            │
                  └────────┬───────────────────┘
                           ▼
                  ┌─────────────────┐
                  │ workspace model │  members, graph, versions
                  └────────┬────────┘
        ┌──────────┬───────┼─────────┬──────────────┐
        ▼          ▼       ▼         ▼              ▼
     run/exec   list/graph  version  publish      doctor
     (tasks)    (introspect) (bump+  (rewrite,    (invariant
                             lockfix) order,retry) checks)
```

A single Rust binary (`trellis`, see §9 for the language decision). Every command
starts by loading the **workspace model**:

1. Find the workspace root by walking up from the current directory to the
   first `gleam.toml` with a `[tools.trellis]` table (so commands work from
   inside a package, like `git` or `cargo` — member manifests along the way
   are skipped, and a `[tools.trellis]` table in a member manifest is a
   doctor error because it would hijack this walk). When no manifest up the
   tree has the table, trellis runs configless: the enclosing git repository
   root becomes the workspace root with an entirely defaulted configuration.
   An unparseable ancestor manifest blocks this fallback — it may be the
   intended root hiding behind a syntax error, and guessing would silently
   change discovery modes.
2. Expand `members` globs into package directories; parse each `gleam.toml` for
   `name`, `version`, and dependencies. When `members` is omitted (or there is
   no config at all), members are auto-discovered instead: every non-gitignored
   `gleam.toml` in the repository — tracked or untracked, outside `build/` —
   marks a member, and the reserved `@members` exclusion key removes
   directories from membership entirely (committed test fixtures, say, which
   gitignore cannot hide). A config-only root manifest (no `name`) is never
   itself a member.
3. Build the dependency graph from path dependencies between members. Reject cycles
   and path deps that point outside the workspace with a clear error.
4. Compute the topological order once; every other command consumes it.

## 4. Configuration

The `[tools.trellis]` table of the root `gleam.toml` is the single source of
configured truth, and stays small. There is no separate config file: the
workspace marker lives in the manifest format the ecosystem already uses
(the root manifest may be config-only or also a regular package). The table
itself is optional — configuration is a graduated ladder: no config at all
(git-inferred root, auto-discovered members), a table without `members`
(auto-discovery plus exclusions, tasks, and release settings), or explicit
`members` globs pinning the set down. Schema (everything optional, with the
defaults shown; omitting `members` auto-discovers, an explicitly empty list
is an error):

```toml
# gleam.toml at the workspace root
[tools.trellis]
members = ["packages/lattice_*", "examples/*"]
# Glob arrays matched against member paths and scoped by task. The reserved
# @release key covers changelog, versioning, tagging, and publishing; the @
# prefix keeps special keys from ever colliding with a task name.
exclude = { docs = ["examples/*"], "@release" = ["examples/*"] }

# Custom tasks, available to `trellis run <name>`. Built-in verbs (build, test,
# check, format, docs, deps, clean) need no declaration.
[tools.trellis.tasks.lint]
command = "gleam run -m glinter"
needs-deps = true            # run `gleam deps download` first if not cached

[tools.trellis.publish]
tag-format = "{name}-v{version}"      # lattice_core-v1.1.0
# How a path dep is rewritten to a Hex requirement at publish time, from the
# dependency's current version X.Y.Z:
#   minor  → ">= X.Y.Z and < (X+1).0.0"   (default)
#   patch  → ">= X.Y.Z and < X.(Y+1).0"
#   exact  → "== X.Y.Z"
path-dep-requirement = "minor"
retry = { attempts = 5, initial-delay = "30s", multiplier = 2 }

[tools.trellis.changelog]
# Native engine (§7): fragments in <dir>/unreleased/, version sections in
# <dir>/<package>/, per-package CHANGELOG.md assembled from them. All
# format values are minijinja templates. Everything below is the default.
dir = ".changes"
header-format = "# {{ name }} changelog"
version-format = "## v{{ version }} - {{ date }}"
kind-format = "### {{ kind }}"
change-format = "- {{ body }}"
kinds = [
  { label = "Initial Release", bump = "major" },
  { label = "Breaking", bump = "major" },
  { label = "Removed", bump = "major" },
  { label = "Added", bump = "minor" },
  { label = "Changed", bump = "minor" },
  { label = "Deprecated", bump = "minor" },
  { label = "Fixed", bump = "patch" },
  { label = "Performance", bump = "patch" },
  { label = "Security", bump = "patch" },
]
```

Notably absent, because derived: package lists, dependency order, per-package
changelog wiring, version-file maps, path-dep rewrite maps, tag→package
mappings.

## 5. Command surface

### Introspection

```
trellis list [--json] [--since <ref>] [--with-dependents] [--releasable]
trellis graph [--format text|dot|mermaid|json]
trellis info <package> [--json]
```

- `list` prints members in topological order — this alone replaces `justfile:18`.
  `--releasable` filters out `@release` matches, i.e. the set that
  changelog/tag/publish commands operate on.
- `--since origin/main` filters to packages owning changed files (diff paths mapped
  to package directories); `--with-dependents` adds the reverse-dependency closure.
  This is the primitive behind "only test what a PR touched."
- `graph --format mermaid` keeps DEV.md's dependency diagram generated instead of
  hand-drawn.

### Task running

```
trellis run <task> [pkgs...] [--since <ref>] [--target erlang|javascript|all]
                   [--strict] [--serial] [--keep-going]
trellis exec [pkgs...] -- <command...>
```

- Built-in tasks map 1:1 onto gleam verbs: `build`, `test`, `check`, `format`
  (`--check` variant), `docs`, `deps`, `clean`. Custom tasks come from `[tasks]`.
  Any built-in or custom task may have a same-named entry under
  `[tools.trellis.exclude]`; excluded member-path globs are removed after normal
  CLI package selection. The reserved `exclude.@release` key defines the
  releasable set; its `@` prefix keeps it from ever colliding with a task name
  (task names may not start with `@`).
- Scheduling is **graph-parallel by default**: a package runs as soon as its
  workspace deps have finished, up to `--jobs N`. Output is streamed with a
  `pkg ▏` prefix, followed by a summary table. The justfile's serial loops become
  the `--serial` fallback.
- `--target all` runs the task once per target, replacing the `*-js` recipe
  duplication (`test-js`, `build-strict-js`, …).
- Compound flows stay in just as one-liners, e.g.
  `ci: trellis run format --check && trellis run check && trellis run lint && trellis run test && trellis run build --strict`.

What this replaces: every bash loop in the justfile (~180 of its 240 lines).

### Changelog & versioning

```
trellis changelog new [--package <pkg>] --kind <kind> --body <text>
trellis changelog check --base <sha> --head <sha> [--json]
trellis version plan [--json]                     # dry-run: what would be bumped
trellis version apply                             # batch + merge + lockfile patch
```

- The engine is native (§7). Fragments are TOML files in
  `.changes/unreleased/` (`project`, `kind`, `body`); `changelog new` writes
  one non-interactively. There is no per-package changelog wiring to
  generate or keep in sync — the lattice failure mode of "forgotten config
  block means a package cannot be released" has no equivalent, because there
  is no config block.
- `changelog check` replaces the changie-check glue: map the base..head diff to
  packages, decide which need fragments, emit JSON (`has-entries`, `needs-entry`,
  `preview`, per-package detail) for the PR workflow's sticky comment. Invalid
  fragments (unknown package or kind, empty body) fail the check.
- `version apply` is the release step: per pending package, compute the next
  version from the fragments' kinds (largest bump wins), render the version
  section (minijinja), store it under `.changes/<package>/`, reassemble the
  package's CHANGELOG.md newest-first, bump `gleam.toml` with a surgical
  toml_edit patch (no regex replacements), then **patch `manifest.toml` locked
  versions of workspace-internal deps directly** — the exact logic of release.yml's
  `post-batch-command`, but implemented as tested code with a TOML parser instead
  of `sed`, and still zero Hex network calls (that constraint is load-bearing:
  `gleam update` per package trips Hex rate limits on shared runners).

### Release & publish

```
trellis tag plan [--json]        # packages whose gleam.toml version has no tag yet
trellis tag create [--github-release]
trellis publish <pkg | --tag <tag> | --all-untagged> [--dry-run]
trellis lockfile refresh [--package <pkg>]
```

- `tag plan/create` replaces the auto-tag workflow's core: compare each
  releasable member's
  `gleam.toml` version against existing `{name}-v{version}` tags, create missing
  tags in topological order, optionally create GitHub Releases with the matching
  CHANGELOG section as the body.
- `publish` performs, per package:
  1. **Idempotency check** — query Hex once; skip if this exact version is already
     published (makes re-runs of a partially failed release safe).
  2. **Validate** — `gleam format --check`, `gleam build --warnings-as-errors`,
     `gleam test` per configured target; each Hex-touching step wrapped in the
     configured retry/backoff policy (publish.yml's inline `retry()` becomes a
     library function).
  3. **Rewrite path deps** — for each workspace-internal path dep, substitute the
     Hex requirement derived from that dep's *current* `gleam.toml` version per
     `path-dep-requirement`. The rewrite map is computed from the graph — the
     hand-maintained `replace-path-deps` list in publish.yml disappears.
  4. **Publish** — `gleam publish --yes`, with retry/backoff.
  5. **Restore** — put the original `gleam.toml` back (rewrite happens in a temp
     copy or is reverted; the repo never shows rewritten files).
- `--tag lattice_core-v1.2.0` resolves a pushed tag to (package, path, version) and
  refuses to publish if the tag version ≠ `gleam.toml` version — this replaces
  `read-gleam-workspace`'s tag mapping *and* adds the missing validation.
- `--all-untagged` publishes every package whose version isn't on Hex yet, in
  topological order — enables collapsing per-tag publishes into one run (§8).
- `lockfile refresh` scopes `gleam deps download` to the just-published package,
  encoding the "don't refresh the whole workspace or you'll get rate-limited"
  rule from publish.yml:124-133 as behavior instead of a comment.

### CI glue

```
trellis ci matrix [--since <ref>] [--json]   # {"include":[{"name","path","version"},…]}
trellis ci outputs                            # projects/version-files/etc. as GHA outputs
```

Emits the exact structures workflows consume, replacing every
`read-gleam-workspace` call site. `--since` gives affected-only CI matrices
for free.

### Validation

```
trellis doctor
```

Checks, each of which is an unenforced invariant in lattice today:

1. Member globs resolve to ≥1 directory; every member has a parseable `gleam.toml`.
2. Every path dep between members stays inside the workspace; graph is acyclic.
3. Unreleased changelog fragments parse and reference a releasable package and
   a configured kind. (Originally ".changie.yaml projects are current, --fix
   regenerates" — the native engine deleted the generated file entirely.)
4. Each releasable member's `gleam.toml` version ≥ its latest CHANGELOG version,
   and each has a changelog file where expected.
5. `manifest.toml` locked versions of workspace-internal deps match those deps'
   actual `gleam.toml` versions (catches a missed lockfile patch).
6. Every task exclusion glob matches at least one member (catches typos), and
   no releasable member path-depends on a release-excluded member — a published
   package cannot require a project that will never exist on Hex.
7. Tag-format collisions (two members whose names would produce ambiguous tags).

`doctor` is the CI tripwire for the duplication that can't be eliminated. Today,
publish.yml's `replace-path-deps` being one package short would only be discovered
when a published package fails to resolve on Hex; under trellis the list doesn't
exist, and everything that still must be duplicated is checked on every PR.

### Scaffolding

```
trellis new <name> [--template lib]
```

Creates `packages/<name>` with a `gleam.toml` pre-filled from workspace metadata
(licence, repository, gleam constraint copied from siblings), a stub test, and
runs `changelog sync`. Adding a package becomes one command instead of edits to
five files.

## 6. The release pipeline, before and after

Current flow (five workflows, two external action repos):

```
PR merge → release.yml:  read-gleam-workspace → changie-release action
                          → 25 lines of inline bash to patch manifests
         → release PR
PR merge → auto-tag.yml:  external reusable workflow reads workspace.toml,
                          creates per-package tags, waits on Publish runs
tag push → publish.yml:   read-gleam-workspace maps tag→path,
                          inline retry bash validates,
                          gleam-publish action rewrites hand-listed path deps,
                          lockfile-refresh job opens a follow-up PR
```

With trellis, each workflow keeps its trigger and becomes a few commands:

```yaml
# release.yml (on push to main)
- run: trellis version apply            # batch, merge, patch lockfiles — no bash
- run: trellis release pr               # create-or-update the release PR via gh

# auto-tag.yml (on release-PR merge)
- run: trellis tag create --github-release

# publish.yml (on tag push '*-v*')
- run: trellis publish --tag "$GITHUB_REF_NAME"
- run: trellis lockfile refresh --package "$(trellis ci tag-package)"
```

An alternative worth considering once trellis exists: drop per-package tags as the
publish *trigger* entirely — on release-PR merge, run `trellis publish
--all-untagged` (idempotent, topologically ordered, one workflow run instead of N),
then `trellis tag create --github-release` to record what shipped. Tags become an
artifact of publishing rather than its trigger, and the auto-tag "wait for the
Publish workflow" coupling disappears. This is a policy change for lattice, not a
requirement of the tool; both shapes are supported.

## 7. Interop decisions

**Changie is subsumed, not wrapped.** The original decision was "wrap changie,
don't replace it (initially)", with a native fragment engine as the escape
hatch if the two-tool dependency chafed. It chafed before it ever shipped:
the wrap needed a generated `.changie.yaml` projects section plus a doctor
drift-check whose only purpose was telling changie things trellis already
knew; `changie next`'s output had to be parsed defensively; version bumps ran
through user-supplied regex "replacements" where trellis has a real TOML
editor; and every consuming workspace had to install a second binary in CI.
Since trellis was pre-release, the native engine slotted in behind the same
`trellis changelog`/`version` commands with no compatibility burden:

- Fragments are TOML (`project`, `kind`, `body`) in `.changes/unreleased/` —
  consistent with everything else trellis reads, and validated by `doctor`
  on every PR (an invalid fragment can't hide until release time).
- Version bumps derive from the kinds' configured `bump` (largest wins);
  `gleam.toml` is bumped with toml_edit, not regex.
- Rendered version sections live under `.changes/<package>/`; each package's
  CHANGELOG.md is a generated file reassembled from them, newest first.
- All formats are minijinja templates with a small context, so rendering
  stays user-configurable without a second tool or a Go-template engine.

(This applies to the workspaces trellis manages. Trellis's own repo releases
via the changie-release/release-plz/cargo-dist pipeline, which is a separate
concern and unaffected.)

**The justfile survives as an interface, not an implementation.** Recipes become
one-line delegations (`test *ARGS: (trellis run test {{ARGS}})`), preserving muscle
memory and `just --list` discoverability while deleting the loops. Repo-level
chores that aren't workspace fan-out stay pure just.

**`read-gleam-workspace` / `gleam-publish` actions are superseded.** The reusable
CI workflow (`gleam-workspace-ci.yml`) can be reduced to setup + `trellis run …`,
which also fixes today's asymmetry where local runs are serial-bash and CI runs are
a separately-implemented matrix. The composite setup action gains a
`taiki-e/install-action`-style step that installs the trellis release binary.

## 8. CI matrix example

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.plan.outputs.matrix }}
    steps:
      - uses: actions/checkout@…
      - id: plan
        run: echo "matrix=$(trellis ci matrix --since origin/main)" >> "$GITHUB_OUTPUT"

  test:
    needs: plan
    strategy:
      matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}
    steps:
      - uses: actions/checkout@…
      - run: trellis run test ${{ matrix.name }} --target all
```

## 9. Implementation notes

**Language: Rust.** The tool must install in CI in ~1s and run with zero runtime
deps, which means prebuilt static binaries — the same distribution model as `just`,
`changie`, and `ratchet` already in this stack. Rust additionally gives first-class
TOML round-tripping (`toml_edit`, needed for surgical `gleam.toml`/`manifest.toml`
patches that don't reformat the file), a mature graph crate (`petgraph`), and easy
parallel subprocess management. The romantic alternative — writing it in Gleam —
founders on distribution: a Gleam CLI needs an Erlang VM or Node on the machine
before the workspace tool that sets up the toolchain can run.

Key crates: `clap` (CLI), `toml_edit` (lossless TOML), `petgraph` (graph +
toposort + cycle detection), `globset`, `serde_json` (CI output), `ureq` or
`reqwest` (Hex API for idempotency checks).

**Hex interaction budget** is a first-class design constraint (three workflow
comment blocks in this repo exist because of it): trellis never runs a
Hex-resolving gleam command when it can edit a TOML file locally, batches what it
must, and applies the configured retry policy to everything else.

**Exit codes / output contract:** human-readable to TTY, `--json` everywhere for
scripting; non-zero exit on any package failure with a final summary table naming
failures (the current bash loops abort on first failure with no summary).

**Testing:** the workspace model and rewrite/patch logic are pure functions over
TOML fixtures — unit-testable, unlike their current YAML-embedded equivalents. An
end-to-end suite runs against a fixture workspace with a mocked Hex API.

## 10. Rollout in lattice

1. **Phase 1 — introspection + tasks.** `list`, `graph`, `run`, `exec`, `doctor`.
   Justfile recipes delegate. CI keeps its shape but drops the reusable-workflow
   matrix plumbing. Low risk; immediately deletes the hardcoded package list.
   **Status: implemented in this repo** (plus `info` and `ci matrix`/`ci outputs`,
   which fall out of the workspace model for free). Deviations from this document:
   the graph layer uses a hand-rolled Kahn's algorithm with an alphabetical
   tie-break instead of `petgraph` (deterministic output, one less dependency),
   and the read-only phase uses `toml` for parsing — `toml_edit` enters with the
   first command that patches files (phase 2's lockfile patch).
2. **Phase 2 — changelog + version.** `changelog check/new`, `version
   plan/apply`. Deletes `.changie.yaml` hand-maintenance, release.yml's inline
   bash, and the changie-check glue.
   **Status: implemented, then revised.** The first implementation wrapped
   changie (generated `.changie.yaml` projects, shelled out to
   `next`/`batch`/`merge`, `TRELLIS_CHANGIE_BIN` override for tests). Before
   release, the wrap was replaced by the native engine described in §7:
   `changelog sync` and its doctor drift-check ceased to exist (nothing left
   to generate), fragments became TOML, bumps derive from configured kinds,
   rendering is minijinja, and `gleam.toml` is bumped with `toml_edit`.
   `version apply` still verifies after batching that every `gleam.toml`
   received its new version, then patches lockfiles with `toml_edit` — zero
   Hex calls, formatting preserved. Invalid fragments fail
   `plan`/`apply`/`check` (and `doctor`) loudly.
3. **Phase 3 — release + publish.** `tag`, `publish`, `lockfile refresh`,
   `ci matrix/outputs`. Retires `read-gleam-workspace` and `gleam-publish` call
   sites. Optionally move to the tags-after-publish flow (§6).
   **Status: implemented.** Notes: the Hex idempotency check uses `ureq`
   (one GET per package, base URL overridable via `TRELLIS_HEX_API_URL` — the
   e2e suite runs against a local mock). Publish validation runs against the
   *original* manifest (path deps intact); the rewrite is computed up front so
   an unpublishable package fails before validation wastes time, and a drop
   guard restores `gleam.toml` even when publishing fails. Dev-only path deps
   to unreleasable members are left alone (Hex doesn't ship dev deps); a
   `[dependencies]` path dep to an unreleasable member refuses to publish.
   `tag create --github-release` implies pushing the tag first and shells out
   to `gh` (`TRELLIS_GH_BIN` overridable), with the release body extracted
   from the member's CHANGELOG section for that version. `ci tag-package`
   (used in §6's publish workflow sketch) resolves `$GITHUB_REF_NAME` to a
   package name. The retry policy from `[publish] retry` wraps every
   Hex-touching step (`with_retry`, exponential backoff). The gleam binary is
   `TRELLIS_GLEAM_BIN`-overridable, which the e2e suite uses to drive publish
   end-to-end with a fake gleam.
4. **Extract.** Once stable in lattice, move to its own repo and publish binaries;
   lattice pins a version in `.tool-versions` like every other tool.
   **Status: implemented** (trellis was built in its own repo from the start,
   so extraction reduces to distribution). The publishing pipeline mirrors
   tylerbutler/repoverlay's: changie fragments (`.changes/unreleased/`) →
   `changie-release.yml` opens a release PR bumping `Cargo.toml` +
   `CHANGELOG.md` + `Cargo.lock` on every push to main → merging it triggers
   `release-plz.yml`, which creates the `v{version}` tag and publishes the
   crate to crates.io as `trellis-gleam` (the `trellis` name itself is taken
   by an unrelated 2016 crate — §11's naming question, answered by the
   registry) → the tag triggers the dist-generated `release.yml`, where
   cargo-dist builds five targets, generates shell/PowerShell installers and
   a Homebrew formula, attaches SLSA provenance attestations, and creates the
   GitHub Release → `publish-homebrew-tap.yml` (a custom dist publish-job)
   pushes the formula to tylerbutler/homebrew-tap with a GitHub App token.
   Consuming workspaces install via `cargo install trellis-gleam`, the dist
   shell installer, or a pin in `.tool-versions` through mise/asdf's ubi
   backend. Requires the shared `RELEASE_APP_ID` / `RELEASE_APP_PRIVATE_KEY`
   secrets plus a `CARGO_REGISTRY_TOKEN` for crates.io publishing.
   repoverlay's SBOM/attestation workflow (`release-sbom.yml`) is not adopted
   yet — it depends on that repo's local composite actions.

Beyond the numbered phases, the rest of the §5 command surface is also
implemented: `trellis new` (scaffolding, with metadata copied from a sibling
member and a members-glob match check so a new package can't be invisible to
the workspace) and `trellis release pr` (see question 2 in §11). Two
pre-release revisions of this document's original proposals are recorded in
place: changie subsumed by the native changelog engine (§7), and the
separate `workspace.toml` replaced by the `[tools.trellis]` table in the
root `gleam.toml` (§4).

## 11. Open questions — resolved

1. **Name.** ~~`trellis` may collide with Roots' WordPress tool; alternatives:
   `gws`, `latwork`, `gleamspace`.~~
   **Resolved: keep `trellis` as the binary/repo name.** The crates.io crate
   name is taken by an unrelated 2016 project, so the package publishes to
   crates.io as `trellis-gleam` (`publish = true` in `release-plz.toml`) while
   a `[[bin]]` entry in `Cargo.toml` keeps the installed binary named
   `trellis`. Distribution also continues via cargo-dist binaries, the
   Homebrew tap, and mise/asdf's ubi backend.
2. **Scope of `version apply` vs. the `changie-release` action.** ~~Keep PR
   management in the action, or absorb into `trellis release pr`?~~
   **Resolved: absorbed.** `trellis release pr` computes the plan, runs
   `version apply` on a release branch, force-pushes it (create-or-update
   semantics), and drives `gh pr create`/`gh pr edit` with a bump table and
   per-package CHANGELOG sections in the body. (With the native changelog
   engine of §7, `trellis release pr` is the only release-PR path for gleam
   workspaces — the changie-release action drives changie, which trellis no
   longer uses.)
3. **Affected-only CI as default.**
   **Resolved: full fan-out stays the default.** `--since` is opt-in for
   `list`/`run`/`exec`/`ci matrix` — the safety trade-off (implicit coupling
   between packages that the path-dep graph can't see) shouldn't be silent.
   A repo that wants affected-only CI writes `--since origin/main` into its
   workflow deliberately.
4. **Should trellis own `.tool-versions` awareness?**
   **Resolved: advisory only.** `doctor` warns when `.tool-versions` pins a
   gleam version different from the gleam on PATH, but never errors —
   installing and enforcing toolchains remains mise/asdf's job.
