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

generated-by: agent (models)
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

### AI Attribution

- Always end with the AI attribution section
- Follow the format specified in `attribution.md`
- Ask the user for the model name if unknown

## Review Before Writing

Before composing the commit message:

1. Check staged changes (`git diff --cached`) to confirm what is in scope
2. For amend commits, also review the HEAD commit (`git show --stat HEAD`)
3. Ensure bullets match actual changes, not planned or discussed work
