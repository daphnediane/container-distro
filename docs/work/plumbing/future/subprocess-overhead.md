# Subprocess overhead

- **Issue:** [#9]
- **Milestone:** none (future -- post-1.0)

One `container` spawn per op plus `system status` checks; only worth
fixing (XPC/Swift client libraries, L1/L2) if it ever hurts. "Rung"
references are the integration levels in [lower-level-integration].

See [subprocess-overhead].

[#9]: https://github.com/daphnediane/container-distro/issues/9
[lower-level-integration]: ../../../design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[subprocess-overhead]: ../../../design/notes/subprocess-overhead.md
