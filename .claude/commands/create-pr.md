---
allowed-tools:
- Skill
- Bash
- Read
- Grep
- Glob
description: Commit the current changes and create a pull request using the repository’s
  commit rules.
---

Create a pull request that contains the current repository changes.

1. Inspect the working tree, current branch, remote, default branch, commits, and full diff. Read any pull request template and repository contribution or commit instructions.
2. Find an available skill related to commits or commit messages. If one exists, invoke it and use its rules for the commit message, pull request title, and pull request description. Repository-specific instructions take precedence. Do not invent a separate PR writing style.
3. If the working tree has changes, stage them and commit them with the generated commit message. Do not amend existing commits or include unrelated files.
4. If the current branch is the default branch, create a short descriptive branch before committing. Never force-push or rewrite history.
5. Push the branch and create the pull request with `gh pr create`. Use the repository’s default branch as the base.
6. Use the commit-style subject as the PR title. Use the commit-style body as the basis of the PR description, adapted only as needed to satisfy the PR template. Preserve required issue references, breaking-change notes, migration notes, and test information.
7. Return the pull request URL and a concise summary of the actions performed.

Invoking this prompt is explicit authorization to commit the current changes, push the branch, and create the pull request.