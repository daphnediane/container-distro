# `cm --export` / `--import` via upstream primitives

- **Issue:** [#15]
- **Milestone:** none (future -- post-1.0)

Once `container export` honors a machine's rootfs mount source, drop
the scratch-container workaround; once a `--no-boot` machine can be
booted by `machine run`, use it and drop the create-boot probe. Both
bugs still need filing upstream.

See [export-import].

[#15]: https://github.com/daphnediane/container-distro/issues/15
[export-import]: ../../../design/upstream-limitations.md
