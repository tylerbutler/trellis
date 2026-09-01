# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.13.0 - 2026-09-01


### new

#### Breaking

- **`trellis new` is removed.** The command saw no real use: `gleam new` scaffolds the package, auto-discovery finds any `gleam.toml`, and `doctor --fix` seeds the changelog stub. Scaffolding can return post-1.0 if real scenarios appear.

## v0.12.0 - 2026-08-31


### pin

#### Added

- **`trellis pin` pins git dependency refs to commit SHAs, ratchet-style.** A symbolic ref (`ref = "lattice_core-v1"`) is readable but not reproducible; a bare SHA is auditable but loses the intent. `pin` keeps both: it resolves each symbolic ref with `git ls-remote`, rewrites `ref` to the commit SHA, and records the original in a `# trellis:pin <ref>` comment on the dependency's own line, leaving the rest of `gleam.toml` byte-for-byte intact. The locked commit in `manifest.toml` is patched surgically, without `gleam update`.

  `--update` re-resolves every recorded ref and rewrites the SHAs that moved — following a series becomes a reviewable diff instead of a silent re-resolution. `--unpin` restores the symbolic refs. `--check` fails (exit 1, for CI) when a pinned SHA is no longer reachable from its tracked ref — the signal that a tag or branch was force-moved past it; `doctor` reports the same drift as an advisory warning.

## v0.11.2 - 2026-08-12


### publish

#### Fixed

- **`publish` clears a package's resolved dependency tree after rewriting its path deps.** Validation resolves `build/packages` while those deps are still paths, and gleam cannot swap a local dependency for the Hex release of the same name in place: it drops the local entry, then fails reading the `gleam.toml` it just removed. Publishing any package with a path dep died that way, retries and all.

  Only `build/packages` is cleared, so compiled output under `build/dev` survives and the publish compile stays incremental. A package whose manifest needed no rewrite keeps its tree untouched.

## v0.11.1 - 2026-08-11


### changelog

#### Changed

- **`changelog new` names a fragment after the change it describes, not a counter.** The file is now `<package>-<first few words of the body>.toml` — `lat_core-reject-path-deps-that.toml` rather than `lat_core-1.toml` — so a directory of unreleased fragments reads as a list of pending changes, and two branches adding a change to the same package no longer both claim `-1`. Bodies with nothing nameable in them fall back to the package alone, and a genuine clash still takes the next free `-2`, `-3` suffix.

  Existing fragments are unaffected: names are never parsed, only written, so anything already in `.changes/unreleased/` keeps working.

## v0.11.0 - 2026-08-08


### tag

#### Breaking

- **`publish.package_tags` replaces `tag_mode`: list the tags each package gets.** An entry names how much of the version its tag keeps — `exact` (`lat_cli-v1.2.3`, immutable), `major` (`lat_cli-v1`) and `minor` (`lat_cli-v1.2`), both moving. `exact` substitutes into `exact_tag_format`, every series level into `series_tag_format`. During 0.x, `major` is the only way to get a plain `lat_cli-v0`.

  A 1.x package that moved `{name}-v1` under `tag_mode = "series"` now moves `{name}-v1.2`; list `major` to keep the one-part tag. The default is unchanged — `["exact"]`, immutable per-version tags only — so a workspace that configured no tag keys tags exactly as before.

  Each removed key fails the load naming its replacement, rather than being ignored and silently writing different tags:

  ```toml
  tag_format                  → exact_tag_format
  tag_mode = "exact"          → package_tags = ["exact"]
  tag_mode = "series"         → package_tags = ["minor"]
  tag_mode = "both"           → package_tags = ["exact", "minor"]
  tag_mode_overrides          → package_tags_overrides, keyed by member-path glob
  [publish.repository_series] → repository_tag_package, repository_tag_format,
                                repository_tags — all three required together
  ```

#### Fixed

