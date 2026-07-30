# Verifying claims against the source

Techniques for Step 2. The goal is to reach every factual assertion in the copy and either
confirm it against the thing that defines it, or record that you couldn't.

The rule underneath all of this: **confirm against the definition, never against another doc
page.** Doc pages copy each other. A wrong key that appears on four pages looks like four
confirmations and is one mistake propagated.

## Find the definition of an identifier

Identifiers in user-facing copy — config keys, flags, env vars, JSON fields, output names,
command names — each have exactly one place they're defined. Locate that place, then check
spelling, requiredness, and default in one pass.

| Identifier | Where it's defined | What to search for |
|---|---|---|
| Config keys | Settings struct or schema | `serde` structs and `#[serde(rename/alias/default)]`; zod/pydantic models; JSON Schema files |
| CLI flags and help text | The CLI definition | clap `#[arg(...)]` and the doc comments above each variant; commander/argparse/oclif definitions |
| JSON payload fields | The serializer | The `Serialize` impl or response struct; snapshot test fixtures |
| Output names (CI, hooks) | Whatever emits them | The literal strings in the emitting function |
| Error message text | The error type or the raise site | `thiserror`/`anyhow` messages, exception constructors |
| Commands and subcommands | The command enum or registry | The dispatch table |

Two things worth checking every time you're at the definition:

- **Aliases and deprecations.** An old spelling kept as an alias means both spellings work but
  only one is current. Copy should use the current one; a migration table can name the old.
- **Defaults.** A field with a default is optional even if the docs call it required. This is
  the single most common false claim in configuration docs, because the default gets added
  after the docs are written.

## Prove a config sample actually works

Reading a sample and thinking it looks right is how the silent-failure class survives. Feed it
to the real parser.

Build a scratch fixture and run the project's own validator against it:

```sh
mkdir -p /tmp/copy-check && cd /tmp/copy-check
# reproduce the minimum the tool needs to consider this a real project
# paste the doc's sample into the config file, verbatim
<the-tool> <its-validate-or-check-command>
```

Verbatim matters. Fixing the sample's indentation or quoting as you paste it means you tested
your corrected version and shipped the broken one.

### The silent-failure traps to look for specifically

These parse successfully and do nothing, so no error ever reaches the reader:

- **Sigil or namespace syntax where the bare word is also valid.** If reserved keys are spelled
  `@release` or `$schema` or `_internal`, the bare form usually parses too — as a user-defined
  entry with no special meaning. A doc sample using `release` where the real key is `@release`
  defines a task named "release" and excludes nothing. Check the parser for how it distinguishes
  reserved from user-supplied names, then confirm every sample uses the right form.
- **Free-form tables.** Sections whose keys are the user's to name (task maps, overrides,
  metadata) accept anything, so a typo there is undetectable by validation. These need the
  strictest reading.
- **Globs matching nothing.** A pattern for a path layout the sample doesn't have.
- **A section under the wrong parent table.** Valid TOML/YAML, ignored entirely.
- **Unknown-key tolerance.** If the parser ignores unrecognized keys rather than rejecting
  them, every misspelled key in every sample is invisible. Check which behavior applies before
  trusting that "it parsed" means anything.

## Regenerate output samples, never hand-patch them

Terminal transcripts, `--help` text, and JSON payloads in docs go stale on every release.

Run the command and paste the actual result:

```sh
<the-tool> <subcommand> 2>&1 | tee /tmp/actual.txt
diff <(sed -n '/```/,/```/p' the-doc.md) /tmp/actual.txt   # or just read both
```

Hand-editing a transcript to add the one new line you noticed leaves every other drifted line
in place, and now the sample looks freshly checked. If the real output can't be reproduced —
it needs a fixture repo, a network call, credentials — say so in your report rather than
guessing at it.

For generated artifacts (CLI reference pages, man pages), don't touch the artifact at all: edit
the help text in the code, run the project's regeneration recipe, and let the diff appear.

## Check requirement and default claims

Every "required", "optional", "must", "defaults to", and "if omitted" in the copy is a claim a
reader will act on. For each one, find the field's definition and confirm:

- Is it genuinely required, or does it have a default?
- If the whole config file is optional, does the copy say so? "Only `members` is required" is
  false in a tool where every key is optional and the table's mere presence is the signal.
- Does "defaults to X" match the actual default value, including after any auto-discovery?

## Verify cross-page example consistency

Pick the canonical example — the one in the tutorial or the landing page — and grep the doc set
for the names it uses:

```sh
rg -n 'example_package_name|1\.2\.0' --glob '!**/generated/**' docs/ website/ README.md
```

Every appearance should agree on package names, versions, and directory layout. A reader
following a sequence of pages whose examples disagree concludes they broke something during
step two. Prefer one example family with names that visibly belong together over per-page
inventions like `package_a`, `my-lib`, `foo`.

## Verify links and version references

- Internal links resolve to a page that exists (route changes silently orphan them).
- Anchors resolve to a heading that still has that text.
- External links return 200.
- "As of version X" claims still describe the *current* release, not a past one. Version
  archaeology usually just wants deleting — see the prose moves in SKILL.md.

## Record what you couldn't verify

Anything you couldn't check — output that needs credentials, behavior that needs a live service,
a claim about a platform you can't run — goes in the report's findings section, naming the file
and what would be needed to settle it. An unverifiable claim left in place with a note is honest.
An unverifiable claim rewritten into confident prose is a new defect you introduced.
