---
name: commit-changes
description: Commit changes with formatted message and progress-log handling
---

# Commit Changes

1. Read `.devin/progress.md` if it exists — it records user prompts and work status since the last commit. Reflect anything still pending in your plan or the commit message.
2. Check if a commit template exists at `.templates/next-commit.txt`; otherwise use the base template at `.templates/next-commit-base.txt`.
3. Compose a commit message to `./next-commit.txt` following `.devin/rules/comment-file.md` (conventional-commit tag, subject, description, bullets).
4. Add the AI usage declaration after a blank line as specified in `.devin/rules/attribution.md`.
   a. If the model is unknown or the user hasn't previously specified a model for this session, use `ask_user_question` to ask.
5. Run the commit command (requires user approval since it creates a commit):
   `git add -A && git commit -F ./next-commit.txt && mv ./next-commit.txt ./next-amend.txt`
6. After a successful commit, clear `.devin/progress.md` (truncate or delete it) — the logged work is now committed.
