# Gap: subprocess overhead and JSON scraping

- **Status:** open -- cosmetic/invisible to users
- **Fix level:** [L1/L2] (XPC or Swift client libraries)
- **Tracking:** [#9]

## What this is

`cm` is a subprocess wrapper: every operation is at least one spawned
`container` process, and machine ops additionally spawn
`container system status` first (`ensure_started`). Structured data
comes from scraping `--format json` output. This is all normal for a
compat shim -- it only becomes a gap if:

- latency matters (e.g. `cm -l` in a prompt or watch loop), or
- output formats drift and break parsing (the real risk -- JSON shape is
  less stable than CLI text).

## Options

- **L0 mitigation:** none needed yet. If flakiness appears, pin/verify
  the parsed fields and add a `container --version` floor check.
- **L1:** direct XPC removes subprocess cost but trades it for an
  undocumented message schema -- worse, not better.
- **L2:** `MachineAPIClient`/`ContainerAPIClient` via a small Swift shim
  dylib gives typed calls and real errors. This is the honest fix if
  scraping ever hurts.

## Recommendation

Do nothing. Revisit only if profiling shows subprocess time matters or
JSON drift actually breaks us -- then go L2, not L1.

## Related

- [exec-stdio] -- correctness problem on the same exec path:
  `machine run` drops the first guest→host write and needs `-i` for
  stdin

[#9]: https://github.com/daphnediane/container-distro/issues/9
[exec-stdio]: exec-stdio.md
[L1/L2]: ../lower-level-integration.md#rung-by-rung-how-low-can-cm-go
