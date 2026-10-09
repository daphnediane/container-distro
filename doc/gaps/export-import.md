# Gap: `--export` / `--import`

- **Status:** implemented (`container` 1.5.0, build d265d66). Machine
  export works around the upstream `container export` rootfs bug with a
  scratch regular container; machine import works, modulo a generic
  first-boot flake it retries. Distros use `container distro
  export`/`import` directly.
- **Fix level:** shipped as host-side workarounds; the export bug and a
  `--no-boot` boot bug are both still worth filing upstream
- **Tracking:** [#2] (the feature); [#15] (dropping the workarounds
  once upstream is fixed)

## What WSL does

- `wsl --export <Distro> <FileName>` -- writes the distro's root
  filesystem as a tar.
- `wsl --import <Distro> <InstallLocation> <FileName>` -- registers a
  new distro whose rootfs comes from a tar (e.g. a previous export).

## What we have today

`cm --export NAME FILE` and `cm --import NAME INSTALL_LOCATION FILE`,
covering machines and distros in one namespace:

- `--export` on a distro is `container distro export`. On a machine it
  stops the machine, clones its `rootfs.ext4` into a scratch regular
  container, runs `container export` on that, deletes the scratch
  container, and restarts the machine. `FILE -` writes stdout.
- `--import` wraps the tar in an OCI image layout
  (`container_distro::oci::build_layout`), `container image load`s it, and
  `machine create`s it — then probes with `machine run -- true` and
  leaves the machine stopped, like `wsl --import`. With `--distro` it
  is `container distro import` instead. `FILE -` reads stdin;
  `INSTALL_LOCATION` is accepted and ignored (machine state lives in
  `container`'s app root).
- Names are lowercase DNS-style — `wsl --import` accepts arbitrary
  distro names, but `container machine` requires it and `cm` holds
  distros to the same rule.

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
machine plugin's state instead -- the container's `rootfs.json`:

```json
{"source": ".../plugin-state/machine-apiserver/machines/alpine/rootfs.ext4",
 "destination": "/", "type": {"block": {"format": "ext4", ...}}}
```

Export ignores the configured rootfs mount source. It's an upstream bug.
Regular containers, including `container distro` ones, are unaffected.

The mechanism (verified 2026-10-04): a container's data dir holds only
`runtime-configuration.json` until first start; then the daemon copies
the image's materialized rootfs (`snapshots/<image-digest>/snapshot`, an
ext4 file) to `containers/<id>/rootfs.ext4`. Machines instead carry
`options.rootFsOverride` in `runtime-configuration.json` pointing at
their plugin-state ext4 -- no copy. Two consequences: `export` also
fails on *never-booted* regular containers (no `rootfs.ext4` yet), and
the same override lets `container distro set` carry a booted rootfs
across a recreate. We only patch `runtime-configuration.json` on
containers `container distro` itself created -- never a machine's
backing container -- and point the override at the conventional
`containers/<id>/rootfs.ext4` path, so `export` still works afterwards
and `delete` removes the disk with the container.

