# Trellis — a workspace CLI for Gleam monorepos

**Status:** Phases 1–2 implemented (see §10)
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

Everything in that table is *derivable* from two sources that already exist:
`workspace.toml` (member globs) and each member's `gleam.toml` (name, version,
path dependencies). The design principle of this tool is therefore:

> **Configure nothing that can be derived. Verify anything that must be duplicated.**

## 2. Goals

1. **One binary replaces the glue.** Task fan-out (justfile loops), workspace
   introspection (`read-gleam-workspace`), publish orchestration (`gleam-publish`
   action's path-dep rewriting), and lockfile patching (release.yml bash) become
   subcommands of a single tool that runs identically locally and in CI.
2. **The dependency graph is computed, never declared.** Topological order, publish
   order, `--since` change impact, and path-dep rewrite maps all come from parsing
   `gleam.toml` files.
3. **Generic.** Nothing lattice-specific: any repo with a `workspace.toml` and
   `packages/*/gleam.toml` gets the same behavior. Lattice is the first consumer,
   not the target.
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
- **Not a general task runner.** `just` remains fine for repo chores unrelated to
  the workspace (the justfile shrinks; it doesn't have to die).

## 3. Design overview

```
             workspace.toml            packages/*/gleam.toml
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

1. Find `workspace.toml` by walking up from the current directory (so commands work
   from inside a package, like `git` or `cargo`).
2. Expand `members` globs into package directories; parse each `gleam.toml` for
   `name`, `version`, and dependencies.
3. Build the dependency graph from path dependencies between members. Reject cycles
   and path deps that point outside the workspace with a clear error.
4. Compute the topological order once; every other command consumes it.

## 4. Configuration

`workspace.toml` stays the single source of truth and stays small. Proposed schema
(everything except `members` optional, with the defaults shown):

```toml
[workspace]
members = ["packages/lattice_*", "examples"]
# Glob array matched against member paths. Matching members participate in all
# task fan-out (format/lint/build/test) like any other member, but are excluded
# from changelog, versioning, tagging, and publishing. Replaces the hand
# special-casing of examples/ in the justfile.
ignore-release = ["examples"]

# Custom tasks, available to `trellis run <name>`. Built-in verbs (build, test,
# check, format, docs, deps, clean) need no declaration.
[tasks.lint]
command = "gleam run -m glinter"
needs-deps = true            # run `gleam deps download` first if not cached

[publish]
tag-format = "{name}-v{version}"      # lattice_core-v1.1.0
# How a path dep is rewritten to a Hex requirement at publish time, from the
# dependency's current version X.Y.Z:
#   caret  → ">= X.Y.Z and < (X+1).0.0"   (default; matches current behavior)
#   exact  → "== X.Y.Z"
path-dep-requirement = "caret"
retry = { attempts = 5, initial-delay = "30s", multiplier = 2 }

[changelog]
tool = "changie"             # trellis generates .changie.yaml projects (§7)
```

Notably absent, because derived: package lists, dependency order, changie project
blocks, version-file maps, path-dep rewrite maps, tag→package mappings.

## 5. Command surface

### Introspection

```
trellis list [--json] [--since <ref>] [--with-dependents] [--releasable]
trellis graph [--format text|dot|mermaid|json]
trellis info <package> [--json]
```

- `list` prints members in topological order — this alone replaces `justfile:18`.
  `--releasable` filters out `ignore-release` matches, i.e. the set that
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
trellis changelog new [--package <pkg>]          # wraps `changie new --project`
trellis changelog check --base <sha> --head <sha> [--json]
trellis changelog sync                            # regenerate .changie.yaml projects
trellis version plan [--json]                     # dry-run: what would be bumped
trellis version apply                             # batch + merge + lockfile patch
```

- `changelog sync` generates the `projects:` section of `.changie.yaml` from the
  workspace model — label, key, changelog path, and version-replacement block per
  releasable member (`ignore-release` matches get no project block). The 88
  hand-written lines in lattice's `.changie.yaml` become output.
  `doctor` fails if the file is out of date (same model as generated lockfiles).
- `changelog check` reimplements the changie-check glue: map the base..head diff to
  packages, decide which need fragments, emit JSON (`has-entries`, `needs-entry`,
  `preview`, per-package detail) for the PR workflow's sticky comment.
- `version apply` is the release step: run `changie batch` + `changie merge` for
  every project with unreleased fragments, then **patch `manifest.toml` locked
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
trellis doctor [--fix]
```

Checks, each of which is an unenforced invariant in lattice today:

1. Member globs resolve to ≥1 directory; every member has a parseable `gleam.toml`.
2. Every path dep between members stays inside the workspace; graph is acyclic.
3. Generated files are current: `.changie.yaml` projects match members (`--fix`
   regenerates).
4. Each releasable member's `gleam.toml` version ≥ its latest CHANGELOG version,
   and each has a changelog file where expected.
5. `manifest.toml` locked versions of workspace-internal deps match those deps'
   actual `gleam.toml` versions (catches a missed lockfile patch).
6. Every `ignore-release` glob matches at least one member (catches typos), and
   no releasable member path-depends on an ignore-release member — a published
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
- run: trellis release pr               # or keep the changie-release action for PR mgmt

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

**Wrap changie, don't replace it (initially).** changie's fragment format, kinds,
and batching are good, and lattice has history in `.changes/`. Trellis generates
the mechanical part of `.changie.yaml` (project blocks) and shells out to `changie`
for `new`/`batch`/`merge`. If the two-tool dependency chafes later, a native
fragment engine can slot in behind the same `trellis changelog`/`version` commands;
the fragment file format would be kept changie-compatible.

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
2. **Phase 2 — changelog + version.** `changelog sync/check/new`, `version
   plan/apply`. Deletes `.changie.yaml` hand-maintenance, release.yml's inline
   bash, and the changie-check glue.
   **Status: implemented.** Notes: `sync` splices only the top-level
   `projects:` section of `.changie.yaml` textually (preserving hand-written
   config and comments elsewhere in the file, per the toml_edit philosophy)
   and creates a full starter config — with the `projectsVersionSeparator`
   derived from `tag-format` — when the file is missing; `doctor` gained the
   generated-file check and `--fix`. `version plan` shells out to
   `changie next auto --project` per pending project rather than reimplement
   changie's kind→bump rules. `version apply` verifies after batching that
   every `gleam.toml` actually received its new version (catching a stale
   replacements block), then patches lockfiles with `toml_edit` — zero Hex
   calls, formatting preserved. Fragments naming unknown or unreleasable
   projects fail `plan`/`apply`/`check` loudly. The changie binary is
   overridable via `TRELLIS_CHANGIE_BIN` (used by the e2e suite to run a fake
   changie).
3. **Phase 3 — release + publish.** `tag`, `publish`, `lockfile refresh`,
   `ci matrix/outputs`. Retires `read-gleam-workspace` and `gleam-publish` call
   sites. Optionally move to the tags-after-publish flow (§6).
4. **Extract.** Once stable in lattice, move to its own repo and publish binaries;
   lattice pins a version in `.tool-versions` like every other tool.

## 11. Open questions

1. **Name.** `trellis` may collide with Roots' WordPress tool; alternatives:
   `gws`, `latwork`, `gleamspace`.
2. **Scope of `version apply` vs. the `changie-release` action.** The action also
   manages the release *PR* (create-or-update, title, body). Keep PR management in
   the action and only replace the batch/patch internals, or absorb PR management
   into `trellis release pr` (requires a GitHub token and API surface in the tool)?
   Leaning: absorb, since `gh` CLI can do the PR mechanics and the tool already
   knows exactly what changed.
3. **Affected-only CI as default.** `--since` makes it possible; is the safety
   trade-off (missed implicit coupling between packages, e.g. via examples) worth
   it for this repo's build times, or should full fan-out stay the default with
   `--since` reserved for local use?
4. **Should trellis own `.tool-versions` awareness** (verify gleam/erlang versions
   match before running tasks), or is that mise/asdf's job?
