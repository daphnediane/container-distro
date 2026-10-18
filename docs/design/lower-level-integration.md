# Integration levels and the settled architecture

Decision record for how far `cm`/`container-distro` reach below the
`container` CLI. We settled on **L0 CLI composition**: `cm` shells out
to `container` (including `container machine`), and distros are regular
containers created through the same CLI. This doc keeps the "rung"
shorthand the other design docs use meaningful, records why the
alternatives lost, and lists what would make us revisit. Written
against `container` 1.5.x; the exploratory survey it condenses was
research, not commitment.

## How the stack is layered

```text
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

Key facts:

- `container` is Swift over the [Containerization] package; the CLI
  talks XPC to `container-apiserver`, which spawns helpers for images,
  networking, and per-container runtimes ([technical-overview.md]).
- **`container machine` is a plugin, not core** -- `MachineClient`
  speaks XPC to `com.apple.container.core.machine-apiserver`, a
  `loadAtBoot` plugin with its own guest init resource. The plugin
  system is a supported extension point for third parties.
- **`machine run` is just `createProcess` on a specially-configured
  container** -- a container with a custom init, UID/GID-mapped user,
  home-dir virtiofs mount, SSH-agent forwarding, all caps, and no
  masked paths ([container-machine.md]). Distros replicate that same
  configuration from the CLI.
- `apple/container` exports library products (`MachineAPIClient`,
  `ContainerAPIClient`, `ContainerXPC`, `ContainerPlugin`, ~20 more)
  and exact-pins `containerization`; `containerization` requires macOS
  26 + Xcode 26 to build.

## Rung by rung: how low can `cm` go?

| Level        | Interface                                           | Language needed                             | Still needs `container` installed? | API stability                                                            |
| ------------ | --------------------------------------------------- | ------------------------------------------- | ---------------------------------- | ------------------------------------------------------------------------ |
| L0 (current) | `container` CLI subprocess                          | Rust only                                   | yes                                | CLI backward compat within major versions                                |
| L1           | XPC to apiserver / machine plugin                   | any (XPC is a C API)                        | yes                                | only `container-apiserver` XPC documented stable; plugin routes internal |
| L2           | `MachineAPIClient` / `ContainerAPIClient` libraries | Swift (C shim for Rust)                     | yes                                | source-stable-ish, but exact-pin churn                                   |
| L2.5         | third-party plugin (`libexec/container-plugins/`)   | any (CLI) / Swift or C ABI (daemon)         | yes                                | plugin contract internal but stable-shaped; used by Apple itself         |
| L3           | `containerization` package directly                 | Swift (C shim for Rust)                     | **no**                             | pre-1.0; minor-version source stability only                             |
| L4           | Virtualization.framework / vmnet directly           | Rust (`objc2-virtualization`) or Swift/ObjC | **no**                             | Apple framework -- stable, but huge scope                                |

## The plugin route (L2.5)

`container` has a third-party plugin system (`Sources/ContainerPlugin/`)
and `container machine` itself is implemented as one. We ship the
**CLI-plugin** form -- no daemon: `container-distro` composes
`container create` (our init as entrypoint, `--cap-add ALL`, no masked
paths, `--ssh`, `CONTAINER_*` env), `exec`, `export`, and `image load`.
It ships as a library `cm` links directly, plus a `container distro`
plugin binary that `install-plugin` registers under
`<prefix>/libexec/container-plugins/distro`. Distros are tracked by
label, not by a plugin-owned store.

A *daemon* plugin (`servicesConfig` present; `core`/`auxiliary` type)
is the remaining escalation step for things a CLI fundamentally can't
do -- e.g. watching guest listeners for automatic port forwarding. The
plugin contract (directory layout, `config.toml`, MachService naming,
XPC routes) is an implementation detail of the open source: usable, but
re-verify on each `container` release -- see container-internals.md.

## Rejected rungs

- **L1 -- XPC from Rust.** XPC is a plain C API and the route/key
  definitions are in the open source, but the message schema is an
  implementation detail, not a documented contract. Trading the stable
  CLI surface for an undocumented wire format is the wrong direction
  for a compat shim whose job is being boring and dependable.
- **L2 -- Swift client libraries behind a C shim.** The best next rung
  *if one is ever needed*: `MachineAPIClient`/`ContainerAPIClient` are
  exported products, so a small Swift dylib could expose a handful of
  `@_cdecl` functions (list, boot, exec with fd plumbing) -- real
  errors, no JSON scraping, no subprocess management. Costs: a
  mixed-language build (`swift-rs`/`swift-bridge`/UniFFI; everything
  crosses the C ABI, and Swift `async` and non-C types don't cross
  without bridging) and a dependency that exact-pins `containerization`.
- **L3 -- `containerization` directly.** "Build our own Lima": own
  kernel boot, the vminitd vsock protocol, OCI unpacking, networking,
  and lifecycle -- reimplementing everything the machine plugin does
  for free. Plus entitlements: `com.apple.security.virtualization`, and
  `com.apple.vm.networking` is a *restricted* entitlement needing Apple
  approval for Developer-ID distribution. Only justified if a hard
  requirement can't be expressed any other way *and* upstream won't
  take it.
- **L4 -- Virtualization.framework/vmnet raw.** Even pure Rust is
  technically possible (`objc2-virtualization`), but it reimplements
  Containerization itself to save a language boundary. Don't.
- **Swift rewrite.** At L2 a rewrite buys the same capabilities as
  calling the CLI -- only the IPC mechanism differs, not capability.
  The hard part (TTY passthrough, exit codes, process semantics) is
  already solved in Rust via `exec()` passthrough, arguably better than
  a library call since exec preserves signal/window-size handling for
  free. Only worth it as a means to L2/L3, never as a goal.

## When we'd revisit

Only if a limitation makes the current architecture impossible:

1. Structured output/errors or subprocess overhead starts hurting →
   prototype the **L2 Swift shim** (`MachineAPIClient`/
   `ContainerAPIClient` behind `@_cdecl`), optionally loaded as a
   `cm-swift-bridge` dylib; interactive shells keep using `exec`'d
   `container machine run`.
2. Something needs a daemon -- e.g. watching guest listeners for
   WSL-style automatic port forwarding ([upstream-limitations]) → a
   `core`/`auxiliary` plugin inside Apple's signing/entitlement
   envelope.
3. The plugin contract proves unstable, or the apiserver can't express
   what we need → evaluate **L3**, accepting the signing and
   maintenance burden.

[Containerization]: https://github.com/apple/containerization
[container-machine.md]: https://github.com/apple/container/blob/main/docs/container-machine.md
[technical-overview.md]: https://github.com/apple/container/blob/main/docs/technical-overview.md
[upstream-limitations]: upstream-limitations.md#no-localhost-port-forwarding-for-machines
