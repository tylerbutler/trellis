# AGENTS.md

Guidance for agents working in this repository. See [docs/DESIGN.md](docs/DESIGN.md)
for what trellis is and why.

## Naming: snake_case for everything we control

**Every identifier trellis defines is snake_case.** That covers:

- `[tools.trellis]` config keys — `tag_format`, `needs_deps`, `series_tag_format`
- `--json` object keys — `exit_code`, `duration_ms`, `auto_members`
- `--json` enum values — `member_glob`, `seed_changelog`, `up_to_date`
- `schema` payload names — `trellis.changelog_check/1`, `trellis.version_plan/1`
- `trellis ci outputs` GitHub Actions output names — `version_files`, `series_tags`

The config table lives inside `gleam.toml`, which spells its own settings
`internal_modules`, and snake_case is the Gleam convention generally. One rule
for everything we emit means nobody has to remember which surface uses which.

Two exclusions, both formats trellis does not own:

- **Gleam manifest keys.** `dev-dependencies` stays as Gleam spells it — see
  `src/gleam.rs`, `src/rewrite.rs`, `src/commands/new.rs`.
- **CLI long flags.** `--no-update-check`, not `--no_update_check`. Kebab is the
  universal convention there and clap's default.

Free-form table keys (`exclude`, `tasks`, `publish.tag_mode_overrides`) are the
user's to name; a hyphen in one is not a violation, and `doctor` says nothing
about them.

Config keys released through v0.7.0 keep their kebab-case spelling as a
`#[serde(alias)]`, reported by `doctor` as a deprecation. These come out at 1.0;
new keys never get one.

## Changelog fragments

Every user-visible change needs one: a YAML file in `.changes/unreleased/` named
`<Kind>-<YYYYMMDD>-<slug>.yaml`, with `component`, `kind`, `body`, and `time`. The
audience is a stranger reading the release notes, not the reviewer of your PR.

Write the body as a `|-` block scalar with a **bolded lead-in sentence**, then
paragraphs:

```yaml
component: completions
kind: Added
body: |-
  **`trellis completions` generates tab-completion for five shells.** Candidates are computed by the binary as you type, so completion offers real package, task, and changelog-kind names from the surrounding workspace.

    Release archives also ship man pages under `man/`.
time: 2026-07-25T18:01:19.474540769-07:00
```

**One line per paragraph. Never hard-wrap a body**, however long the line gets —
the shipped entries run to 500+ characters. cargo-dist copies the version's
CHANGELOG.md section verbatim into the GitHub release body, and GitHub renders
release notes the way it renders comments: every single newline becomes a `<br>`,
unlike a `.md` file in the repo, where it collapses to a space. A body wrapped at
80 columns reads fine in CHANGELOG.md and arrives in the release notes as a
ragged column of short lines. Wrap only *between* paragraphs, with a blank line.

`component` is the subcommand the change belongs to, and renders as a `###`
heading above the `####` kind headings, so a reader can find what changed in
`doctor` without reading the whole release. The list and its render order live in
`.changie.yaml`; `changie new` prompts from it.

**`trellis` is the catch-all,** for a change that genuinely isn't scoped to one
subcommand: the exit-code contract, the global flags, a rule that holds for every
`--json` payload. It sorts first, ahead of the per-command sections.

Reach for it last. A change landing on two or three subcommands wants two or
three fragments, one per command, each saying what that command's payload or flag
actually does — not one `trellis` entry. Splitting usually sharpens the copy,
because the caveats differ per command: `run --json` documents `exit_code` naming
the failed command of a `needs_deps` task, which `exec --json` has no equivalent
of. Used as a bucket for "more than one command", `trellis` stops meaning
anything.

Two ways to get this wrong, neither of which fails the batch:

- **Omitting `component`** renders the entry under an empty `### ` heading.
- **Misspelling one** invents a section rather than erroring — changie does not
  validate the value against the configured list.

changie renders each body as `- {{.Body}}`, one bullet, so every paragraph after
the first needs a 2-space indent to stay inside it. YAML takes the block's
indentation from its **first** line, so write the first paragraph at 2 spaces and
every later one at **4** — that lands as the 2-space markdown indent. A line at
6+ spaces becomes a code block; a line at 2 becomes a sibling bullet.

- The lead-in is one sentence, ≤ ~15 words, naming the command, flag, or key and
  what it now does. For a small change it is the whole entry.
- Keep what a reader acts on: the change, its user-visible consequence, the
  migration, flags and keys by name, and edge cases they can actually hit.
- Cut the design rationale — why this shape, what was rejected, how it is
  implemented. That belongs in [docs/DESIGN.md](docs/DESIGN.md) or the website
  docs. At most one "previously…" clause, where the fix makes no sense without it.
- End a `Breaking` entry with the concrete migration: old spelling → new, or the
  `jq` change.
- Use current spellings in prose: snake_case keys, "package" not "project".
- 40–90 words for most entries; two or three short paragraphs for a big one.

## Conventions

- **Commits** use [conventional commits](https://www.conventionalcommits.org/):
  `type(scope): description`. Include a body; skip attribution trailers.
- **Tests** come first for bug fixes and new behavior.

## Commands

```sh
just test     # cargo test
just lint     # cargo clippy -- -D warnings (CI gates on this)
just format   # cargo fmt
just docs     # regenerate website/src/content/docs/docs/reference.md and assets/man
just ci       # format, lint, test, build
```

Snapshot tests use [insta](https://insta.rs/): `cargo insta review` after any
change to `src/json.rs`, and read each diff — a snapshot accepted without
looking is a wire-format break accepted without looking.

`website/src/content/docs/docs/reference.md` and `assets/man/` are generated.
Edit the clap definitions, then run `just docs`.
