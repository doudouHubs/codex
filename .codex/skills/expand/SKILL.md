---
name: expand
description: "Use when the user explicitly invokes $expand or explicitly asks to incrementally update the project root Expand.md from personal Git changes."
metadata:
  short-description: "按 Git 增量维护个人功能说明"
---

# Expand

## Outcome

Maintain the repository-root `Expand.md` as a product-oriented record of the current Git author's added capabilities. Preserve useful manual content and update only facts supported by the repository state.

## Fact Sources

Read these sources before editing:

- The repository instructions, especially `AGENTS.md` and any more specific instructions for `Expand.md` or `.codex/skills`.
- The current Git identity from `git config user.name` and `git config user.email`. Do not guess authorship when the identity is missing or ambiguous.
- Personal history from `git log --all --author=<current identity>` with commit subjects, dates, and hashes.
- The current branch, `git status --short --untracked-files=all`, and bounded `git diff HEAD` output for uncommitted work.
- The existing root `Expand.md`, if present.

Use the current author's Git metadata as the authorship boundary. Do not treat every commit on the branch as a personal contribution.

## Incremental Update

1. Compare the collected facts with the existing document before writing.
2. Preserve existing headings, manual explanations, product tone, and valid commit evidence.
3. Add or revise only newly discovered feature areas, user-visible behavior, status labels, and commit references.
4. Classify changes into product features, engineering support, merge/upstream work, release or maintenance work, and current uncommitted work.
5. Mark committed capabilities as `已落地` and uncommitted capabilities as `开发中`. Never describe uncommitted code as released.
6. Describe behavior and user value before implementation details. Keep cross-module implementation notes in an engineering-support section.
7. Keep merge commits and upstream-only changes out of personal feature claims; mention them only as supporting context when they affect the resulting product behavior.
8. If `Expand.md` is missing, create a concise baseline using the same evidence and status rules instead of inventing history.

The document should normally include a feature overview, user-facing workflow sections, engineering-support notes, and a Git evidence index. Do not add a second report file, duplicate the whole Git log, or expose the user's email unless the existing document already requires it.

## Safety and Verification

- Modify only the repository-root `Expand.md` unless the user explicitly requests a skill or supporting-file change in the same task.
- Never revert, stage, commit, or rewrite unrelated worktree changes. Ignore line-ending-only changes and generated snapshots when they do not represent a new user-visible capability.
- Do not run builds or Rust tests for a documentation-only update.
- Keep new or updated files UTF-8, preserve the repository's existing line-ending style, and avoid trailing whitespace.
- Run `git diff --check -- Expand.md` and inspect `git status --short --untracked-files=all` before reporting completion.
- Report any missing Git identity, ambiguous authorship, or source contradiction instead of silently choosing a scope.
