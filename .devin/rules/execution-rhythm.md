---
trigger: model_decision
description: Guidelines for implementing work items and plan phases
---

# Execution Rhythm

## Core Principles

- **Work in complete units**: Finish one item/phase before starting the next
- **Scope discipline**: Implement only the current item/phase; resist scope creep
- **Always green**: `cargo test` must pass at every commit
- **Document as you go**: Update inline docs and relevant documentation files

## Progress Log

Maintain a running log at `.devin/progress.md` (git-ignored — do not commit it):

- Record each user prompt (verbatim or summarized) and the status of planned work for it
- Update statuses as work proceeds: pending → in progress → done/blocked
- The commit-changes and amend-changes skills examine and clear this log after each commit

## Committing

Follow `.devin/skills/commit-changes/SKILL.md` for the commit workflow. See `.devin/rules/comment-file.md` for format and `.devin/rules/attribution.md` for AI attribution.

Always propose the commit command for user approval rather than auto-running.
