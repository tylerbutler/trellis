//! Embeds `VERGEN_GIT_*` env vars so dev builds can report the commit they
//! were built from. The default emitter degrades to placeholder values when
//! git metadata is unavailable (e.g. a crates.io tarball), so builds never
//! fail on missing git.

use vergen_gitcl::{Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gitcl = GitclBuilder::default()
        .describe(true, true, None)
        .sha(true)
        .dirty(true)
        .build()?;
    Emitter::default().add_instructions(&gitcl)?.emit()?;
    Ok(())
}
