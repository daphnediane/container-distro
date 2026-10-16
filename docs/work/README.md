# Work items

The working roadmap -- open work only, one file per item at
`<area>/<milestone>/<feature>.md`:

- `<milestone>` mirrors the GitHub milestone on the item's issue
  (`1.0`, `distrofs`). `future/` holds items with no milestone
  (post-1.0 or undated).
- Every item links its GitHub issue; the issue and the design docs it
  cites are the source of truth for details. "Rung" references are the
  integration levels in [lower-level-integration].
- When an item is completed its file leaves this tree: the
  user-facing change is recorded in the `X.Y.Z-dev` section of
  [CHANGELOG.md](../../CHANGELOG.md), which is the permanent record of
  what shipped.

## Index

### `distrofs` milestone

- [interop/distrofs/distrofs-shipping.md](interop/distrofs/distrofs-shipping.md)
  -- decide how `distrofs` ships ([#35])
- [interop/distrofs/host-file-access.md](interop/distrofs/host-file-access.md)
  -- host access to distro filesystems (`\\wsl$`) ([#27])

### `future` (no milestone)

- machines/ --
  [configuration-files](machines/future/configuration-files.md)
  ([#29], [#33]),
  [export-import-upstream](machines/future/export-import-upstream.md)
  ([#15]),
  [mounts-outside-home](machines/future/mounts-outside-home.md) ([#3]),
  [machine-run-first-write](machines/future/machine-run-first-write.md)
  ([#4]),
  [localhost-forwarding](machines/future/localhost-forwarding.md)
  ([#5]), [gui-apps](machines/future/gui-apps.md) ([#7])
- interop/ -- [remote-wsl](interop/future/remote-wsl.md) ([#8])
- plumbing/ --
  [launcher-entries](plumbing/future/launcher-entries.md) ([#33]),
  [profile-styling](plumbing/future/profile-styling.md) ([#34]),
  [locate-own-commands](plumbing/future/locate-own-commands.md) ([#36]),
  [subprocess-overhead](plumbing/future/subprocess-overhead.md) ([#9])

[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#7]: https://github.com/daphnediane/container-distro/issues/7
[#8]: https://github.com/daphnediane/container-distro/issues/8
[#9]: https://github.com/daphnediane/container-distro/issues/9
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#27]: https://github.com/daphnediane/container-distro/issues/27
[#29]: https://github.com/daphnediane/container-distro/issues/29
[#33]: https://github.com/daphnediane/container-distro/issues/33
[#34]: https://github.com/daphnediane/container-distro/issues/34
[#35]: https://github.com/daphnediane/container-distro/issues/35
[#36]: https://github.com/daphnediane/container-distro/issues/36
[lower-level-integration]: ../design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
