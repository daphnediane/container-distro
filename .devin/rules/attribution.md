---
trigger: model_decision
description: AI assistance attribution for commit messages
---

# AI Attribution

## Format

All commit messages must include an AI assistance declaration at the end:

```
generated-by: agent (models)
```

## Getting the Model Name

1. **Preferred:** If the user or environment has already specified a model for this session, use that value.
2. **Otherwise:** Use the `ask_user_question` tool to ask the user which AI model is assisting them.
3. **Last resort:** Leave `[model]` as a placeholder for the user to fill in.

## Examples

```
generated-by: devin-desktop (SWE-2)
```

## Placement

- Always place the attribution after a blank line following the main commit message content
- This should be the final content in the commit message file
- No additional content should follow the model name
