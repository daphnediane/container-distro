---
trigger: model_decision
description: Guidelines for implementing work items and plan phases
---

# Execution Rhythm

## Core Principles

- **Work in complete units**: Finish one item/phase before starting the next
- **Scope discipline**: Implement only the current item/phase; resist scope creep
- **Always green**: `cargo test` must pass at every commit
- **Document as you go**: Update inline docs and relevant documentation
  files. When a `docs/work/` item is completed, delete its file and
  record the user-facing change under the `## X.Y.Z-dev` section of
  `CHANGELOG.md` (matching the dev version in `Cargo.toml`) in the same
  commit — see `docs/design/release.md`. User-facing work that was never
  a work item gets a changelog entry directly.

## Progress Log

Maintain a running log at `.devin/progress.md` (git-ignored — do not commit it):

- Record each user prompt (verbatim or summarized) and the status of planned work for it
- Update statuses as work proceeds: pending → in progress → done/blocked
- The commit-changes and amend-changes skills examine and clear this log after each commit

## Committing

Follow `.devin/skills/commit-changes/SKILL.md` for the commit workflow. See `.devin/rules/comment-file.md` for format and `.devin/rules/attribution.md` for AI attribution.

Run the commit command directly — the tool permission prompt gives the user a chance to review the message. Only pause to confirm intent first when running in a mode where commands execute unprompted.
