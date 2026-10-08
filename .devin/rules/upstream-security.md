---
trigger: model_decision
description: Handling suspected security vulnerabilities in upstream projects
---

# Upstream security findings

If work in this repo uncovers a **possible** security vulnerability in
an upstream project — Apple `container`, dependencies, base images,
tools we wrap — treat it as sensitive until triaged, even before it is
confirmed.

- **Never commit details to shared branches.** No finding details
  (affected component/code path, reproducer, exploit hints) on `main`,
  `release/*`, `wip/*`, `feature/*`, or any branch that could be
  pushed. Once pushed, commit history is public record.
- **If it must be recorded, use a `local/` branch.** `local/` branches
  stay unpushed. Park the analysis there while deciding whether to
  report it upstream or it turns out to be a false positive — and do
  not push that branch.
- **Sanitize commit messages and the `Prompt:` block.** Do not name the
  vulnerable component, describe the weakness, or quote the user's own
  description of the finding — even when the details came from the
  user's prompt. Summarize neutrally (e.g. "upstream security review")
  in `next-commit.txt`, PR bodies, issues, and comments.
- **Keep specifics out of public docs.** `doc/security.md`, `doc/TODO.md`,
  progress logs, and tracked notes cover *our* trade-offs; upstream
  findings are not ours to publish there.
- **Reporting upstream is the user's call.** Present the finding and a
  suggested disclosure path (the upstream project's private security
  contact per its policy), then wait for direction before doing
  anything public with it.
