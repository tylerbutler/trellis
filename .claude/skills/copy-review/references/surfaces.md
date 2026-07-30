# Surfaces

Per-surface rules and the format traps specific to each. Read the section for whatever you're
editing; skip the rest.

- [Changelog entries and release notes](#changelog-entries-and-release-notes)
- [Landing page and marketing copy](#landing-page-and-marketing-copy)
- [Guides and reference docs](#guides-and-reference-docs)
- [README](#readme)
- [Code-level copy: doc comments, CLI help, error messages](#code-level-copy)

## Changelog entries and release notes

### Establish the format before writing a word

Changelog tooling varies more than any other surface, and projects migrate between tools while
old-format files sit on disk. Determine, from the repo rather than from assumption:

- **Which tool renders these** — a fragment-based tool (changie, towncrier, scriv, changesets), a
  hand-maintained Keep a Changelog file, or a native implementation the project wrote itself.
  Its config file names the directory, the file format, and the categories.
- **The file format actually on disk.** List the fragment directory. A project mid-migration can
  have a config describing YAML fragments and source code that reads TOML, or vice versa. The
  files present tell you what the next one should look like; the project's own contributor doc
  breaks the tie.
- **The render template.** This is what makes the indentation rule below bite. Find how a body
  becomes a line of markdown.
- **The category list**, and what each one means for version bumps. Miscategorizing an entry
  can change the computed release version.

### The block-scalar indentation trap

When the tool renders each entry as a single list item (a template like `- {{.Body}}`), every
line after the first needs the markdown continuation indent — normally 2 spaces — to stay inside
that bullet. In YAML, a block scalar's indentation is measured from its **first** line. So write
the first line at 2 spaces and every later line at **4**, which lands as the 2-space markdown
indent:

```yaml
kind: Added
body: |-
  **The lead-in sentence sits at two spaces.** The rest of this paragraph, and
    every line of every later paragraph, sits at four.

    A blank line separates paragraphs; the indent resumes at four.
time: 2026-07-25T18:01:19.474540769-07:00
```

What breaks when you get it wrong: a line at 6+ spaces becomes a fenced code block in the
rendered output; a line back at 2 becomes a sibling bullet, splitting one entry into several.
Neither fails loudly — both look fine in the YAML and wrong in the release notes. Preview the
render (`changie batch <level> --dry-run` or the tool's equivalent) rather than trusting the
file.

### Prose rules

The audience is a stranger reading release notes, deciding whether this release affects them.
Not the reviewer of the PR that produced the change.

- **Open with a bolded lead-in sentence**, one sentence, roughly 15 words or fewer, naming the
  command, flag, or key and what it now does. For a small change that sentence is the whole
  entry. This is what makes a long release section scannable — twenty entries that each open
  with a dense paragraph is twenty walls of text with no entry point.
- **Keep what the reader acts on:** the change, its user-visible consequence, the migration,
  flags and keys by name, and edge cases they can actually hit.
- **Cut the design rationale** — why this shape, what was rejected, how it's implemented, what it
  cost to build. It belongs in the design doc or the docs site. Allow at most one "previously…"
  clause, and only where the fix makes no sense without it. Watch for a contributor-facing rule
  in the project's own docs that asks for the rationale — "say why it was worth changing" is
  written for a reviewer, and following it here is how a release note becomes a design memo. Cut
  it anyway and tell the author the rule needs scoping to contributors.
- **End a breaking entry with the concrete migration:** old spelling → new spelling, or the
  exact `jq` path that changed, or the search-and-replace to run.
- **Use current spellings** — the terminology the census settled on, not what the code called it
  when the change was written.
- **Length, counted:** 40–90 words for most entries; two or three short paragraphs for a
  genuinely large one. Count each entry before and after and report both numbers. An entry that
  came out the same length it went in was reorganised rather than rewritten, and the wall of text
  is still a wall — a bolded lead-in on an uncut paragraph just gives the wall a door. Entries
  written as working notes typically run a quarter to two-fifths rationale by volume.

## Landing page and marketing copy

The highest-stakes copy in the project and the most likely to be stale, because it's written
once at launch and the product moves.

- **State the premise before the capabilities.** A visitor who doesn't know what gap you fill
  can't evaluate a feature list. Name the gap, then fill it.
- **Every code sample here is load-bearing** and gets the full verification treatment. A broken
  sample on the landing page is the first thing a prospective user tries.
- **Check the claims against the current product,** not the one that existed at launch. Setup
  steps that are no longer required, configuration that's now auto-discovered, commands that
  were renamed. "It's one TOML table" stops being true the moment the tool works with no config
  at all — and that's a *better* story going unclaimed.
- **One tagline, not four.** Landing pages accumulate variants across the hero, the page title,
  the meta description, and the repo description. They should agree.
- **Cut version archaeology.** "In 0.2.0, `exclude` gained per-task globs" tells a new visitor
  nothing; they're starting at the current version.
- **Marketing register still means no fluff.** Superlatives, "blazingly fast", and
  "revolutionary" read as noise to a technical audience. Concrete beats enthusiastic: real
  output, real config, real numbers.

## Guides and reference docs

- **Define the terminology where readers meet it,** near the top of the overview — one sentence,
  before the words start doing work. Two similar words should only both survive when the census
  earned it, and a definition is not a substitute for the rename: if the secondary word still
  appears throughout the page, the sentence you added just documents the confusion.
- **One heading, one job.** A section covering two things hides the second from anyone
  scanning headings.
- **Reference docs are consulted, not read.** Optimize for the reader who arrived by search and
  needs one fact: tables over prose for enumerable things, headings that name what someone would
  search for, no narrative that has to be read from the top.
- **"Should", "may", and "must" are commitments** in reference material. Use them deliberately.
- **Order guides by what a reader does first,** not by internal architecture.
- **Never edit generated reference pages.** They carry a "do not edit" banner and are rebuilt
  from the code. Edit the source definition, run the regeneration recipe, commit both.

## README

The README serves at least three readers at once, and they want different things: someone
deciding whether to try it, someone installing it, and someone who already uses it and needs a
fact fast.

- **The first paragraph is for the first reader.** Premise, then what it does, then install.
- **Don't let it become a second docs site.** A long README duplicating the docs will drift from
  them. Prefer a short README that links out; when you find duplicated content, pick which copy
  is canonical and reduce the other to a pointer.
- **Verify the install instructions actually work** for each platform listed, and that the
  versions and URLs are current.
- **Check the badges** still point at live services and passing states.

## Code-level copy

Doc comments, CLI help text, and error messages are user-facing copy that happens to live in
source files. Two consequences shape how you edit them:

**They may be upstream of generated artifacts.** CLI help text commonly generates a reference
page and man pages. Edit the definition, then run the regeneration recipe. Never edit both ends.

**They're often under test.** Snapshot tests assert on help output, error strings, and JSON
payloads. Run the test suite after editing, and when a snapshot diff appears, read it — a
snapshot accepted without looking is a wire-format change accepted without looking.

### CLI help text

- **One line, imperative, no trailing period** for a flag or subcommand summary, matching the
  convention already in the file.
- **Terminology must match the docs exactly.** Help text is where competing words hide longest,
  because each string is read in isolation and looks fine. A single command's help reading
  *"Packages to run in; all members when omitted"* is the census failing in one sentence.
- **Say what it does, not what it is.** "Only packages owning files changed since this git ref"
  beats "Since ref filter".
- **Name the default** when there is one and it's not obvious.

### Error messages

The highest-value copy in the whole product, because every reader of one is stuck.

- **Say what went wrong, where, and what to do next.** The third part is the one usually missing.
- **Name the file and the offending value** when the error is about input.
- **Use the same words as the docs and config keys** so the reader can search for them.
- **Don't blame the user, and don't apologize.** State the condition.

### Doc comments

- **Public API docs are user-facing; private ones aren't.** Spend the effort on the public
  surface and on module-level docs that orient a reader.
- **Document the contract**, not the implementation: what it guarantees, what it returns, when
  it fails. Implementation notes belong in ordinary comments, which are free to be internal.
- **Fix stale identifier names in doc comments** during a terminology pass — they're prose and
  cost nothing to change, unlike the identifiers themselves.
