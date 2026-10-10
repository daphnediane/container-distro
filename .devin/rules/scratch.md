---
trigger: model_decision
description: Use the repo's gitignored scratch/ directory — not /tmp — for scratch work like pinned old releases, fixtures, and throwaway output
---

# Scratch work goes in `scratch/`

`scratch/` at the repo root is the canonical place for throwaway
artifacts: pinned old releases, test fixtures, downloaded files,
captured output, experiment directories.

- **Prefer it over `/tmp`/`$TMPDIR`.** Scratch inside the repo is easy
  to find, inspect, and diff — and cleanup is just removing the
  subdirectory you're done with (or all of `scratch/`).
- It's gitignored (`/scratch` in `.gitignore`), so nothing there lands
  in a commit by accident. `input/`, `output/`, and `docs-to-be-added/`
  are gitignored too, but `scratch/` is the convention — use it for
  anything new.
- Example: the release check pins the last pre-journal build with
  `cargo install --locked container-distro@0.4.2 --root
  scratch/old-0.4.2` (see `scripts/check-legacy-adoption.sh`).
- `mktemp` inside a script is still fine for truly transient files —
  this rule targets work products you'd want to keep or inspect.
