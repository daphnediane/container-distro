# Configuration files

- **Issues:** [#29], [#33]
- **Milestone:** none (future -- post-1.0)

Host-side `cm` config (L0) still open; the guest `wsl.conf` subset is
honored for distros (init reads the live file at boot, mode-gated;
snapshots in the journal) with deferred keys tracked separately.
"Rung" references are the integration levels in
[lower-level-integration].

See [configuration-files] and `design/security.md`.

[#29]: https://github.com/daphnediane/container-distro/issues/29
[#33]: https://github.com/daphnediane/container-distro/issues/33
[configuration-files]: ../../../design/notes/configuration-files.md
[lower-level-integration]: ../../../design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
