# Gap: `--export` / `--import`

**Status:** punted — the obvious L0 recipe doesn't work for machines
(`container` 1.5.0, build d265d66). Prototype parked on branch
`wip/export-import`.
**Fix level:** upstream fix for export; host-side ext4 reader (L0, no
guest tools) as a workaround; import blocked on an unexplained boot
failure

## What WSL does

- `wsl --export <Distro> <FileName>` — writes the distro's root
  filesystem as a tar.
- `wsl --import <Distro> <InstallLocation> <FileName>` — registers a new
  distro whose rootfs comes from a tar (e.g. a previous export).

## What we have today

Nothing exposed via `cm`.

## Export: `container export` can't snapshot machines

The intended recipe was `container machine inspect <m>` → `containerId`
→ `container export <cid> -o file.tar`. It fails:

```console
$ container export alpine-730439 -o /tmp/a.tar
Error: failed to export container (cause: "internalError: "failed to snapshot disk
in container alpine-730439 (cause: "unknown: "Error Domain=NSCocoaErrorDomain
Code=260 "The file “rootfs.ext4” couldn’t be opened because there is no such
file." UserInfo={NSFilePath=.../com.apple.container/containers/alpine-730439/rootfs.ext4, ...
```

**Cause.** `export` assumes the root disk lives at
`<app-root>/containers/<cid>/rootfs.ext4`, which is true for regular
containers. A machine's backing container mounts its rootfs from the
machine plugin's state instead — the container's `rootfs.json`:

```json
{"source": ".../plugin-state/machine-apiserver/machines/alpine/rootfs.ext4",
 "destination": "/", "type": {"block": {"format": "ext4", ...}}}
```

Export ignores the configured rootfs mount source. It's an upstream bug.
Regular containers, including the planned `container distro` ones, are
unaffected.

**Upstream:** no existing issue found (searched 2026-10-03: "export",
"machine export", "rootfs.ext4", "snapshot disk"). The related
issues are about other things: [#1400](https://github.com/apple/container/issues/1400)
(export of running containers, closed), [#1265](https://github.com/apple/container/issues/1265)
(export should write a tar, closed), and [#2325](https://github.com/apple/container/issues/2325)
(`export -o` deletes an existing directory, open). A new issue is
warranted, with the repro above.

### Workarounds considered

- **Guest-side tar stream (prototyped, rejected).** Run
  `machine run --root -- sh -c '<bind-mount / and tar it>'` and capture
  stdout. It works (byte-identical over 3 runs, no first-write loss), but
  it depends on `sh`, `mount`, and `tar` being installed in the distro.
  That's the wrong dependency for an export tool.
- **Host-side ext4 read (preferred if we revisit).** Take an APFS clone
  of the machine's `rootfs.ext4` (`cp -c`, instant). The clone is
  crash-consistent if the machine is running and exact if it's stopped.
  Walk it with a pure-Rust read-only ext4 reader (`ext4-view`) and write
  the tar with the `tar` crate. No guest involvement. To verify: does the
  reader expose device major/minor and xattrs? Also, the machine plugin's
  state path is internal, so read it from `container inspect`'s rootfs
  source rather than hard-coding it.
- **Upstream fix.** Once export honors the rootfs mount source, plain
  `container export` is the whole implementation.

## Import: imported machines don't boot

The prototype wrapped the rootfs tar in an OCI image layout. The single
layer is stored as-is (plain or gzip), with the `diff_id` computed over
the uncompressed stream. It then ran `container image load` and
`container machine create --no-boot local/<name>:imported`. Loading and
creating both succeed, and the machine is listed. **Booting fails:**

```console
$ container machine run -n a2 -- true
Error: The operation couldn’t be completed. Operation not supported by device
```

The machine's `stdio.log` shows busybox init running `/etc/inittab`,
failing `can't run '/sbin/openrc'`, then rebooting. The source machine
(stock `alpine:latest`) has the same inittab and no openrc but boots
fine, so the difference lies elsewhere: image config, layer handling, or
init environment. Root cause not determined. It may be related to
[#2024](https://github.com/apple/container/issues/2024) (machine create
succeeds for images that then fail to boot, with a misleading error).

## Caveats (still apply)

- Machine names are lowercase DNS-style (`[a-z0-9-]`); `wsl --import`
  accepts arbitrary distro names — `cm` must validate.
- WSL `--import` takes an install location. Machines keep their state
  inside `container`'s app root, so `cm` would accept the argument for
  compatibility and ignore it.

## Recommendation

Punt for now. File the export bug upstream. Revisit with the host-side
ext4 reader for export if upstream is slow. For import, debug the boot
failure by diffing the inspect output and config of a stock-image
machine against an imported one. A `container distro` backend (regular
containers) sidesteps the export bug entirely, so export/import may be
best delivered there first.
