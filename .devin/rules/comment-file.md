---
trigger: model_decision
description: Commit message format and structure
---

# Commit Message Format

## Output File

Write commit messages to `./next-commit.txt` in the repository root.

**Important:** This file may already exist from previous sessions. Use the `edit` tool to replace its contents rather than `write_to_file`.

## Template Structure

```text
<tag>: <short concise subject> [<work item>]

<Description paragraph, typically one sentence>

- <Brief list of work done>
- <More work> [<work item>]
  - <Nested details>
- <More work>

Prompt: [summarized|verbatim]
<the user request that motivated this commit>

Followup: [summarized|verbatim]
<later user prompts handled in this commit, if any>

Co-Authored-By: <agent/model name> <noreply email>
```

## Format Rules

### Subject Line

- `<tag>` follows conventional commit style: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `style`, `build`, `ci`, `perf`
- Keep the subject under 50 characters when possible
- Use imperative mood ("Add" not "Added")

### Description

- One sentence summarizing the change from the user's perspective
- Focus on what and why, not how

### Bullet Points

- List significant changes
- Work item references like `[FEATURE-007]` go inline after relevant bullets
- Use nested bullets for implementation details
- Keep bullets accurate to what actually changed

### Prompts

- After the bullets, include a `Prompt:` block recording the user request that motivated the commit
- Note whether it is quoted verbatim or summarized
- Add `Followup:` blocks for additional user prompts handled in the same commit
- Keep these blocks between the bullets and the attribution trailer
- Omit only when there was no user prompt (e.g., automated or self-directed work)

### AI Attribution

- Always end with a `Co-Authored-By` trailer crediting the AI agent
- Follow the format specified in `attribution.md`
- Ask the user for the model name if unknown

## Review Before Writing

Before composing the commit message:

1. Check staged changes (`git diff --cached`) to confirm what is in scope
2. For amend commits, also review the HEAD commit (`git show --stat HEAD`)
3. Ensure bullets match actual changes, not planned or discussed work
