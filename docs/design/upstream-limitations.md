# Upstream `container` limitations and our workarounds

The places where `container` / `container machine` can't do what `cm`
and `container distro` need, what shipped in each gap's place, and the
upstream fix that would let the workaround be deleted. "Rung" references
are the integration levels in [lower-level-integration]; "T#" references
are the trade-off register in [security].

| Limitation                                           | Workaround shipped                           | Upstream fix                              | Tracking  |
| ---------------------------------------------------- | -------------------------------------------- | ----------------------------------------- | --------- |
| `export` ignores the rootfs mount source             | scratch-container clone + `container export` | honor `rootfs.json`/`rootFsOverride`      | [#15]     |
| `machine create`/`machine run` boot races            | plain `create` + retrying boot probe         | fix `--no-boot` + first-boot settle       | [#15]     |
| `machine run` exec fidelity (stdin, argv, 1st write) | `-i` always; `container exec` for `cm -e`    | attach sequencing; argv-preserving mode   | [#4]      |
| No mounts outside `$HOME` for machines               | distros (`-v`/`--automount`)                 | [apple/container#1805], [#2278]           | [#3]      |
| No port publishing / localhost forwarding            | `cm --forward`; distro `-p` publishes        | publish API / listener-watching forwarder | [#5]      |
| `cp`/`exec` reject stopped containers                | conf snapshots refresh at boot + pre-stop    | a copy path that reads a stopped rootfs   | not filed |

## `container export` can't snapshot a machine (or distro) rootfs

**Status:** worked around and shipped; the upstream bug is still worth
filing ([#15]).

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

Export ignores the configured rootfs mount source. Regular containers
are unaffected; `container distro` ones are -- their rootfs mounts the
journaled disk via `rootFsOverride`, just like a machine's.

The mechanism (verified 2026-10-04): a container's data dir holds only
`runtime-configuration.json` until first start; then the daemon copies
the image's materialized rootfs (`snapshots/<image-digest>/snapshot`, an
ext4 file) to `containers/<id>/rootfs.ext4`. Machines instead carry
`options.rootFsOverride` in `runtime-configuration.json` pointing at
their plugin-state ext4 -- no copy. Two consequences: `export` also
fails on *never-booted* regular containers (no `rootfs.ext4` yet), and
the same override lets `container distro` attach a journaled rootfs
when it materializes a container at `start`. We only patch
`runtime-configuration.json` on containers `container distro` itself
created -- never a machine's backing container.

**Upstream:** no existing issue found (searched 2026-10-03: "export",
"machine export", "rootfs.ext4", "snapshot disk"). Related but not the
same: [apple/container#1400] (export of running containers, closed),
[apple/container#1265] (export should write a tar, closed),
[apple/container#2325] (`export -o` deletes an existing directory,
open -- already worked around in `distro::export`).

### Workaround shipped: scratch regular container

`distro::export_machine` creates a scratch `container create` (never
started), clonefile-copies the machine's `rootfs.ext4` -- found via
`machine_rootfs`, i.e. the `rootfs.json`/`rootFsOverride` source, not a
hard-coded path -- into `containers/<scratch-id>/rootfs.ext4`, runs
`container export` on the scratch container, and deletes it on drop.
That exploits the exact assumption behind the bug: export reads the
conventional path literally, no matter where the container would have
mounted its disk from. The machine is stopped while its disk is cloned
-- a running machine's clone is only crash-consistent otherwise -- and
restarted if it was running. Implementation notes:

- `container create` requires a command/entrypoint even for a container
  that never starts (our older imported images carry no `Cmd`), so the
  scratch container passes an explicit `--entrypoint /bin/sh`.
- A SIGKILLed export can leak the scratch container, so it carries an
  `export-scratch` label naming the machine.
- `distro export` reuses the same trick: the scratch container's
  conventional `rootfs.ext4` is a clonefile of the distro's journaled
  disk.
- `distro migrate` / `migrate --out` use the same clone machinery --
  the better path for moving either direction without a tar round-trip.

**Rejected alternatives:** streaming a guest-side tar (works,
byte-identical, but depends on `sh`/`mount`/`tar` existing in the
image -- the wrong dependency for an export tool); reading the ext4
host-side with `ext4-view` (unnecessary once the scratch trick worked).

**Upstream fix:** once `container export` honors the rootfs mount
source, plain `container export <container-id>` on the machine's
backing container is the whole implementation.

## `machine create` / `machine run` boot races

**Status:** worked around and shipped; upstream bugs still worth filing
([#15]).

Two quirks the import path works around:

- **`machine create --no-boot` is effectively unbootable** -- a
  `machine run` on a never-booted machine races the boot and every
  attempt failed in testing (1.5.0). `cm --import` therefore uses plain
  `machine create`, whose internal boot absorbs the first-boot
  provisioning reboot, rather than `--no-boot`.
- **The first `machine run` after `machine create` can still lose the
  settle race** -- the import boot probe retries with a pause before
  warning (the machine is left created, not deleted).

The first-boot flake itself is *not* an import defect -- a stock
`alpine:latest` machine reproduces it exactly (verified 2026-10-06): the
machine plugin's `/sbin.machine/init` ends with an unconditional
`exec /sbin/init`, alpine's inittab points at openrc which the image
doesn't ship, busybox init reboots, and the second boot succeeds.
Possibly related to [apple/container#2024]; see
container-internals.md.

A separate landmine found while building import: `container create`
refuses an image with no command, and early `oci::build_layout` output
carried only `Labels`. Generated images now write the conventional
defaults (`PATH`, `Cmd /bin/sh`, `WorkingDir /`), so imported images
also work via plain `container run`.

## `machine run` exec fidelity: stdin, argv, first write

**Status:** partially worked around; the first-write drop is still open
upstream ([#4]; argv re-eval is [apple/container#1954]).

`container machine run -- <cmd>` has three fidelity problems, observed
on `container` 1.5.0:

**stdin is closed unless `-i` is passed** -- without it the guest
process sees immediate EOF. Workaround shipped: `cm` always passes
`-i`, so piped stdin works (`echo hi | cm -- cat`).

**argv is not preserved** -- the guest init does `exec "$SHELL" -c
"$*"`, concatenating the argv vector and re-evaluating it as shell code
([apple/container#1954]):

```bash
$ container machine run -n alpine -i -- printf ':%s:' 'one two three'
:one::two::three:          # 'one two three' re-split into 3 args
```

Workaround shipped: `cm -e` execs `container exec` in the machine's
backing container (argv-exact; your provisioned user; the working dir
`machine run` would pick), falling back to `machine run` with each
argument single-quoted when exec isn't possible. Distros always use
`container exec`, so `-e` and `--` are both argv-exact there. Quoting
survives one extra eval round, which is why `sh -c 'a; b; c'` works at
all -- and why remote-extension `bash -c '<single-quoted script>'`
calls mostly land correctly. Fragile but functional.

**The first guest→host write is dropped -- on either stream:**

```bash
$ container machine run -n alpine -i -- sh -c 'echo A; echo B; echo C'

B          # "A" vanished, its newline partially survived
C
```

Observed characteristics (12/12 repro on multi-write `sh -c`, 1.5.0;
*not* reproduced in later 1.5.0 runs on 2026-10-03):

- The **first `write()` across both streams** is lost; everything after
  it arrives intact. A fragment survives as a leading blank line --
  looks like partial first-datagram delivery rather than a clean drop.
- stderr is affected identically to stdout -- the streams attach late
  as a pair, not a TTY-mode stdout bug.
- Once attached, streaming is reliable -- long-running or verbose
  commands are mostly unaffected.
- Not fully deterministic: bare executables sometimes survive (`echo
  hi` 6/6) and sometimes truncate oddly (`printf 'a b c\n'` delivered
  only `a`). Treat *any* early output as unreliable.
- Exit codes and late stderr are unaffected.

Likely cause: `machine run` starts the guest process before the vsock
stdio channels are confirmed attached. If the race is in the CLI's
sequencing an L2 client could sequence it correctly; if it's in
`vminitd`'s process-start ordering, no client-side level escapes it.

No workaround is shipped for the write drop. Considered and parked: an
attach handshake (`sh -c 'IFS= read -r _; exec "$@"' cm-attach <cmd>…`
with `cm` writing the byte once attached) -- only sound if
stdin-reaching-the-guest implies stdout attach, which is unverified; a
fixed delay prefix is flakier still (busybox `sleep` rejects
fractional args). Impact: the remote-extension bootstrap
([remote-wsl-interop]) survives because its result block is echoed
late in a long script, but the quick probes editors sprinkle around it
(`uname`, `command -v …`) are exactly the first-write-fast commands
that lose output.

Related upstream issues (searched 2026-10-01; no exact match for the
write drop -- a new issue is warranted, citing #1148 as adjacent):

| Issue                                                          | Status | Relation                                                       |
| -------------------------------------------------------------- | ------ | -------------------------------------------------------------- |
| [apple/container#1148] -- `run -i` truncates piped stdout      | open   | Closest -- output bytes lost on the interactive/exec path      |
| [apple/container#1954] -- `machine run` shell-evals argv       | open   | The argv problem above; asks for an argv-preserving mode       |
| [apple/container#2299] -- `exec -it` leaves host tty broken    | open   | Same attach/teardown machinery                                 |
| [apple/container#2009] -- log MultiWriter dies on client EPIPE | open   | Same output-fanout plumbing, different symptom                 |
| [apple/container#949] -- `-i`/`-t` didn't pipe stdout          | closed | Historical precedent: this plumbing has shipped broken before  |
| [apple/container#2057] -- completed exec state retained        | open   | Exec lifecycle in container-runtime-linux                      |
| [apple/container#2210] -- exec exit 255 ambiguous              | open   | Exit-code fidelity -- the "codes propagate" claim has a caveat |

## `container cp`/`exec` need a running container

**Status:** worked around for the `wsl.conf` snapshots ([#6]);
upstream issue not yet filed.

`ContainersService.copyIn`/`copyOut` and `exec` reject non-running
containers, so the guest→host config snapshots can only refresh while
a distro is up: right after boot, and just before a host-initiated
stop (`stop`, `set`, `export`, `rename`, `migrate --out`). A distro
stopped via bare `container stop` keeps its last-boot snapshot —
matching WSL's restart-to-apply model anyway.

**Upstream fix:** a copy path that reads a stopped container's rootfs
(the data is on disk; nothing needs the guest up) would make the
snapshots exact instead of last-boot.

## No mounts outside `$HOME` for machines

**Status:** closed for distros; still open for `container machine`
itself (upstream [apple/container#1805], [apple/container#2278];
tracked as [#3]).

`container machine` shares the macOS home directory into the guest via
virtiofs **at the same path** (`/Users/<name>` → `/Users/<name>`), mode
`rw`/`ro`/`none` via `machine create --home-mount` or `machine set`.
Nothing outside `$HOME` is shared and -- as of `container` 1.5.x -- the
`machine` surface exposes no additional-mount option. Repos on
`/Volumes/...` or outside home are unreachable in-guest; WSL mounts
every drive under `/mnt/<letter>`.

Shipped: the "L0 approximation" -- `cm`/`container distro` compose
`container create` with `-v SRC:DST[:ro]` and `--automount` directly
(the machine container is just a container whose init is bind-mounted
in; a spike confirmed our own init provisions the user correctly from a
plain `container create`). Distros are tracked by label, not
`machine list`; `cm` merges both. Mounts are virtiofs, so they keep the
host volume's case sensitivity and uid mapping.

Caveats: `--automount` re-resolves `/Volumes` on `set` and whenever a
stopped distro starts -- but a running distro doesn't see volumes
attached after boot, and Apple-private/unreadable volumes
(`com.apple.TimeMachine.localsnapshots`, TCC-protected Time Machine
destinations) are skipped because VZ refuses to share them. WSL's drvfs
handles hot-plug natively; matching that would need a guest-side
mounter.

If machines ever grow mounts upstream, the model sketched for them was:
per-volume mounts at `/mnt/<volume-name>` (the WSL `/mnt/<letter>`
analogue); the honest `C:` equivalent is the **Data volume**
(`/System/Volumes/Data` gets `/Users`, `/opt`, `/usr/local`, `/etc`,
`/private` in one shot -- `/` itself drags in the sealed system
volume); keep `$HOME` at its same-path convention and make extra mounts
*additive* at `/mnt/…`; honor `source`/`destination`/`ro` at minimum --
virtiofs uid/umask semantics mean `wsl.conf`'s drvfs options may not
translate, so document what can't be honored rather than silently
ignoring it (see [configuration-files] for where such options would
live).

## No localhost port forwarding for machines

**Status:** worked around; automatic forwarding still open ([#5]).

WSL2 defaults to `localhostForwarding=true`: a guest listener on
`localhost:3000` is reachable at `localhost:3000` on Windows, managed
by a service watching guest listeners dynamically. A container machine
instead gets its own IP on the `machine` network -- guest services are
reachable *at that IP only*, nothing bridges to macOS localhost, and
`container machine` has no publish API.

Shipped, two complementary paths:

- **`cm --forward HOST[:GUEST]`** -- a foreground userspace forwarder
  (`TcpListener` on `127.0.0.1:<p>` → `TcpStream` to `<machine-ip>:<p>`;
  ~100 lines of std-only Rust). Static mapping only, no listener
  discovery. **Experimental** -- the only option for machines.
- **Distro `-p`/`--publish`** -- real runtime port publishing via
  `container create`/`set --publish` (host IP defaults to `127.0.0.1`).
  The supported path for distros; no userspace proxy.

Open: WSL-style automatic forwarding (watch guest listeners, map them
dynamically). The plausible level is L2.5 -- a `core`/auxiliary plugin
running a host-side socket forwarder (`container` ships a
`SocketForwarder` library product to copy from; a `network` plugin
controls IPAM, not forwarding). L2 gives the same forwarder without the
plugin wrapper; L3/L4 owns the network entirely. When listener-watching
lands, `--forward` should be deprecated outright; dynamic WSL-parity
forwarding is probably not worth it until requested.

An sshd-in-guest `ssh -L` works today (the agent is forwarded in) but
depends on image contents -- not a mechanism to build on.

Beyond forwarding there's the broader mode question: WSL's
`networkingMode` (NAT/mirrored/bridged) has no knob here at all --
the guest always gets a dedicated IP. What `container`/VZ could
expose, and what a `network` plugin or lower rung would need, is
investigated in [#43].

[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#6]: https://github.com/daphnediane/container-distro/issues/6
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#43]: https://github.com/daphnediane/container-distro/issues/43
[#2278]: https://github.com/apple/container/issues/2278
[apple/container#1148]: https://github.com/apple/container/issues/1148
[apple/container#1265]: https://github.com/apple/container/issues/1265
[apple/container#1400]: https://github.com/apple/container/issues/1400
[apple/container#1805]: https://github.com/apple/container/issues/1805
[apple/container#1954]: https://github.com/apple/container/issues/1954
[apple/container#2009]: https://github.com/apple/container/issues/2009
[apple/container#2024]: https://github.com/apple/container/issues/2024
[apple/container#2057]: https://github.com/apple/container/issues/2057
[apple/container#2210]: https://github.com/apple/container/issues/2210
[apple/container#2278]: https://github.com/apple/container/issues/2278
[apple/container#2299]: https://github.com/apple/container/issues/2299
[apple/container#2325]: https://github.com/apple/container/issues/2325
[apple/container#949]: https://github.com/apple/container/issues/949
[configuration-files]: notes/configuration-files.md
[lower-level-integration]: lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[remote-wsl-interop]: remote-wsl-interop.md
[security]: security.md
