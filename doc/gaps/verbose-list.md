# Gap: `-v`/`--verbose` on `--list` is a no-op

**Status:** closed — `cm -l`, `-l -v` (exact WSL layout, VERSION `2`), and
`-l -v -v` (extra columns from `machine inspect`) are rendered by `cm`
**Fix level:** **L0** — the data is already available via `machine inspect`;
lower levels would only make it cheaper

## What WSL does

`wsl -l -v` adds columns: `NAME STATE VERSION` — per-distro running
state and WSL version (1/2).

## What we have today

`cm -l -v` is accepted for WSL compatibility but ignored — `container
machine list` has no verbose mode and `cm` passes the flag through
unchanged.

## Options

- **L0 — implement it.** `container machine inspect <m>` returns JSON
  with everything we'd want: `status`, `containerId`, `ipAddress`,
  `diskSize`, `platform`, `initialized`, `createdDate`. `cm -l -v` could
  inspect each machine and print an enriched table — STATE column maps
  to `status`; WSL's VERSION column has no analogue (platform is the
  closest). Cost: N extra subprocesses per `-l -v`.
- **L1/L2 — structured data direct.** Same fields without the N+1
  subprocess pattern.
- **L3** — trivially included if we ever own the model.

## Recommendation

Implement at L0 when convenient — the interesting part is deciding the
column set, not the plumbing.
