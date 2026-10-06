---
trigger: model_decision
description: AI assistance attribution for commit messages
---

# AI Attribution

## Format

All commit messages must end with a standard git `Co-Authored-By` trailer
crediting the AI agent that assisted:

```
Co-Authored-By: <agent/model name> <noreply email>
```

Use the standard trailer spelling so GitHub and other tools parse it and
surface the co-author credit.

## Getting the Model Name

1. **Preferred:** If the user or environment has already specified a model for this session, use that value.
2. **Otherwise:** Use the `ask_user_question` tool to ask the user which AI model is assisting them.
3. **Last resort:** Leave `[model]` as a placeholder for the user to fill in.

## Email Address

- Use the agent's standard attribution email when it has one:
  - Claude Code: `noreply@anthropic.com`
  - Devin: `158243242+devin-ai-integration[bot]@users.noreply.github.com`
- Otherwise use a `noreply`-style address associated with the agent's vendor,
  or ask the user which address to use.

## Examples

```
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Co-Authored-By: Devin (SWE-2) <158243242+devin-ai-integration[bot]@users.noreply.github.com>
```

## Placement

- Always place the trailer after a blank line following the main commit message content
- This should be the final trailer in the commit message file
- No additional content should follow the trailer

## GitHub Issues

Any issue filed on this project by an AI agent must end with an
attribution line identifying the agent and model, placed after the body
content and separated by a horizontal rule:

```
---

_Generated with [<agent name>](<agent URL>) (<model name>)._
```

Use the same agent/model identification as the commit trailer. This
applies only to issues created on this repository — never file issues on
upstream or third-party projects unless the user explicitly asks.
