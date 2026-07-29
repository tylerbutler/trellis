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
user's to name; a hyphen in one is not a violation, and `doctor` deliberately
says nothing about them.

Config keys released through v0.7.0 keep their kebab-case spelling as a
`#[serde(alias)]`, reported by `doctor` as a deprecation. These come out at 1.0;
new keys never get one.

## Conventions

- **Commits** use [conventional commits](https://www.conventionalcommits.org/):
  `type(scope): description`. Include a body; skip attribution trailers.
- **Changelog fragments** are required for user-visible changes — a YAML file in
  `.changes/unreleased/` with `kind`, `body`, and `time`. Prose, not a summary:
  say what changed and why it was worth changing.
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
