---
name: amend-changes
description: Amend the previous commit with formatted message and progress-log handling
---

# Amend Changes

1. Read `.devin/progress.md` if it exists — it records user prompts and work status since the last commit. Reflect anything still pending in your plan or the amended message.
2. If the changes complete a `docs/work/` item, delete its file and record the user-facing change under the `## X.Y.Z-dev` section of `CHANGELOG.md` (matching the dev version in `Cargo.toml`). User-facing work that was never a work item gets an entry directly.
3. Check if a commit template exists at `.templates/next-commit.txt`; otherwise use the base template at `.templates/next-commit-base.txt`.
4. Check if `next-amend.txt` exists in the repository root:
   - If it exists, use the `edit` tool to update its contents.
   - Otherwise, read the current commit message with `git log -n1 --format="%B" HEAD` and create the file based on that content.
5. Compose the amended message in `./next-amend.txt` following `.devin/rules/comment-file.md`.
6. Add the `Co-Authored-By` attribution trailer after a blank line as specified in `.devin/rules/attribution.md`.
   a. If the model is unknown or the user hasn't previously specified a model for this session, use `ask_user_question` to ask.
7. Run the amend command (requires user approval since it amends a commit):
   `git add -A && git commit --amend -F ./next-amend.txt`
8. After a successful amend, clear `.devin/progress.md` (truncate or delete it) — the logged work is now committed.
