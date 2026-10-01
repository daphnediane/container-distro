# Lower-level integration options

Research notes on whether `cm` can bypass the `container` CLI, how far down
the stack is reachable, what Rust↔Swift interop looks like, and whether a
Swift rewrite makes sense. Written against `container` main / 1.5.x and
`containerization` 0.47.x — details will drift; both are pre-1.0-ish in API
stability terms (see [Risks](#risks)).

## TL;DR

- Yes, you can go below `container machine`. There are roughly four rungs
  between "shell out to the CLI" and "talk to Virtualization.framework
  yourself", and only two of them are attractive.
- The sweet spot, if we ever need it: a **small Swift helper library** that
  uses `apple/container`'s exported `MachineAPIClient`/`ContainerAPIClient`
  SwiftPM products, exposed to Rust over a C ABI. That removes the
  subprocess layer without reimplementing anything or needing new
  entitlements.
- Going **fully independent** via `apple/containerization` directly (or
  `objc2-virtualization` from pure Rust) is possible but means owning
  kernel boot, the guest-agent protocol, OCI unpacking, networking, and
  Apple entitlements/signing. Only worth it for features the installed
  `container` services fundamentally can't express.
- Rust↔Swift interop is real but entirely through a C ABI boundary
  (`@_cdecl`, or `swift-bridge` codegen). There is no first-class
  Swift↔Rust path in the toolchain.
- A full Swift rewrite is defensible but not compelling today: `cm`'s job
  is mostly argument mapping, which the Rust/clap implementation already
  does well. The interesting work happens in layers that require Swift
  *or* a C shim regardless of the CLI's implementation language.

## How the stack is actually layered

```
cm (Rust, this repo)
└── container CLI  ── Swift, Sources/CLI + Sources/ContainerCommands
    └── client libraries (SwiftPM products of the `container` package)
        ├── MachineAPIClient  → XPC → com.apple.container.core.machine-apiserver
        │                          (a *plugin*: Sources/Plugins/MachineAPIServer)
        └── ContainerAPIClient → XPC → container-apiserver (launch agent)
                                     ├── container-core-images     (XPC helper)
                                     ├── container-network-vmnet   (XPC helper, vmnet)
                                     └── container-runtime-linux   (one per container)
                                          └── vsock + gRPC → vminitd (guest agent)
                                               └── Virtualization.framework / vmnet
```

Key facts, with sources:

- `container` itself is "written in Swift ... uses the
  [Containerization](https://github.com/apple/containerization) Swift package
  for low-level container, image, and process management"
  ([container README](https://github.com/apple/container)).
- The CLI talks XPC to `container-apiserver`, a launch agent started by
  `container system start`, which in turn spawns XPC helpers for images,
  networking, and per-container runtimes
  ([technical-overview.md](https://github.com/apple/container/blob/main/docs/technical-overview.md)).
- **`container machine` is a plugin, not core.** Commands live in
  `Sources/ContainerCommands/Machine/` and call `MachineClient`
  (`Sources/Services/MachineAPIService/Client/MachineClient.swift`), which
  speaks XPC to `com.apple.container.core.machine-apiserver` — a
  `loadAtBoot` plugin with its own guest init resource
  (`Sources/Plugins/MachineAPIServer/`).
- **`machine run` is just `createProcess` on a specially-configured
  container.** `MachineRun.swift` boots the machine via the machine plugin,
  then uses the *regular* `ContainerClient.createProcess` to exec
  `/sbin.machine/init -s` inside it. A "machine" is a container with a
  custom init, UID/GID-mapped user, home-dir virtiofs mount, SSH agent
  forwarding, all caps, and no masked paths
  ([container-machine.md](https://github.com/apple/container/blob/main/docs/container-machine.md)).
- `apple/container`'s `Package.swift` exports **library products** —
  `MachineAPIClient`, `MachineAPIService`, `ContainerAPIClient`,
  `ContainerXPC`, `ContainerPersistence`, `ContainerPlugin`, ~20 more. It
  pins `containerization` to an *exact* version (0.47.0 at time of
  writing).
- `apple/containerization` is the standalone low-level package: OCI image
  management and registry clients, ext4 filesystem creation, netlink,
  `LinuxContainer`/`LinuxProcess`, a `VirtualMachineManager` abstraction
  with `VZVirtualMachineManager` (Virtualization.framework, macOS) and
  `CHVirtualMachineManager` (cloud-hypervisor/KVM, Linux) backends, and
  `vminitd` — a guest init exposing a gRPC API over vsock for I/O,
  signals, and events. Requires macOS 26 + Xcode 26 to build
  ([containerization README](https://github.com/apple/containerization)).

## Rung by rung: how low can `cm` go?

| Level        | Interface                                                 | Language needed                                      | Still needs `container` installed? | API stability                                                                |
| ------------ | --------------------------------------------------------- | ---------------------------------------------------- | ---------------------------------- | ---------------------------------------------------------------------------- |
| L0 (current) | `container` CLI subprocess                                | Rust only                                            | yes                                | CLI backward compat within major versions                                    |
| L1           | XPC to apiserver / machine plugin                         | any (XPC is a C API)                                 | yes                                | only `container-apiserver` XPC documented stable; plugin routes are internal |
| L2           | `MachineAPIClient` / `ContainerAPIClient` Swift libraries | Swift (C shim for Rust)                              | yes                                | source-stable-ish, but exact-pin churn                                       |
| L3           | `containerization` package directly                       | Swift (C shim for Rust)                              | **no**                             | pre-1.0; minor-version source stability only                                 |
| L4           | Virtualization.framework / vmnet directly                 | Rust possible (`objc2-virtualization`) or Swift/ObjC | **no**                             | Apple framework — stable, but huge scope                                     |

### L1 — XPC from Rust, no Swift at all

XPC is a plain C API in libSystem (`xpc/xpc.h`), and Rust bindings exist
(`xpc-connection` on crates.io, or bindgen). `cm` could send
`XPCMessage`-shaped dictionaries straight to
`com.apple.container.core.machine-apiserver` and `container-apiserver` —
the routes and keys are all in the open source
(`MachineRoutes.swift`, `MachineKeys.swift`).

**Verdict: possible, fragile.** The README only promises compatibility for
the `container-apiserver` XPC API within a major version; the machine
apiserver is a plugin and the message schema is an implementation detail,
not a documented contract. You'd also have to reverse-engineer the message
encoding (which uses XPC data fields with JSON payloads). This trades a
stable CLI surface for an undocumented wire format — the wrong direction
for a compat shim whose job is being boring and dependable.

### L2 — Swift library products, thin C shim for Rust

`MachineAPIClient` and `ContainerAPIClient` are real exported products of
the `container` package. A small Swift dylib could wrap their async API in
a handful of `@_cdecl` functions (list machines, boot, exec with fd
plumbing), and Rust links/calls it. All the type-safe, async, structured
API — no JSON scraping, no subprocess, no exec tricks for TTY passthrough
(though exec passthrough is actually a *feature* for interactive shells —
see below).

**Verdict: the best next rung if we ever need one.** Still requires a
`container` install (the clients speak XPC to its services), still tracks
upstream releases, but eliminates subprocess management and gives real
errors. The cost is a mixed-language build (see interop section) and
pulling a dependency that exact-pins `containerization`.

### L3 — `containerization` directly

Depend on `apple/containerization` and drive `LinuxContainer` /
`VZVirtualMachineManager` + `vminitd` ourselves. This is the "own the VM"
option — the `container` daemon, plugins, and CLI all become optional.
It unlocks things the machine plugin doesn't expose:

- **Arbitrary virtiofs shares** — mounts outside `$HOME`
  (`/Volumes/...`, project dirs on other disks), the single biggest
  functional gap vs WSL today.
- **Multiple/virtual networks and bridging options** beyond what
  `container network` exposes — or host-reachable port forwarding done our
  way.
- Custom kernels per machine (API-supported), Rosetta for amd64 images,
  direct control of boot args, disk layout, snapshots.
- No dependency on `container` release cadence or CLI surface.

But it also means **reimplementing everything the machine plugin does for
free**: image pull + rootfs unpack (OCI layer → ext4, via
`ContainerizationOCI`/`EXT4`), user provisioning (UID/GID mapping,
passwordless sudo), `$HOME` virtiofs wiring, `SSH_AUTH_SOCK` forwarding,
the `/sbin.machine/init` entry protocol, DNS, and lifecycle state on disk.
Plus the signing problem: any binary using Virtualization.framework needs
`com.apple.security.virtualization`; vmnet networking needs
`com.apple.vm.networking`, a *restricted* entitlement that needs Apple's
approval for Developer-ID distribution. Apple's installer already signs
all that away — we wouldn't be anymore.

**Verdict: powerful, heavy.** This is "build our own Lima," not "wrap
`wsl.exe` semantics." Only justified if mounts-outside-`$HOME` (or similar)
becomes a hard requirement *and* upstream won't add it.

### L4 — Virtualization.framework/vmnet raw

Pure Rust *is* technically possible here — `objc2-virtualization` (0.3.x)
provides generated bindings to `VZVirtualMachine` & friends. You'd boot a
kernel + initrd directly, attach virtio devices, virtiofs shares, and a
`VZNATNetworkDeviceAttachment` (or vmnet). Then you need a guest agent —
reimplement or embed `vminitd`'s vsock gRPC protocol — for exec/signal/IO.

**Verdict: don't.** This reimplements Containerization itself to save a
language boundary. The only argument for it is "no Swift in the build,"
which is a bad trade at this layer.

## Can Rust interop with Swift?

Yes, with caveats. There is no toolchain-level Swift↔Rust bridge (Swift
has C++ interop; Rust is not C++). Everything goes through the **C ABI**:

| Direction    | Mechanism                                                                                    | Tooling                                                                                                                                                     |
| ------------ | -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Swift → Rust | Rust `staticlib`/`cdylib` with `extern "C"`; Swift imports via module map                    | manual, or **UniFFI** (0.32.x — generates Swift bindings, async support), **swift-bridge**, **cargo-swift** (XCFramework packaging)                         |
| Rust → Swift | Swift exposes `@_cdecl` globals (C-representable types only: scalars, pointers, `NSObject`s) | manual + `swiftc`, or **swift-rs** (1.0.8 — automates build/link from Cargo), **swift-bridge** (`extern "Swift"` — codegen, supports async both directions) |

Practical constraints for the Rust→Swift direction we'd need:

- Only C-representable types cross directly. Swift structs/generics/`String`
  don't — pass `UnsafePointer<CChar>`, or opaque `UnsafeRawPointer` handles
  with explicit retain/release exports.
- Swift `async` doesn't cross the ABI. Bridge via completion callbacks,
  blocking wrappers (run the async work on a Swift `Task` + semaphore —
  fine for a CLI), or a handle-based poll model. `swift-bridge` automates
  this; hand-rolled `@_cdecl` means writing it yourself.
- `throws` becomes an out-param error or a result struct.
- A SwiftPM package can't directly emit a C-ABI library product; you
  compile Swift to a dylib/framework (or object files) in a build script —
  `swift-rs` exists precisely for this pattern.

So: `cm` could stay Rust and host a Swift shim — this is the L2 design.
Conversely, if we ever wrote the tool in Swift, the *existing* Rust arg
parser could be embedded as a staticlib via UniFFI/`extern "C"` — but
that's pointless, since a Swift rewrite would use swift-argument-parser
anyway.

## Would rewriting in Swift be a good option?

**As a means to reach L2/L3: yes, it's the honest way.** If the project
commits to talking to `MachineAPIClient` or `containerization`, Swift is
the native language of those APIs and a Swift `cm` drops the whole FFI
shim problem. `swift-argument-parser` is roughly clap-equivalent for our
option surface.

**As a goal in itself: no.** Arguments against a rewrite on the current
feature set:

- The hard 20% (TTy passthrough, exit codes, process semantics) is already
  solved in Rust via `exec()` passthrough — arguably *better* than what a
  library call gives, since exec preserves signal/window-size handling for
  free.
- A rewrite is only worth it if it buys the L3 capabilities (arbitrary
  mounts, our own networking). At L2 you're still a client of the same
  services — so "rewrite in Swift" and "call the CLI" differ only in IPC
  mechanism, not capability.
- API churn: `containerization` is 0.x with source stability only within
  minor versions, and `container` exact-pins it. Living inside that
  dependency means tracking it closely — fine for SwiftPM, painful across
  an FFI boundary.
- Build/distribution gets heavier: Xcode toolchain requirement, code
  signing with entitlements (at L3), vs. today's `cargo install`.

**Middle path worth keeping in mind:** `cm` stays Rust; add an *optional*
`cm-swift-bridge` dylib, loaded when present, that upgrades L0 → L2 for
operations where subprocess handling is annoying (streaming `list`, detach
semantics, richer errors), while interactive shells keep using `exec`'d
`container machine run`. Incremental, keeps the boring path boring.

## What lower-level access would actually fix

Mapping current `cm`/machine gaps to the rung that solves them:

| Gap today                                             | Fixed at                                                                              |
| ----------------------------------------------------- | ------------------------------------------------------------------------------------- |
| No mounts outside `$HOME` (`/Volumes/...`)            | L3 (arbitrary virtiofs); L1/L2 can't — plugin doesn't expose it                       |
| No localhost port forwarding (machine has its own IP) | L3/L4 (own NAT/forwarder); possibly L2 via `SocketForwarder`/`NetworkClient` products |
| No `--export`/`--import`, snapshots                   | L3; or wait for upstream                                                              |
| `-v` ignored, no verbose list                         | L1/L2/L3 (structured data direct)                                                     |
| Subprocess overhead + JSON scraping                   | L1/L2                                                                                 |
| Startup `system start` dance                          | unchanged at L1/L2 — services still required                                          |

Most of the list is L3-only. That's the real decision: the benefits of
going low-level concentrate at the level that requires owning the VM.

## Risks

- **Stability contracts.** `container-apiserver` XPC: stable within a
  major version. Everything below (plugin routes, `MachineClient`
  internals, `containerization` source): explicitly not guaranteed.
  Pinning `container` CLI (L0) is the most stable contract available;
  L2/L3 swap stability for capability.
- **Entitlements/signing.** L3/L4 binaries need
  `com.apple.security.virtualization`; vmnet needs the restricted
  `com.apple.vm.networking`. Local dev signing may suffice for the former
  (verify); the latter is the friction point for distribution.
- **macOS/Xcode floor.** `containerization` builds on macOS 26 + Xcode 26
  only. `container` is supported on macOS 26 (runs with limits on 15). Any
  lower-level path hard-requires the newest toolchain — today `cm` only
  needs cargo.
- **Duplicated semantics.** Below L2 we reimplement user provisioning,
  home-mount wiring, ssh-agent forwarding, and the init protocol — code
  that lives in `MachineAPIServer` and its bundled `init`, and will drift
  upstream.

## Open questions

- Does the `MachineAPIServer` *plugin* XPC service fall under the
  "container-apiserver XPC API" compatibility promise, or the "non-public
  XPC helpers" carve-out? (Determines whether L1 is even semi-safe.)
- Is `com.apple.vm.networking` obtainable for an open-source tool without
  a paid-program entitlement request? Check current Apple docs/forums.
- Can `vminitd`'s vsock gRPC be spoken directly from a Rust-created VM
  (the proto files are in `containerization/vminitd`)? If yes, an L4-Lite
  exists: VZ boot + existing vminitd initramfs, no Swift anywhere — but
  still the L4 workload.
- Would upstream accept `container machine create` flags for extra
  virtiofs mounts? Cheapest fix for the biggest gap is a PR, not a
  rewrite.
- `container`'s `ContainerPlugin`/`config.toml` plugin mechanism: can
  third parties register XPC plugins? If so, a `cm`-managed plugin could
  add machine features (mounts, forwarding) *inside* the supported
  architecture — L2.5, arguably the cleverest option of all.

## Recommendation

Keep `cm` on the CLI (L0). It's the only layer with a real stability
guarantee, the exec-passthrough model is a feature, and everything we'd
gain below L3 is cosmetic. Track two future triggers:

1. If structured output/error handling or subprocess overhead starts
   hurting → prototype the **L2 Swift shim** (`MachineAPIClient` behind
   `@_cdecl`, linked via `swift-rs`). No Swift rewrite of `cm` needed.
2. If mounts-outside-`$HOME` or port forwarding become real requirements
   and upstream won't take them → evaluate **L3** (Swift binary/library on
   `containerization`), accepting the signing and maintenance burden, or
   investigate the **plugin route** (open questions) which could deliver
   the same features while staying a guest of the `container` ecosystem.
