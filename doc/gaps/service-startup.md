# Gap: service startup / shutdown semantics

- **Status:** by design at L0
- **Fix level:** structural -- unchanged at every level ≤ [L2.5], since
  the services *are* the product
- **Tracking:** [#11]

## What WSL does

`wsl --shutdown` stops the whole WSL VM platform. `wsl` otherwise
auto-starts its VM transparently.

## What we have today

`cm` auto-starts `container` services before machine ops
(`ensure_started` → `container system start`), matching WSL's
transparent-start behavior. The divergence is at shutdown:
`cm --shutdown` stops all *machines* but leaves `container` services
(apiserver, plugins) running -- `container system stop` is the full
shutdown, but that also stops unrelated containers, which is too broad
for a WSL `--shutdown` semantic.

## Options

- **L0 -- current behavior:** `--shutdown` stops machines, document
  `container system stop` for full teardown.
- **L0 -- "stop what we started":** if `cm` was the process that ran
  `system start`, and a later `cm --shutdown` finds **no machines and no
  regular containers running**, it could finish the job with
  `container system stop` -- restoring the machine to the state before
  `cm` touched it. Feasible today:
  - Record that we started services (a small state file, e.g.
    `~/.local/state/cm/services-started`, written by `ensure_started`
    when it actually starts them).
  - On `--shutdown`: after stopping machines, check
    `container list --format json` (regular containers) and
    `machine list` -- if both empty *and* our state file exists,
    `system stop` and remove it.
  - Caveats: a race with other concurrent `cm`/GUI users starting work
    between our checks; services may have been started by us but adopted
    by others meanwhile. Mitigate by treating the check as best-effort
    -- the cost of a missed stop is just idle launchd services, and the
    cost of a premature stop is that the next invocation re-starts.
  - Alternatively the inverse heuristic without state: offer
    `cm --shutdown --all` (or `--system`) that always `system stop`s --
    explicit, race-free, no bookkeeping.
- **L2.5:** our own plugin could track "machines we started" vs
  pre-existing services and idle-stop; speculative value.
- **L3:** only level where lifecycle is fully ours -- and also where we
  own *everything* else. Not a reason to go there.

## Recommendation

Keep `--shutdown` conservative by default, but the "stop what we
started" refinement is a worthwhile L0 enhancement -- it makes `cm` a
better guest on a shared system without changing the default contract.
Ship it behind correct detection (state file + double-check idleness);
add the explicit `cm --shutdown --all`/`--system` variant regardless,
since it's two lines and unambiguous.

[#11]: https://github.com/daphnediane/container-distro/issues/11
[L2.5]: ../lower-level-integration.md#the-plugin-route--l25