- **A package's series tag no longer follows commits that did not release it.** `tag create` moved a series tag whenever it did not point at `HEAD`, so in a monorepo one package's release force-pushed every other package's series tags onto a commit that never touched them, and a docs-only commit moved all of them. A series tag now moves when its own package's manifest version changes, and a commit that releases nothing reports `every releasable package version is already tagged`.

#### Changed

- **A `series_tag_format` without `{name}` is deprecated, and removed at 1.0.** Dropping `{name}` gave the whole repository one shared series tag, but the template then matches every member, so `trellis ci tag-package` cannot resolve such a tag to a package. `doctor` now warns on the shape whether or not a second package has made it ambiguous, as `workspace_config` rather than `tag_collision`. The tag still moves and releases still work.

  To migrate, restore `{name}` and declare the repository tag instead: `repository_tag_package`, `repository_tag_format`, `repository_tags`. It is repository metadata, so unlike the shared package tag it never resolves through `ci tag-package` or `publish --tag` — keep a `{name}`-ful series tag for anything that routes CI on a tag.

## v0.10.3 - 2026-08-07


### trellis

#### Fixed

- **Git dependencies with a `path` subdirectory (Gleam 1.18+) are treated as external.** Gleam 1.18.0 added an optional `path` key on git dependencies that selects a subdirectory of the remote repository. Trellis classified any dependency carrying a `path` key as a workspace path dependency, so a git dependency into an external monorepo made every command fail with `workspace is invalid` — or, when the dependency shared a member's name, invented a phantom graph edge that could report a false dependency cycle. The `git` key now wins: such dependencies stay out of the workspace graph, and `trellis publish` leaves them untouched instead of rewriting them to Hex requirements.

## v0.10.2 - 2026-08-06


### changelog

#### Fixed

- **`trellis changelog check` now rows every package the branch documented, not just the ones whose files it changed.** A fragment written for a package the diff never touched — a break propagating to a dependent, documented where it lands — still bumps that package, and the release preview said so while the table above it did not. Such a package now gets its own row, is reported as `changed: false` in the `--json` payload, and is never asked for a changelog entry of its own.

### doctor

#### Fixed

- **`trellis doctor` no longer flags a working `@members` exclusion glob as a typo.** The check compared the glob against the already-filtered member list, so any glob that successfully excluded a directory could never appear to match — every real `@members` exclusion was reported as `matches no member (typo?)`. The typo check now runs against the pre-filter candidate set, before `@members` removes anything, so a working exclusion is never flagged while a genuine typo still is.

## v0.10.1 - 2026-08-04


### version

#### Fixed

- **`trellis version` no longer ripples dependency bumps outside the published requirement.** A dependent now bumps only when its existing `path_dep_requirement` can select the dependency's new version, so a default `minor` requirement does not ripple across a major-version boundary.

## v0.10.0 - 2026-08-03


### trellis

#### Added

- **Command output now uses color.** Errors, warnings, notes, and completed actions (`ok:`, `tagged`, `published`, `bumped`, and similar verbs) are colored consistently across every command, and package names carry the same per-package color already used by `run`'s live output through `list`, `graph`, `version`, `tag`, `publish`, `changelog check`, `info`, and the run summary table. Structural and metadata text — tree glyphs, `$ command` echoes, versions, field labels, `-v` traces, and dry-run `would …` lines — is dimmed so it reads as background rather than action. Color follows the existing `--color`/`NO_COLOR`/TTY resolution, so piped output and `--color never` are unchanged.

### changelog

#### Breaking

- **`changelog check` counts only the fragments the branch wrote.** A fragment is the branch's own when its contents differ from the merge base of `base...head` — added outright, or edited from what the base branch already had. Unreleased fragments the branch left untouched document the PRs that added them, and no longer satisfy the check for a later PR touching the same package: one entry can no longer excuse every change until the next release. Comparing contents rather than reading the diff means an uncommitted fragment counts too, so a local run answers the way CI will. Invalid fragments are deliberately not scoped — a fragment that does not parse blocks the next release whoever committed it. `packages[].fragments`, `packages[].has_entry`, and `has_entries` narrow with the count, advancing the payload from `trellis.changelog_check/1` to `trellis.changelog_check/2`. PRs that relied on a base-branch fragment now report a missing entry; set `changelog.strictness = "warn"` to land the reporting without failing builds.

