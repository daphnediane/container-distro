# `cm --export` / `--import` via upstream primitives

- **Issue:** [#15]
- **Milestone:** none (future -- post-1.0)

Once `container export` honors a machine's rootfs mount source, drop
the scratch-container workaround; once a `--no-boot` machine can be
booted by `machine run`, use it and drop the create-boot probe. Both
bugs still need filing upstream.

Retested on `container` 1.6.1 (0.9.1 version-gating pass): export
still reads `containers/<id>/rootfs.ext4` literally and fails on an
overridden rootfs — workaround stays. `machine run` does boot a
`--no-boot` machine there, so that half of the precondition is met;
the export half still blocks the change.

See [export-import].

[#15]: https://github.com/daphnediane/container-distro/issues/15
[export-import]: ../../../design/upstream-limitations.md
