# Host access to guest filesystems (`distrofs`)

<!-- cspell:ignore srwxr unsbx UPBK2H6LZM -->

How macOS gets a `\\wsl$`-style view *into* a distro's filesystem:
a small file server injected into the guest, reached over a
`--publish-socket` relay, and surfaced in Finder by an FSKit module.
Also covers distro-to-distro mounts (WSL's `/mnt/wsl`) as a future
extension.

Written against `container` 1.5.0 (containerization 0.47.0) on
macOS 27 / Xcode 27. Facts marked *verified* were checked by reading
upstream source or by experiment; everything else is design intent.

**Tracking:** [#27].

## TL;DR

- **Guest:** `distrofs`, a static `aarch64-linux-musl` Rust binary,
  ships in each distro's read-only `/sbin.distro` assets. It is started
  on demand by `container exec -d` and exits when idle. It speaks a
  small custom protocol over a Unix socket at
  `/var/lib/container-distro/distrofs.sock`.
- **Transport:** every distro with the feature on is created with
  `--publish-socket <host>:<guest>`. `container`'s runtime relays each
  host connection over vsock to the guest socket — no guest network
  needed, so `--network none` distros work (*verified*).
- **Trust:** the host socket lives in the Darwin per-user temp dir
  (outside `$HOME`, so no guest can plant anything there), and every
  connection is authenticated *after* `connect`: peer audit token must
  be Apple's runtime helper, and a mutual HMAC handshake proves the
  peer is the server we just launched in *that* distro.
- **Host front-end:** a signed `Distro Files.app` carrying an FSKit
  module (`distrofs://<name>` generic-URL resource) plus an unsandboxed
  launch agent that does the `container` work and passes the
  authenticated socket fd to the sandboxed module over XPC. macOS 27's
  `FSVolume.DataCacheHandler` lets the server's inotify events
  invalidate kernel caches.
- **All validation that protects the host** — names, symlink targets,
  sizes, errno mapping — lives in one Rust client library shared by the
  CLI and (via a C ABI) the FSKit module.

## How WSL does it

`\\wsl$\<distro>` (`\\wsl.localhost\<distro>`) is served by a 9P
server inside WSL's own init, reached by Windows' built-in 9P
redirector over Hyper-V sockets. Explorer, editors and `Copy-Item`
see the distro's whole root, live. The distro is started on first
access. There is no network exposure: the transport is the VM socket.

Our shape is the same — guest-side server, VM-socket transport, native
OS client — with two differences: macOS has no 9P client (so the
"native client" is an FSKit module we write), and each distro is its
own VM rather than a namespace in one shared VM (which matters for
`/mnt/wsl`, below).

## What upstream gives us

| Primitive                        | What it does                                                                             | Usable for us?                                                              |
| -------------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `container copy` / `cp`          | One-shot file/tar transfer: `vminitd` reads/writes under `rootfs` over a dedicated vsock | Fine for a `cp` command; no listing, no partial reads -- can't back a mount |
| `--publish-socket host:guest`    | Guest listens, host connects; per-connection vsock relay into `rootfs/<guest>`           | **Yes -- the transport.**                                                   |
| `--volume <socket>:<guest>`      | A socket-typed volume source relayed *into* the guest                                    | Future guest->host broker for distro-to-distro mounts                       |
| `ContainerClient.dial(id:port:)` | Host dials a guest vsock port; XPC-only, no CLI                                          | No -- private API                                                           |

The mechanics: `cp` has the runtime helper ask `vminitd` (guest
PID 1) to read/write under `/run/container/<id>/rootfs`, resolved
with `openat2(RESOLVE_IN_ROOT|RESOLVE_NO_MAGICLINKS)` -- running
containers only, guest root, single files land `0644`. For
`--publish-socket` the helper binds `host` and `vminitd` connects
to `/run/container/<id>/rootfs/<guest>` for each host connection;
the host socket is created at start and removed at stop. The
socket-volume path is `RuntimeService.swift`'s `mount.isSocket()`
branch -> `UnixSocketConfiguration(direction: .into)`.

*Verified by experiment* (`debian:latest`, `--network none`):

- the host socket appears at create/start (`srwxr-xr-x`, owned by the
  user) and disappears at stop;
- a guest listener on `/var/lib/…/fs.sock` (on the rootfs) was
  reachable from the host with `nc -U`, peer uid 0 in the guest;
- no guest network interface was needed.

Not yet verified: whether a socket on a filesystem mounted *inside*
the container's mount namespace (e.g. systemd's tmpfs on `/run`) is
visible to `vminitd`, which resolves the path at VM level. The guest
socket therefore lives on the rootfs itself (`/var/lib/...`), never
under `/run` or `/tmp`.

## Architecture

```text
 macOS                                                  Linux guest (distro)
┌──────────────────────────────────────────────┐       ┌──────────────────────────────┐
│ Finder / editors / shell                     │       │                              │
│        │ VFS                                 │       │ /sbin.distro/distrofs serve  │
│ ┌──────▼───────────────┐  XPC (fd)  ┌───────┐│       │   (on demand, root,          │
│ │ DistroFS.appex       │◄──────────►│ agent ││       │    idle-exit)                │
│ │ FSKit module,        │            │(unsbx)││       │        ▲                     │
│ │ sandboxed            │            └──┬────┘│       │        │ AF_UNIX             │
│ │  └ libdistrofs_client│               │     │       │ /var/lib/container-distro/   │
│ └──────────────────────┘     container-distro│       │   distrofs.sock (0700 dir)   │
│                               fs connect NAME│       │        ▲                     │
│                                  │  exec -d  │       │        │ vminitd connects    │
│                                  ▼           │ vsock │        │                     │
│  /var/folders/…/T/container-distro/s/<h>.sock├──────►│ vminitd relay                │
│        (bound by container-runtime-linux)    │       │                              │
└──────────────────────────────────────────────┘       └──────────────────────────────┘
```

1. Finder touches a mounted `distrofs://NAME` volume (or the user runs
   `cm --mount NAME`).
2. The module asks the agent for a connection to `NAME`.
3. The agent runs `container-distro fs connect NAME`, which starts the
   distro if needed, launches `distrofs serve` with a fresh session
   token, connects to the published socket with the checks below,
   completes the handshake, and hands back the connected fd.
4. The agent passes the fd to the module over XPC. From then on the
   module talks to the guest server directly through the relay.

## Guest server: `distrofs`

**Build.** `distrofs` is a standalone project
([daphnediane/distrofs](https://github.com/daphnediane/distrofs)) —
one crate producing a static `aarch64-unknown-linux-musl` guest server
*and* the macOS host client/binary from a shared protocol module
(prebuilt musl binaries for both arches ship on its GitHub releases;
`distrofs fetch-servers` downloads them verified). Rust's
self-contained musl target links with `rust-lld`, so no cross C
toolchain or Docker is needed as long as the dependency tree stays
pure Rust (no `ring`, no `openssl`). The Linux kernel in every
`container` VM is arm64; amd64 distros run their userspace under
Rosetta, and the arm64 static binary executes natively in them
(*verified*: `--arch amd64` alpine, where `uname -m` reports
`x86_64`). The release build is ~0.5 MB.

container-distro deliberately knows nothing of the wire protocol: it
supplies the socket path (its own `<temp>/container-distro/s/`
layout), the launch (`container exec`), a fresh session token, and the
mapped subcommand arguments. The `distrofs` client does everything
from there — socket-directory and leaf hygiene, the runtime-helper
peer check, and the mutual handshake. Before container-distro execs
it, the client binary itself is verified: a Developer ID-signed copy
must satisfy `identifier "io.github.daphnediane.distrofs" and anchor
apple generic`; an unsigned copy (cargo/dev build) must sit on a path
owned by us or root and writable by no untrusted account.

**Delivery.** Distros mount their own init assets read-only at
`/sbin.distro` from the journal (`distros/<name>/sbin.distro/`,
`uchg`-locked; see security.md T6). `distrofs` joins `init` and
`create-user.sh` there. It is found at run time:

1. under `--dev`, the sibling worktree's
   `../distrofs/target/aarch64-unknown-linux-musl/{release,debug}/distrofs`;
2. relative to the canonicalized running binary's prefix:
   `<prefix>/libexec/container-distro/distrofs-server-aarch64` — the
   `scripts/build-distrofs.sh --install` layout beside `cargo
   install`, the `.pkg`, and the plugin copy's own
   `distro/libexec/container-distro/` — with plain `distrofs` accepted
   for earlier installs;
3. the distrofs-managed `<prefix>/libexec/distrofs/distrofs-server-aarch64`
   that `distrofs fetch-servers` and its `.pkg` populate. No fixed
   fallback path: a binary outside any install prefix
   (`target/debug/cm`) finds nothing unless `--dev`.

A found file must carry the `distrofs-version:` stamp embedded in every
distrofs build — a stale or misplaced file is rejected up front rather
than surfacing as a failed `container exec`.

`write_assets` copies it in with the same unlock → write
`O_NOFOLLOW` → re-lock dance as the scripts — at materialize, and
again (under the distro lock) before every `fs` session, so a running
distro gets a new build through the live read-only share without a
restart. When no build is found, `fs` says so before touching the
distro, and materialize is unaffected. Machines have no assets mount
— see Phase 5.

**Lifetime: on demand.** Nothing listens in the guest until a host
client asks:

- `container exec -d -u 0 -e DISTROFS_TOKEN <id> /sbin.distro/distrofs serve …`
  — the bare `-e NAME` form inherits the value from our environment, so
  the token never appears on a command line.
- The server creates `/var/lib/container-distro/` mode `0700` root
  (guest users must not reach a root server's socket), unlinks any
  stale socket, binds, and listens.
- **Last starter wins.** A new instance unlinks and re-binds the path.
  The relay connects by path per connection, so new connections reach
  the new instance while an older one keeps serving the clients it
  already has and exits when they're gone. A client that loses the race
  fails the handshake (wrong token) and retries.
- Idle exit: no client within 30 s of start, or 30 s after the last
  client disconnects.

**Server behavior.** The server runs as guest root and reports real
ownership and modes; the *host kernel* enforces them against the Mac
user, whose uid equals the provisioned guest user's uid
(`CONTAINER_UID` passthrough) — so the user's own files are
read-write, world-readable files are readable, and `/etc/shadow`
isn't. It reads `/proc/self/mountinfo` and serves only local
filesystem types, so virtiofs shares and `/proc` appear as empty
read-only directories rather than looping host content back or
leaking pseudo filesystems into Finder. Devices/FIFOs/sockets list
with their real type but can't be opened. Details (and a "root view"
option idea) live in distrofs's
[protocol doc](https://github.com/daphnediane/distrofs/blob/main/docs/design/protocol.md).

## Transport and connection hygiene

**Paths.**

- Guest: `/var/lib/container-distro/distrofs.sock` — on the rootfs,
  not under `/run`/`/tmp` (see "not yet verified" above).
- Host: `<_CS_DARWIN_USER_TEMP_DIR>/container-distro/s/<h>.sock`, where
  `<h>` is 16 hex digits of SHA-256 over the runtime container id.
  `sun_path` is limited to 104 bytes; the path is computed at
  materialize and refused up front if it's too long, rather than
  letting `container create` fail opaquely.

`create_args` adds `--publish-socket <host>:<guest>` when the spec's
`files` setting resolves on. Distros are re-materialized from the
journal at every start, so the relay appears on the next start of any
existing distro — no `set` required.

**Why the Darwin temp dir.** `~/Library/Application Support` is inside
the home share, so an rw-home guest could replace the socket with a
symlink to some other socket (T6's confused-deputy shape). `uchg` can't
help, because the runtime helper must create entries in the directory.
`/var/folders/…/T/` is per-user, `0700`, and outside `$HOME`, so no
virtiofs share reaches it.

**Connecting.** The `distrofs` client owns this whole path —
`connect(2)` necessarily resolves a path and follows symlinks, so it
defends in three layers (private-directory chain check, socket-leaf
`fstatat`, and a post-connect audit-token check against the
`--peer-requirement` we pass, pinning Apple's
`container-runtime-linux` helper), then runs a mutual HMAC-SHA256
handshake proving both ends hold this session's token. Only "no
server yet" errors retry; trust failures don't. Details:
[distrofs protocol doc](https://github.com/daphnediane/distrofs/blob/main/docs/design/protocol.md).

## Protocol

The wire protocol, handshake, mount-filtering policy, and client-side
validation all live in the distrofs repo — see its
[protocol design doc](https://github.com/daphnediane/distrofs/blob/main/docs/design/protocol.md).
container-distro deliberately knows nothing of it. The properties the
rest of this document relies on:

- FUSE-shaped (single-component `Lookup`, node/handle ids, pipelined
  requests, errno mapping) so an FSKit module can sit on top directly;
- mutual-token handshake — a guest-local process that reaches the
  socket can't get a root file server;
- the client is the trusted side: it validates names, attributes, and
  symlink targets (chroot-style rewriting) before the host sees them.
  The FSKit module will link that code through a C ABI, so the CLI
  and Finder get identical enforcement.

## Host front-end: FSKit

`Distro Files.app` (bundle id
`io.github.daphnediane.container-distro.files`), installed in
`/Applications` by the `.pkg`. It contains:

- **`DistroFS.appex`** — an FSKit module (`FSUnaryFileSystem`), short
  name `distrofs`, declaring `FSSupportedSchemes = [distrofs]`. Its
  `loadResource` takes an `FSGenericURLResource` (`distrofs://NAME`,
  macOS 26+) and produces one volume. It is sandboxed (required) and
  holds the restricted `com.apple.developer.fskit.fsmodule` entitlement.
  It speaks the protocol through `libdistrofs_client.a`.
- **`distrofs-agent`** — a launch agent registered with
  `SMAppService.agent`, unsandboxed, vending an XPC Mach service whose
  name is prefixed with the app group
  (`<TEAMID>.io.github.daphnediane.container-distro`) so the sandboxed
  module may look it up. Its one call, `connect(name) → FileHandle`,
  runs `container-distro fs connect NAME --pass-fd`, which does all the
  `container` work and the connection checks above and returns the
  authenticated socket over `SCM_RIGHTS`; the agent forwards the fd over
  XPC. A sandboxed module can neither run `container` nor connect to a
  socket outside its container, so the agent is required regardless;
  passing an fd keeps it out of the data path.
- **The app itself** — mostly a host for the other two: registers the
  agent, explains the one-time "enable the File System Extension" step
  (System Settings → General → Login Items & Extensions), and later
  lists distros with mount/unmount buttons.

**Mounting.** `cm --mount NAME [DIR]` and
`container distro fs mount NAME [DIR]` run
`mount -F -t distrofs distrofs://NAME DIR` (default
`~/Distros/NAME`); `--unmount` runs `umount`. Volumes mounted by the
user on a directory they own need no root.

**Caching.** On macOS 27 the volume adopts `FSVolume.DataCacheHandler`:
it grants `readCache` (and `writeThrough` for files opened for writing)
in `open(_:modes:cacheMode:…)`, and on `Event::Invalidate` calls
`setCacheState(for:…, action: .invalidate)` or `.revoke` for `Gone`.
On macOS 26 the volume falls back to conservative attribute TTLs.

**Signing and provisioning.** FSKit's module entitlement is restricted:
Developer ID distribution needs App IDs with the FSKit capability for
the app and the appex, an app group, and Developer ID provisioning
profiles embedded in both bundles. The existing
`scripts/build-pkg-signed.sh` / notarization flow extends to sign the
app inside-out (appex, agent, app) before it goes into the `.pkg`
payload. One-time portal setup is a maintainer task, documented in
packaging.md when Phase 2 lands. The project is built with `xcodebuild`
from a checked-in project under `macos/DistroFiles/`; the distrofs
client library is built by a run-script phase (`cargo build --lib
--target aarch64-apple-darwin` in the distrofs checkout).

**Minimum OS.** macOS 26 for generic-URL resources; the cache handler
is used when running on 27+.

## CLI surface

Phase 1 (debug and scripting, no FSKit needed):

```text
container distro fs ls    NAME [PATH]          # readdir + attrs
container distro fs stat  NAME PATH
container distro fs get   NAME PATH [DEST|-]   # file or directory
container distro fs put   NAME SRC  PATH
container distro fs check NAME                 # relay + handshake diagnostics
```

Phase 2+: `container distro fs connect NAME --pass-fd` (agent-internal),
`container distro fs mount|unmount NAME [DIR]`, and `cm --mount` /
`--unmount` as WSL-flavored front doors.

## Spec and journal

A new resolved `files: bool` on `DistroSpec`, set through `Intent`
like `ssh` and pinned by `--files`/`--no-files` on `create`/`import`/
`set`. Defaults:

- on for the default and `--trusted` presets — the relay is inert
  until a host client launches the server, and a guest cannot initiate
  anything through it;
- **off under `--restricted`**, which keeps its "every hole needs its
  own flag" rule. The host risk there isn't guest reach but Finder,
  Quick Look and Spotlight parsing an untrusted rootfs's contents once
  it's mounted.

Old journals without the field resolve through the same chain, so
upgrading turns the relay on at the next start of a non-restricted
distro. `set` reports changes to it; showing it in `list -v`/`info` is Phase 4.

## Security

Recorded as **T13** in security.md (Phase 1); the substance:

- **New host surface:** a per-distro socket bound by Apple's runtime
  helper in a `0700` per-user directory outside `$HOME`. Same-user host
  processes can connect to it, but they still have to pass the
  handshake, and anything running as the user can already do everything
  `cm` can.
- **Guest → host:** none added. The relay is host-initiated. The
  server's replies are untrusted input to the `distrofs` client
  (validation in its protocol doc), and the mount is `nosuid,nodev`.
- **Guest-internal:** a root server inside the guest. Mitigated by the
  `0700` root socket directory, client-side token proof, on-demand
  lifetime, and idle exit. A guest user who can become root can already
  read everything the server can.
- **Restricted distros:** the feature is off by default; enabling it is
  an explicit flag, documented next to `--restricted`'s claims.
- **Spotlight and Quick Look** will index and render guest files.
  Volumes are mounted with `nobrowse` until a deliberate decision is
  made otherwise, and the app explains the trade-off.
- **Token handling:** never on a command line (`-e NAME` inheritance),
  never logged, compared in constant time.

## Phasing

| Phase        | Scope                                                                                                             | Exit criteria                                                                      |
| ------------ | ----------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| 0            | this document                                                                                                     | committed                                                                          |
| 1            | shared `proto`, guest `distrofs`, host client; `files` spec + `--publish-socket`; asset delivery; `fs` verbs; T13 | get/put round-trip on a running distro (incl. `--network none`, amd64); unit tests |
| 2            | FSKit spike, then app + appex + agent + C ABI; signing                                                            | Finder browses and edits a distro; spike answers the open questions below          |
| 3            | inotify events + `DataCacheHandler`                                                                               | host sees guest-side edits without remount                                         |
| 4            | `.pkg` payload, `cm --mount`, docs                                                                                | release-gate passes with the app in the payload                                    |
| 5 (optional) | machines and not-yet-restarted distros via `container exec -i ... distrofs serve --stdio`                         | `fs ls` works on a machine (binary pushed with `container copy`)                   |

## Future extension: distro-to-distro mounts (`/mnt/wsl`)

**What WSL does.** All WSL 2 distros share one VM, and `/mnt/wsl` is a
tmpfs shared across them. Tools bind-mount things there to share them
between distros (Docker Desktop's `/mnt/wsl/docker-desktop-bind-mounts`),
and `wsl --mount` disks appear under it. Because it's one kernel, a
mount made in one distro is visible in the others.

**Why ours differs.** Each distro is its own VM with its own kernel, so
nothing can be shared at the mount level. The guest kernel has
`CONFIG_FUSE_FS=y` but `CONFIG_NET_9P` off (*verified*,
`containerization/kernel/config-arm64`), so a cross-VM mount has to be
FUSE in userspace. Options, cheapest first:

1. **Shared host-backed directory.** A host directory
   (`state_dir()/mnt-wsl/`, `uchg`-free by necessity) virtiofs-mounted
   read-write at `/mnt/wsl` in every distro that opts in. This shares
   *files*, not mounts — enough for the common "drop a socket or a
   config here" uses, though Unix sockets don't cross VMs through
   virtiofs. It's just another `--volume`. It is a cross-distro write
   channel, so it's opt-in per distro and never on for `--restricted`.
2. **Peer root views over distrofs.** `/mnt/wsl/<other>` in distro A
   is a FUSE mount whose backend is distro B's `distrofs`:
   - A's side: `distrofs mount` (same binary) as a FUSE client — guest
     root has `CAP_SYS_ADMIN`, so it can `mount(2)` `/dev/fuse` itself
     without `fusermount`.
   - Transport A→host: a socket-typed `--volume
     <broker.sock>:/var/lib/container-distro/peers.sock` (the verified
     `.into` relay). The socket must exist when A is created, so the
     broker is the always-running agent (or a placeholder it rebinds).
   - Host broker: authorizes A→B by policy, starts B's server, connects
     to B's published socket with the same hygiene, and splices the two
     — or, better, hands A's stream to B's server after a broker-run
     handshake so the token never enters A.
   - Policy: explicit per-pair or global opt-in; restricted distros can
     neither see nor be seen; the broker logs every peer session.
   - Symlink rewriting and name validation move into the guest-side
     FUSE client for this path, since A's kernel resolves the links.
3. **Shared block devices** (the analogue of `wsl --mount`): attaching
   one ext4 image to two VMs read-write corrupts it; read-only sharing
   is possible but niche. Not planned.

Option 1 is a small, independent feature; option 2 reuses Phases 1–3
almost entirely. Both are post-FSKit.

## Alternatives considered

| Alternative                                          | Why not (as the primary design)                                                                                                                         |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Wrap `container copy`                                | Running-only, root-only, one-shot; no listing or random access. Still a fine basis for a future `cm --cp`                                               |
| Read `rootfs.ext4` offline (pure-Rust ext4 reader)   | Read-only, misses never-booted distros, reads upstream internals; maybe for stopped distros later                                                       |
| Loopback NFSv3 translator + `mount_nfs`              | Quick and pure Rust, but AUTH_SYS on loopback TCP lets any local user mount it; superseded by FSKit (pre-26 macOS is unsupported anyway)                |
| SMB / WebDAV / SFTP servers on the guest's vmnet IP  | Reachable by every guest on the subnet; absent under `--network none`; heavy per-distro packages                                                        |
| File Provider extension (the Dropbox/OneDrive model) | Sync-and-materialize under `~/Library/CloudStorage`; weak POSIX semantics -- fine for documents, wrong for a live Linux root                            |
| macFUSE / FUSE-T + sshfs                             | Third-party install (for macFUSE, a kext or its own FSKit backend); FSKit directly is the first-party path                                              |
| 9P2000.L or SFTP as the wire protocol                | No change notifications; no guest-side reuse (the guest kernel lacks 9P); SFTP's POSIX coverage is thin. A custom protocol is small -- we own both ends |
| `ContainerClient.dial` vsock                         | Internal XPC API, no CLI; would need Swift and track upstream internals                                                                                 |
| Server started at boot by `init`                     | A root daemon in every distro all the time, plus systemd/openrc interplay; on demand costs ~100 ms exec per session                                     |

## Open questions (Phase 2 spike)

- `mount -F -t distrofs distrofs://NAME DIR` with an
  `FSGenericURLResource`: confirm the exact `mount(8)` invocation and
  which other `Info.plist` keys (beyond `FSSupportedSchemes`) the
  module must declare.
- Sandboxed module + app-group-prefixed Mach service + fd over XPC:
  confirm no extra temporary-exception entitlement is needed.
- A socket on a container-namespace tmpfs vs. the rootfs (see
  "not yet verified").

[#27]: https://github.com/daphnediane/container-distro/issues/27
