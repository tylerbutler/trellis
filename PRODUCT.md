# Product

## Register

brand

## Platform

web

## Users

Gleam monorepo maintainers: developers already running (or about to run) multi-package Gleam repositories who may coordinate packages with bash loops in justfiles, per-package YAML blocks, or inline sed in CI workflows. They are experienced engineers evaluating a tool, not browsing; they arrive skeptical and want to understand how it works before they run anything.

## Product Purpose

This surface is the public website for trellis — a landing page that introduces the tool plus reference documentation for its commands and configuration. Trellis itself is a single Rust binary that offers workspace tooling for multi-package Gleam repositories: task fan-out, dependency-graph introspection, versioning, changelogs, and publish orchestration, all derived from `gleam.toml` files that already exist. Success for the site is adoption: a visitor reads enough of the docs to trust the model, then installs trellis and adds it to their workspace.

## Positioning

Workspace tooling for multi-package Gleam repositories. Present trellis as one practical option, not the canonical way to organize Gleam projects. Describe the scope of Gleam's package commands factually when relevant, without framing the language's design as a failure. Lead with what trellis does and how it derives its workspace model from existing manifests.

## Conversion & proof

- Primary CTA: read the docs — the conversion path is evaluation-first, so the site's job is to get visitors into the documentation. Secondary CTA: the one-line install command (shell installer, Homebrew, or mise) for visitors ready to try it now.
- The line a visitor remembers after 10 seconds: "Workspace tooling for multi-package Gleam repositories, derived from gleam.toml."
- Belief ladder: (1) Coordinating a growing multi-package repository can involve repeated package lists, shell loops, and CI configuration. (2) Trellis can derive much of that information from the `gleam.toml` files already present. (3) It's safe to try: one binary, no lock-in, and `doctor` checks the invariants that remain explicit.
- Proof on hand: lattice as the case study — a real inventory of hand-maintained glue (justfile package lists, changie project blocks, workflow sed scripts, an external SHA-pinned action) that lattice replaced with trellis. The before/after table lives in `docs/DESIGN.md` §1.

## Brand Personality

Precise, calm, trustworthy. Engineering-grade confidence: the tool quietly does the right thing, and the site does too. Claims are exact and verifiable; the tone never oversells. Wit, if any, is dry and earned by specificity (naming the exact YAML that got deleted), never by exclamation.

## Anti-references

- Generic SaaS landing page: gradient heroes, floating product screenshots, pricing-page energy. Wrong register for a free, open-source CLI tool.
- Sterile default docs theme: an out-of-the-box Starlight/Docusaurus look with no identity of its own. The docs are the primary conversion surface, so they must feel designed, not generated.

## Design Principles

- **Docs are the pitch.** The conversion path runs through understanding, so documentation quality IS marketing. Reference pages get the same design attention as the landing page.
- **Proof by concrete example, not adjectives.** The lattice before/after — the actual glue that got deleted — carries the argument. Show real config, real commands, real output; never claim "simple" or "powerful" in the abstract.
- **Derive before declaring — the site too.** Trellis's own principle applies to its website: every element earns its place from the content; no decorative scaffolding, no section that exists because landing pages have one.
- **Calm over loud.** Precise typography, restrained motion, exact language. The visitor is a skeptical engineer; earn trust the way the tool does — by being correct and quiet about it.

## Accessibility & Inclusion

WCAG 2.1 AA: body text contrast ≥ 4.5:1, full keyboard navigation, visible focus states, and `prefers-reduced-motion` alternatives for all animation. Code samples and terminal output must remain readable at AA contrast in both light and dark themes.