#### Added

- **`changelog check`'s PR comment previews the release itself.** The sticky-comment `preview` now adds a version column (`1.2.0 → 1.3.0`) to the package table and a collapsible "Release preview" section rendering the changelog sections `version apply` would write — fragment bodies, generated ripple entries, and rippled-only dependents included. Like the rest of the check it is scoped to the branch, so the comment shows what merging this branch releases and `version` is the bump this PR causes rather than what the base branch's backlog adds up to. When a fragment does not parse, the versions and preview are omitted and the problem is reported as before.

## v0.9.0 - 2026-08-01


### list

#### Changed

- **`trellis list`'s default text output is now two columns, name and resolved release lifecycle, instead of names alone.** `--json` gains an additive `lifecycle` field (`workspace`, `git_only`, or `hex`) on every package alongside the retained `releasable` boolean. Human-readable output was never a stable contract — script against `--json` if you're parsing.

### graph

#### Added

- **`trellis graph --format json` nodes carry the resolved release lifecycle.** Each node's additive `lifecycle` field (`workspace`, `git_only`, or `hex`) sits alongside the existing `releasable` boolean, matching `list`/`info`.

### info

#### Added

- **`trellis info` reports a package's resolved release lifecycle.** Text mode gains a `lifecycle:` line alongside `releasable:`; `--json` gains the same additive `lifecycle` field `list --json` carries.

### changelog

#### Added

- **`changelog.strictness` decides what a missing changelog entry costs.** `error` is the default and fails `trellis changelog check` exactly as before; `warn` reports the missing entry and exits 0; `off` skips the verdict while still listing each changed package's fragment count. `--strictness error|warn|off` overrides the configured value for one run, so a workflow can gate harder than the workspace default without editing `gleam.toml`.

  Strictness covers missing entries only. A fragment that doesn't parse, or that names an unknown package or kind, still fails the check at every setting.

