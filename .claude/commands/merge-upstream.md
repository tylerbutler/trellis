---
allowed-tools:
- Skill
- Bash
- Read
- Edit
- Write
- Grep
- Glob
description: Merge the latest upstream main into the current branch, or resolve and
  complete an existing merge.
---

Merge the latest upstream main branch into the current branch. If a merge is already in progress, resolve its conflicts and complete that merge instead.

1. Read the repository contribution and commit instructions. Inspect the current branch, working tree, staged changes, remotes, and Git operation state. Preserve unrelated work. Do not reset, discard changes, rewrite history, or switch branches.
2. Check for `MERGE_HEAD` with `git rev-parse -q --verify MERGE_HEAD`. If it exists, inspect the pending merge and go to step 5, even if someone has already resolved and staged all conflicts. Do not fetch, start another merge, or abort the existing merge. If a rebase, cherry-pick, revert, or other operation is in progress instead, stop and report it. Do the same if unmerged index entries exist without `MERGE_HEAD`.
3. For a new merge, require an attached branch and a clean working tree and index. If local changes exist, stop and report them; do not stash or commit them. Use the `upstream` remote if it exists; otherwise use `origin`. If neither exists, stop and report the missing remote. Use that remote's `main` branch, not the current branch's tracking branch or a stale local `main`.
4. Fetch `main` from the selected remote with `git fetch <remote> refs/heads/main`, then record the fetched commit with `git rev-parse FETCH_HEAD`. If the fetch fails, stop and report the error. Merge that commit into the current branch with `git merge --no-edit <fetched-commit>`. Accept a fast-forward or an already-up-to-date result without creating an extra commit. If the command fails, inspect Git state: continue only for merge conflicts; report other errors.
5. Resolve each unmerged path. Read the base, current, and incoming versions where available, plus related code and tests. Preserve the intent of both branches and any existing resolutions. Handle content, rename, delete, and binary conflicts; do not choose an entire side just to remove conflicts. Use the repository's tools to regenerate generated files and lockfiles when needed. If you cannot determine the correct resolution, stop and request guidance. Leave the merge in progress and explain what remains.
6. Stage only the resolved paths, including resolved deletions. Do not use `git add .` or `git add -A` across the repository. Review the full staged merge result and any remaining unstaged changes. Keep the merge's existing staged changes, but stop if you cannot distinguish them from unrelated work that would enter the commit.
7. Confirm that `git ls-files -u` has no entries and that the merge changes contain no accidental conflict markers or whitespace errors. Run the smallest existing tests, build, or lint commands that cover the affected behavior. Fix problems caused by the merge. If validation fails or cannot run, report the blocker and leave the merge in progress rather than claim success.
8. If `MERGE_HEAD` still exists, complete the merge with a merge commit. Use an available commit-message skill and follow repository commit rules; retain the pending merge message when those rules permit. Do not amend a commit. Confirm that Git cleared `MERGE_HEAD` and that no unmerged entries remain. For a new upstream merge, also confirm that the fetched commit is an ancestor of `HEAD`.
9. Report whether you completed an existing merge, merged upstream main, or found the branch up to date. Summarize conflict resolutions and any remaining local changes or blockers. Do not push.

Invoking this prompt is explicit authorization to fetch, merge, edit files to resolve conflicts, and create the merge commit. It does not authorize a push or a commit of unrelated work.