**Upstream:** no existing issue found (searched 2026-10-03: "export",
"machine export", "rootfs.ext4", "snapshot disk"). The related issues
are about other things: [apple/container#1400] (export of running
containers, closed), [apple/container#1265] (export should write a tar,
closed), and [apple/container#2325] (`export -o` deletes an existing
directory, open). A new issue is still warranted, with the repro above
— tracked by [#15].

### Workarounds

- **Scratch regular container (shipped).** `ops::export_machine`
  creates a scratch `container create` (never started), clonefile-copies
  the machine's `rootfs.ext4` — found via `machine_rootfs`, i.e. the
  `rootfs.json`/`rootFsOverride` source, not a hard-coded path — into
  `containers/<scratch-id>/rootfs.ext4`, runs `container export` on the
  scratch container, and deletes it on drop. That exploits the exact
  assumption behind the bug: export reads the conventional path
  literally, no matter where the container would have mounted its disk
  from. The machine is stopped while its disk is cloned — a running
  machine's clone is only crash-consistent — and restarted if it was
  running. No guest `sh`/`mount`/`tar` needed. Two implementation
  notes: `container create` requires a command/entrypoint even for a
  container that never starts (our older imported images carry no
  `Cmd`), so the scratch container passes an explicit `--entrypoint
  /bin/sh`; and a SIGKILLed export can leak the scratch container, so
  it carries an `export-scratch` label naming the machine.
- **`distro migrate` / `migrate --out`.** The same clone machinery —
  `migrate` stages the machine's disk into a distro, `migrate --out`
  replaces a new machine's disk with the distro's — still the better
  path for moving either direction without a tar round-trip.
- **Guest-side tar stream (prototyped, rejected).** Run
  `machine run --root -- sh -c '<bind-mount / and tar it>'` and capture
  stdout. It works (byte-identical over 3 runs, no first-write loss),
  but it depends on `sh`, `mount`, and `tar` being installed in the
  distro. That's the wrong dependency for an export tool.
- **Host-side ext4 read.** Take an APFS clone of the machine's
  `rootfs.ext4`, walk it with a pure-Rust read-only ext4 reader
  (`ext4-view`), write the tar with the `tar` crate. Now unnecessary —
  the scratch-container workaround covers it without an ext4 reader.
- **Upstream fix.** Once export honors the rootfs mount source, plain
  `container export <container-id>` on the machine's backing container
  is the whole implementation.

## Import: the boot failure was the generic first-boot flake

The prototype wrapped the rootfs tar in an OCI image layout (single
layer stored as-is, `diff_id` over the uncompressed stream), ran
`container image load`, and `container machine create --no-boot
local/<name>:imported`. Loading and creating succeeded; booting failed:

```console
$ container machine run -n a2 -- true
Error: The operation couldn’t be completed. Operation not supported by device
```

The machine's `stdio.log` shows busybox init running `/etc/inittab`,
failing `can't run '/sbin/openrc'`, then rebooting. **That signature is
not an import defect** — a stock `alpine:latest` machine reproduces it
exactly (verified 2026-10-06): the machine plugin's `/sbin.machine/init`
ends with an unconditional `exec /sbin/init`, alpine's inittab points
at openrc which the image doesn't ship, busybox init reboots, and the
second boot succeeds. It's the first-boot provisioning flake already
noted in container-internals.md, possibly related to
[apple/container#2024].

Missing OCI `config` fields were a real but *separate* issue: generated
import images carried only `Labels` — no `Env`/`Cmd`/`WorkingDir` — and
`container create` refuses an image with no command, which broke the
export scratch container. `oci::build_layout` now writes the
conventional defaults (`PATH`, `Cmd /bin/sh`, `WorkingDir /`), so
imported images also work via plain `container run`.

Two upstream quirks the implementation works around:

- **`machine create --no-boot` is effectively unbootable** — a
  `machine run` on a never-booted machine races the boot and every
  attempt failed in testing (1.5.0). `cm --import` therefore uses plain
  `machine create`, whose internal boot absorbs the first-boot
  provisioning reboot, rather than `--no-boot`.
- **The first `machine run` after `machine create` can still lose the
  settle race** — the import boot probe retries with a pause before
  warning (the machine is left created, not deleted).

## Caveats

- An export of a running machine briefly stops it; the resulting clone
  is exact, not crash-consistent.
- Importing an image is trusting it with your identity — see
  security.md.

## Recommendation

Shipped as described. Remaining — tracked by [#15]: file the two
upstream bugs (`container export`'s `containers/<id>/rootfs.ext4`
assumption, and `--no-boot` machines failing every `machine run`),
then swap the workarounds for the primitives.

[#2]: https://github.com/daphnediane/container-distro/issues/2
[#15]: https://github.com/daphnediane/container-distro/issues/15
[apple/container#1265]: https://github.com/apple/container/issues/1265
[apple/container#1400]: https://github.com/apple/container/issues/1400
[apple/container#2024]: https://github.com/apple/container/issues/2024
[apple/container#2325]: https://github.com/apple/container/issues/2325
