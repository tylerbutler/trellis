---
name: trellis-onboarding
description: >
  Use when the user asks to onboard to trellis, set up trellis, or otherwise mentions onboarding to trellis.
---

Onboard the repo to use trellis. This includes adding the trellis config file, converting any existing changelog and

## Initial Setup

First confirm that the project is a Gleam project. If not, explain to the user that trellis is designed for Gleam projects.

Then check the repository for existing tools for managing changelogs, handling releases, and task running. If any of those are found, ask the user if they want to keep using those tools or switch to trellis. If they want to switch, clarify that trellis will take over those responsibilities and that they will need to migrate any existing changelog or release notes to trellis.

## Migration

Refer to the documentation for trellis for instructions on how to migrate existing repos to trellis. Use what you learned from the initial setup to determine if any existing tools need to be migrated or removed. If the user is unsure, provide guidance on how to migrate their existing changelog and release notes to trellis.

## Reference

- Documentation for trellis: https://github.com/tylerbutler/trellis/tree/main/website/src/content/docs/docs, also published to https://trellis.tylerbutler.com.
