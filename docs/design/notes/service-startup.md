# Gap: service startup / shutdown semantics

- **Status:** implemented at L0 — "stop what we started" shipped
- **Fix level:** structural -- unchanged at every level ≤ [L2.5], since
  the services *are* the product
- **Tracking:** [#11]

## What WSL does

`wsl --shutdown` stops the whole WSL VM platform. `wsl` otherwise
auto-starts its VM transparently.

## What we have today

`cm` auto-starts `container` services before machine ops
(`ensure_started` → `container system start`), matching WSL's
transparent-start behavior — and records it: `ensure_started` writes a
`services-started` marker in the state dir when *it* ran
`system start` (not when services were already up). At shutdown,
`cm --shutdown` stops all machines and distros, then — when the
marker exists and a fresh listing shows no machines and no regular
containers still running — finishes with `container system stop` and
clears the marker. A user-started service tree (no marker) or a busy
one (anything left running) is left alone; `--shutdown --system`
remains the explicit "stop everything regardless" hammer. The marker
is cleared whenever `system stop` succeeds or services are found
already down, so a stale one can't survive across reboots.

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

Shipped per the L0 "stop what we started" option above: marker +
idle double-check, conservative on any probe failure (a failed
`list` keeps services up). The remaining caveats stand — another
client can adopt our services between the check and the stop, whose
worst cost is that the next invocation re-starts.

[#11]: https://github.com/daphnediane/container-distro/issues/11
[L2.5]: ../lower-level-integration.md#the-plugin-route-l25