- **`trellis changelog check --format github` emits `key=value` lines for `$GITHUB_OUTPUT`.** A workflow reads `ok`, `has_entries`, `needs_entry`, `needs_entry_packages`, `invalid_fragments`, and a ready-to-post markdown `preview`, so a PR comment takes a redirect instead of a `jq` pipeline. See [CI recipes](https://trellis.tylerbutler.com/docs/ci/) for the sticky-comment workflow.

  The `trellis.changelog_check/1` payload gains two fields alongside them: `ok`, the verdict after strictness is applied, which matches the exit code, and the `strictness` that decided it. `--format json` is the new spelling of `--json`, which still works as a deprecated alias; passing both is a usage error.

- **`trellis changelog` now follows each package's resolved release lifecycle.** Packages in `git_only` and `hex` accept fragments and participate in `changelog check`; `workspace` packages do neither. Errors for explicitly selecting a `workspace` package name its lifecycle instead of describing every exclusion as an `exclude.@release` match.

### version

#### Added

- **`trellis version` now plans and applies bumps for `git_only` and `hex` packages while leaving `workspace` packages unchanged.** Dependency ripple still stops at a non-versioned `workspace` boundary, and `--bump` or `--set` reports the resolved lifecycle when a named package cannot participate.

### release

#### Added

- **`trellis release bootstrap` tags existing package versions.** Use it when adopting trellis in a repository whose versions and changelogs are already correct but whose tags are missing. It is an alias for `tag create`: it never runs `version apply` or requires unreleased fragments. `--dry-run` (also new on `tag create`) previews every tag, push, and GitHub Release action; `--push` updates `origin`, and `--github-release` implies `--push`.

  When pushing, every planned tag is checked for a local/remote conflict before any of them are mutated, so one package's immutable tag disagreeing with origin fails the whole run instead of leaving another package half-tagged.

#### Changed

- **`release pr` drives the GitHub API directly; the gh CLI is no longer required.** The PR is created or updated through api.github.com with a token from `GITHUB_TOKEN` (ambient in GitHub Actions, so CI needs no setup), `GH_TOKEN`, or a logged-in gh CLI as the fallback (`gh auth token`). The repository is read from the `origin` remote; set `TRELLIS_GITHUB_REPO` (`owner/repo`) when the remote URL is not a github.com one.

### tag

#### Added

- **Repository series tags track one anchor package across a monorepo.** Configure `[tools.trellis.publish.repository_series]` with `package` and `format` to expose a mutable tag for Gleam git path dependencies. It moves only when the anchor manifest version changes, preserves tags from earlier series, creates no GitHub Release, and stays out of package tag resolution.

- **`trellis tag` creates git tags and GitHub Releases for both `git_only` and `hex` packages.** `workspace` packages produce no exact or series tags, while each participating package continues to follow its resolved `tag_mode`.

#### Changed

- **`tag create --github-release` creates GitHub Releases through the API instead of the gh CLI.** It needs a token from `GITHUB_TOKEN`, `GH_TOKEN`, or a logged-in gh CLI (`gh auth token`), resolved once before any tag is created so a missing token fails the run up front. Existence checks and release bodies are unchanged: one release per immutable tag, with the matching CHANGELOG section as the body.

### publish

#### Added

- **`[tools.trellis.publish.lifecycle]` gives each package a release lifecycle: `workspace`, `git_only`, or `hex` (the default).** `workspace` packages never get a changelog entry, a version bump, a git tag, or a Hex publish; `git_only` packages get all of those except the Hex publish; `hex` packages get the full pipeline. Configure a `default` and per-package `packages = { "glob/**" = "state" }` overrides, matched against member paths — a member matched by globs resolving to different states is a doctor error, but globs agreeing on the same state are fine.

  The legacy `exclude.@release` key keeps working: a match there still resolves to `workspace`, unless an explicit `publish.lifecycle.packages` rule for that member says otherwise, which lets a package graduate from `workspace` to `git_only` to `hex` without moving directories or rewriting the exclusion. `--releasable` keeps meaning `git_only` **or** `hex`; `publish` alone narrows further, since only `hex` packages ever reach Hex — naming a `workspace` or `git_only` package with `--package` or `--tag`, or leaving one out of the path-dependency rewrite map, now fails with a message naming the package's actual lifecycle instead of a generic `@release` exclusion.

### doctor

#### Added

- **`doctor` validates dependency availability against each package's release lifecycle, not just a binary releasable/excluded split.** The `release_boundary` check now enforces that a runtime (`[dependencies]`, not `[dev-dependencies]`) path dependency is at least as capable as its dependent: a `hex` package may depend only on `hex`; `git_only` may depend on `git_only` or `hex`; `workspace` may depend on anything. The message names both packages' lifecycles and why the dependency would be unavailable.

  `--format json` gains an additive `package_lifecycles` array of `{name, lifecycle}`, one per member in workspace order, alongside the retained numeric `packages` count; text mode prints the same data as compact counts, e.g. `ok: 4 package(s) (1 workspace, 0 git_only, 3 hex), 0 warning(s)`. `publish.lifecycle.packages` globs are checked for typos the same way `exclude` and `tag_mode_overrides` globs already are.

### ci

#### Added

- **`trellis ci` treats `git_only` and `hex` packages as releasable and excludes `workspace` packages from release outputs.** `ci matrix --releasable`, `releasable`, `version_files`, exact tags, series tags, and tag-to-package resolution now follow the resolved lifecycle without changing their existing output shapes.

## v0.8.0 - 2026-07-30


### trellis

#### Added

- **Every JSON document now names its payload and major version in a `schema` field** — `"schema": "trellis.list/1"` — so a workflow can assert on the shape it was built against.

  The new [JSON output contract](https://trellis.tylerbutler.com/docs/json-output/) page sets the rules: new fields can turn up at any time, but renaming, removing, or retyping one bumps the major. Every existing payload gains the `schema` key and nothing else, bar the three bare arrays that had to become objects to hold it — `list`, `version plan`, and `tag plan`, each covered in its own entry. `ci matrix` and `ci outputs` take their shape from GitHub Actions and carry no `schema`.

- **Four global flags now work before or after the subcommand.** `--color auto|always|never` overrides terminal detection both ways, so `--color always` survives a pipe and `--color never` survives a terminal; `auto` keeps honoring `NO_COLOR` and `CLICOLOR=0`. Spinners still follow the terminal, so forcing color into a pipe won't draw any.

  `-q/--quiet` drops the per-package stream and the summary table without touching exit codes. `-v/--verbose` traces every `gleam`, `git`, and `gh` command to stderr, prefixed `+ ` and naming the directory it ran in. `--no-update-check` skips the release check for one invocation.

#### Fixed

- **`-q/--quiet` now silences the normal chatter of every command,** not just `run` and `exec` — progress lines, summaries, and confirmations. It leaves alone the `--json`, `--format json|github`, and `--format dot|mermaid` payloads, the `man`/`completions`/`markdown-help` output, fatal errors, and exit codes.

#### Changed

- **Trellis now exits `3` when it couldn't run at all,** as distinct from the `1` it returns when a command ran fine and found problems. An unparseable config, a missing `gleam` or `gh`, a non-git directory, and Hex unreachable after retries all exit `3`; they used to exit `1`, indistinguishable from `doctor` findings.

  The whole contract: `0` success, `1` ran fine and found problems, `2` usage error, `3` couldn't run. A failed task exits `1` and doesn't pass along the child's exit code. The new [Compatibility](https://trellis.tylerbutler.com/docs/compatibility/) page writes all this down next to what semver covers at 1.0, and declares an MSRV of 1.85.

- **Every identifier trellis controls is now snake_case,** matching Gleam's own convention. `[tools.trellis]` keys lose their hyphens — `tag-format` becomes `tag_format`, `needs-deps` becomes `needs_deps`, and so on for the eleven keys that had one — as do the `--json` payloads' object keys, enum values, and `schema` names.

  Every spelling released through v0.7.0 still parses, so existing workspaces keep working; `doctor` warns on each one and names its replacement, and the aliases go away at 1.0. The free-form tables (`exclude`, `tasks`, `publish.tag_mode_overrides`) and Gleam's own manifest keys, `dev-dependencies` among them, are untouched.

- **`package` replaces `project` as the word for a Gleam package everywhere trellis names one.** A fragment's `project` key is now `package`, `trellis ci outputs` emits `packages` alongside `projects`, and the `changelog.dependency_body` template context gains `package`. Help text and `doctor`'s output follow — it now sums up with "ok: 4 package(s)".

  Nothing breaks: existing fragments parse unchanged, `projects` carries the same value as `packages`, and `{{ project }}` still renders. `changelog new` writes the new spelling, and the aliases go away at 1.0. "Member" survives where membership is the point — the `members` key and the `@members` exclusion.

### list

#### Breaking

- **`trellis list --json` returns an object, not a bare array.** The array moves under a `packages` key so the document can carry the new `schema` field: `{"schema": "trellis.list/1", "packages": [...]}`.

  `trellis list --json | jq ".[].name"` becomes `jq ".packages[].name"`.

### run

#### Added

- **`trellis run` accepts `--json`.** The new `trellis.run/1` payload names the task and the `--target` as you gave it, then carries one record per package — path, status, wall-clock duration, and, on failure, the exit code and the command that produced it. Until now the only machine-readable signal was the overall exit code.

  A task that sets `needs_deps` runs several commands, so `exit_code` and `command` describe the one that failed rather than the whole job. A package that never ran — scheduling stopped at an earlier failure — reports `skipped`, which still fails the command. Under `--json` the per-package stream moves to stderr and the spinners and summary table go away, leaving stdout to the payload alone.

### exec

#### Added

- **`trellis exec` accepts `--json`.** The new `trellis.exec/1` payload echoes the command back as argv, so nothing downstream has to re-split a quoted string, and carries one record per package — path, status, wall-clock duration, and, on failure, the exit code. Until now the only machine-readable signal was the overall exit code.

  A package that never ran — scheduling stopped at an earlier failure — reports `skipped`, which still fails the command. Under `--json` the per-package stream moves to stderr and the spinners and summary table go away, leaving stdout to the payload alone.

### changelog

#### Added

- **Changelog entries can group under a second axis above their kind.** Set `categories` under `[tools.trellis.changelog]` — a vocabulary like `kinds`, but with no bump attached — then write a fragment into one with `trellis changelog new --category <name>`.

  Leave `categories` unset and nothing changes. Even with it set, a fragment needn't name one; those that don't trail the rest under `uncategorized_label`, which is `Other` unless you say otherwise. Naming a category that isn't in the list invalidates the fragment, exactly as an unknown kind does. While the axis is on, kind headings drop from `###` to `####` unless `kind_format` says otherwise — and an older trellis will choke on a fragment carrying a `category`. See [categories](https://trellis.tylerbutler.com/docs/configuration/#categories).

### version

#### Breaking

- **`trellis version plan --json` returns an object, not a bare array.** The array moves under a `bumped` key so the document can carry the new `schema` field — matching `version apply`, which already used that name.

  `trellis version plan --json | jq ".[].name"` becomes `jq ".bumped[].name"`.

#### Added

- **When a package bumps, its workspace dependents now bump too** — transitively, one patch each, with a generated `Dependencies` entry naming what moved. Trellis derives a path dependency's Hex requirement at publish time, so a dependent left behind can ship resolving to two different dependency sets.

  Dependents need no fragment of their own, and one that already has a fragment keeps its larger bump. A ripple stops at any package excluded from `@release`. Two new keys under `[tools.trellis.changelog]` shape the generated entry: `dependency_kind` and `dependency_body`.

- **`version plan` and `version apply` accept `--bump` and `--set`,** so the version no longer has to be whatever the pending fragments derive. `--bump major` sets the level for the whole plan, `--bump lat_core=major` for one package; `--set lat_core=1.0.0` pins an exact version, for the jump to 1.0 that the pre-1.0 rule would otherwise call a minor.

  `--set` wins over a per-package `--bump`, which wins over a workspace-wide one, which wins over the derived level. Both commands take the same flags, so you see an override in the dry run first, and trellis rejects conflicting or backwards overrides before writing anything.

- **`version apply --pre rc` cuts a release candidate; `--pre none` promotes it.** Repeat `--pre rc` and the counter advances within the same base version — `1.0.0-rc.1`, then `1.0.0-rc.2`.

  Fragments survive a prerelease: the candidate renders its changelog section but leaves them in `.changes/unreleased/`, so an entry shows up twice in `CHANGELOG.md` — once under the RC, once under the final release. `version --json` reports `fragments_retained` so a workflow can tell the two apart.

  The label covers the whole plan, rippled dependents included. Once a package sits at a prerelease, a plain `version apply` errors out and names both ways forward. See [prereleases](https://trellis.tylerbutler.com/docs/changelog/#prereleases).

#### Fixed

- **`version apply` no longer deletes a package's pre-existing changelog.** Trellis regenerates `CHANGELOG.md` from the sections under `.changes/<package>/`, so a package that already had a changelog when it adopted trellis lost the lot on its first release.

  That history now survives verbatim as a single section, filed under the newest version its headings mention. `trellis doctor` flags the pending capture and `doctor --fix` does it up front, so the restructuring lands in a diff of its own.

### init

#### Added

- **`trellis init` bootstraps a workspace.** It writes the `[tools.trellis]` table into the repository root's `gleam.toml`, creating a config-only manifest if the root isn't itself a package and keeping your comments and formatting if it is.

  The table it writes is nearly bare, and that's the point: its presence alone marks the workspace root, members stay auto-discovered, and the comments point at what you could set. `init` prints the members it found, bails out on a repository that is already a trellis workspace, and finishes by running `doctor`.

### tag

#### Breaking

- **`trellis tag plan --json` returns an object, not a bare array.** The array moves under a `tags` key so the document can carry the new `schema` field: `{"schema": "trellis.tag_plan/1", "tags": [...]}`.

  `trellis tag plan --json | jq ".[].tag"` becomes `jq ".tags[].tag"`.

### doctor

#### Breaking

- **`trellis.doctor/1` renames two identifiers to match the package/member rule:** the workspace-size field `members` becomes `packages`, and the `member_manifest` check value becomes `package_manifest`. `member_glob`, `configless`, and `auto_members` keep their names — they report on the `members` globs and on how trellis worked out membership, not on the packages themselves.

  A consumer doing `jq ".members"` switches to `jq ".packages"`, and a workflow branching on `check == "member_manifest"` has to match `"package_manifest"`.

#### Added

- **`trellis doctor --format json|github` reports findings as data instead of prose.** Each finding is a typed record — `{check, severity, message, file, package, fixable}` — so CI can group them by package or narrow to the ones `--fix` would clear. `--format github` emits GitHub Actions workflow commands, which annotate the offending file right in a PR's Files tab.

  Branch on `check`: it's a documented enum on the [JSON output contract](https://trellis.tylerbutler.com/docs/json-output/) page. Don't branch on `message` — that prose can change. Text output stays as it was.

- **Trellis now reports keys under `[tools.trellis]` it doesn't recognize instead of ignoring them,** so a typo no longer leaves the workspace quietly sitting on a default. `doctor` calls each one a warning rather than an error — an unrecognized key may simply belong to a newer trellis. The free-form tables (`exclude`, `tasks`, and `publish.tag_mode_overrides`) take keys you choose, and never get reported.

- **`trellis doctor` now checks that packages agree on the external dependencies they share** — `lat_core` wanting `gleam_stdlib >= 0.44.0` while `lat_cli` wants `>= 0.60.0`. It compares the requirement strings as written instead of parsing them as ranges, so `>= 1.0` and `>=1.0` count as a disagreement; path dependencies are out of scope.

  Disagreeing on purpose is common enough that this only warns and won't fail CI. Set `shared_dependencies` under `[tools.trellis.doctor]` to `warn`, `error`, or `off`. Nothing here is `--fix`-able.

### ci

#### Breaking

- **`trellis ci outputs` renames two of its GitHub Actions outputs to snake_case:** `version-files` becomes `version_files` and `series-tags` becomes `series_tags`. `releasable` and `tags` are unchanged; `packages` supersedes `projects`, which sticks around as a deprecated alias. The values are identical — only the names moved, to follow the same snake_case rule as the config keys and the `--json` payloads.

  A workflow reading `steps.<id>.outputs.version-files` or `.series-tags` has to switch to `.version_files` and `.series_tags`.

### completions

#### Added

- **`trellis completions <shell>` prints the snippet that turns on tab-completion** for bash, zsh, fish, PowerShell, and elvish. The snippet asks trellis for candidates on each tab-press, so you get the real package names, task names, and changelog kinds from the workspace you're standing in. Evaluate it on shell startup rather than saving it to a completions directory — a saved copy goes stale on the next upgrade.

  Release archives also ship man pages under `man/`: `trellis.1`, plus a page per subcommand.

## v0.7.0 - 2026-07-25


### Added

- Packages can now move a series tag alongside (or instead of) their immutable per-version tag. `tag-mode` picks the lifecycle for the workspace and `tag-mode-overrides` picks it per package; the series is derived from the version — `0.Y` while the major is 0, `X` after — so releasing 0.0.1, 0.0.2, 0.0.3 keeps moving one `pkg-v0.0` tag.

## v0.6.0 - 2026-07-22


### Added

- Interactive task runs now show live per-package progress rows with stable, hash-derived package colors while logs scroll above them; pipes and CI retain plain prefixed output.

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

