---
trigger: model_decision
description: Security review requirements when changing code, shell assets, or docs that touch the host/guest boundary, privilege, credentials, or security claims
---

# Security review

This project gives guests host reach (home mounts, SSH agent, published
ports) and guest-root conveniences (sudo/doas, `--cap-add ALL`) by
design — the trade-offs are spelled out in `docs/design/security.md`, not
accidental.

- **Think about security impact before committing.** Does the change add
  host surface, provision privilege, forward credentials, loosen a
  check, or weaken the VM/guest boundary? If yes, the security review is
  part of the same commit, not a follow-up.
- **Check both directions of the boundary.** Guest→host is the subtle
  one: host code trusting config, state, or output a guest can write is
  a confused deputy (the T6 trap — most findings C1–C12 are this shape).
  New host-trusted inputs need validation or a guest-unreachable
  location. This includes `sbin.distro/` shell assets — they run as
  PID 1 and root, so review them like the Rust, not like config.
- **Never commit secrets.** No keys, tokens, or credentials in code,
  tests, docs, or commit messages — and don't print them to logs.
- **Test hooks that touch trust must be opt-in per invocation.** Any
  seam that redirects downloads, swaps a verification input, or
  otherwise steers a privileged/integrity-critical operation for
  testing (fixture servers, alternate release roots, skip-check
  switches) must sit behind an explicit flag on that invocation —
  the `--dev` pattern — never an env var alone: a variable planted in
  a shell profile is a persistent, silent backdoor. Prefer local
  fixtures that keep the real checks running (version-bumped worktree
  builds, `python3 -m http.server` trees impersonating the real API)
  over weakening verification for the test. Env-only seams that merely
  select a *user*-privilege binary (`CONTAINER_CLI`) are PATH-
  equivalent — document them in the trade-off table, don't flag-gate
  them.
- **Document every new trade-off or shortcoming in `docs/design/security.md`.**
  New accepted risk → trade-off table entry; new violation/panic →
  findings table; new mitigation → note it under the risk it addresses.
- **Upstream findings follow `upstream-security.md` instead.** A
  suspected vulnerability in upstream code is parked on a `local/`
  branch — no details in shared branches, `docs/design/security.md`, or commit
  messages. (Linking an already-*public* upstream issue, as the findings
  table does, is fine.)
- **Docs may not oversell.** Anything marketed as "restricted",
  "hardened", "isolated", or similar must state next to the claim what
  it does *not* do (guest root, `set` reopening, `run --root`, etc.).
- **Reduction ≠ removal.** A flag that skips provisioning is a
  convenience boundary, not a wall — verify the protective code is
  absent or unexecuted (asset not mounted, not just an env check) when
  the claim is "cannot".
- **Keep `docs/design/security.md`'s checklist honest** — flip statuses as work
  lands, and add new entries when review uncovers a hole worth fixing
  later.
