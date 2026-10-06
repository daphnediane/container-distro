---
name: container-distro
description: How `cm` (WSL-compatible wrapper) and the `container distro` plugin map onto Apple's `container` CLI, and how to verify them
---

# container-distro (`cm` + `container distro`)

A Cargo workspace with three crates:

- `crates/cm` — the `cm` binary: `wsl.exe` argument semantics on top of
  Apple's `container` CLI. Optionally symlinked to `wsl`.
- `crates/container-distro` — a library plus the `container-distro`
  binary. It registers as the `container distro` CLI plugin and creates
  machine-like containers ("distros") with what `container machine`
  lacks: mounts outside `$HOME`, published ports, export/import, `set`.
- `crates/cm-core` — shared plumbing: `container` CLI wrappers and serde
  types, `naming` (persistent names/labels), `oci` (rootfs tar → OCI
  layout), `forward` (TCP forwarder), `table`.

Persistent names (labels `io.github.daphnediane.container-distro.*`, the
state dir `~/Library/Application Support/container-distro/`) all come
from `cm_core::naming`; see `doc/naming.md` before changing any of them.

## `cm` argument mapping (machines)

- No args / `-d <m>` / `-u <u>` / `--cd <dir>` / `--shell-type <t>` →
  `container machine run -i` (`exec()`ed for TTY passthrough)
- `-e <cmd...>` → argv-exact via `container exec` in the machine's
  backing container (`containerId` from `machine inspect`, user from
  `userSetup`, workdir only if it exists in the guest — `exec -w`
  creates missing dirs). Falls back to `machine run` with each arg
  single-quoted (apple/container#1954)
- `-- <cmd...>` or bare positional args → run via the guest shell (WSL
  semantics)
- `-l` → WSL-style name list; `-l -v` → exact `NAME STATE VERSION`
  table (VERSION is always `2`); `-l -v -v` → extra columns from
  `machine inspect`; `-q`, `--running`
- `-s <m>` → `machine set-default`; `-t <m>` → `machine stop`;
  `--unregister <m>` → `machine rm`
- `--shutdown` stops running machines; `--shutdown --system` also runs
  `container system stop`
- `--install <image>` (`--name`, `--no-launch`, `--cpus`, `--memory`,
  `--home-mount`) → `machine create`, then a shell
- `--forward HOST[:GUEST]` (repeatable, `-d` selects) → foreground
  localhost → machine-IP TCP forwarder
- `--status` → `container system status`
- `--export`/`--import` are punted (see `doc/gaps/export-import.md`);
  the prototype lives on branch `wip/export-import`

Before any machine operation `cm` runs `container system start` if
services are down.

## `container distro` (plugin / library)

A distro is a regular container: our `assets/init` is the entrypoint
(mounted read-only at `/sbin.distro`), it has `--cap-add ALL`, no masked
or read-only paths, `--ssh`, the host account passed through
`CONTAINER_*` env, and the home directory at the same path. `run` uses
`container exec` (argv-exact). `set` recreates the container on the same
filesystem: it clones `containers/<id>/rootfs.ext4` (`fs::copy` → clonefile
on APFS) to `state_dir()/preserved/<id>.ext4` with a `<id>.json` spec
journal, recreates, moves it back, and sets `options.rootFsOverride` in
`runtime-configuration.json` (the machine-apiserver mechanism) so `start`
mounts it instead of copying the image snapshot — no tar/image round-trip.
Only distro-owned containers are patched, never machine backing containers.
A per-distro `flock` (`locks/<id>.lock`) serializes `set`/boot/`delete` —
a racing second `set` just applies on top of the first. An interrupted
`set` self-heals: `set`, `run`/`exec`, and `start` call
`recover_interrupted` before touching the distro (recreate-from-journal
when the container is gone, move-back when it exists, kept-with-warning
when the container already has a rootfs); `list` shows orphans as
`recreating` (lock held) or `interrupted` (lock free) pseudo-rows, `rm`
deletes staged files even without a container, and `create`/`import`
refuse while a staged fs exists for the name (`set` recovers, `rm`
discards).
A never-booted distro has no `rootfs.ext4`, so it is recreated from its
recorded image (this is also why `container export` fails on never-booted
distros and on machines, whose ext4 lives under `plugin-state/`). Code
that reads or writes `appRoot` internals calls `warn_unverified_version()`
— a once-per-process warning when the daemon isn't a
`VERIFIED_CONTAINER_MINOR` release. Ports default to
`127.0.0.1`. Only an explicit `--set-default` sets the default distro.

`migrate MACHINE` turns a `container machine` into a distro: it finds
the machine's `rootfs.ext4` (backing container's `rootFsOverride`
source, else `rootfs.json`, else the plugin-state path — machines get
a backing container only after first boot), stages it through
`preserved/` like `set` does, and recreates it as a distro carrying
the machine's cpus/memory/home-mount/user. `--keep` retains the
machine; `-n` renames. Distro → machine is planned as a separate
subcommand.

`create`/`import` take `--restricted` (alias `--untrusted`): a defaults
preset giving `--home-mount none --network none --no-ssh --no-sudo`
(explicit flags still apply; `cm --install --restricted` implies
`--distro`). `--no-sudo` distros mount `sbin.distro.restricted/`, which
lacks `grant-admin.sh`; the `admin` label records `true`/`false`/`never`
(`never` = predates the grant env — `set` recreates don't re-arm it, an
explicit `--sudo` does). `set` accepts `--network`, `--ssh`/`--no-ssh`,
`--sudo`/`--no-sudo`.

`cm` treats machines and distros as one namespace (`crates/cm/src/backend.rs`),
linking the library rather than exec'ing the plugin: `-d NAME` resolves a
distro first, then a machine; a default distro overrides the default
machine; `-l` merges both (KIND column at `-v -v`); `--install --distro`
/ `--share` / `--publish` create distros. `CM_BACKEND=machine` disables
distros.

## Host filesystem (machines)

`container machine` mounts macOS `$HOME` via virtiofs at the **same
path**; nothing outside `$HOME` is shared (container 1.5.0;
apple/container#1805, #2278). Distros add arbitrary `-v SRC:DST[:ro]`
mounts and `--automount` (`/Volumes/<X>` → `/mnt/<x>`, skipping
hidden/`com.apple.*`/unreadable volumes — e.g. TM local snapshots and
TCC-protected backup destinations, which VZ refuses to share).
Automounts are resolved once at `create`, not live: later-attached
volumes need `set --add-volume`, and an ejected volume fails `start`
(`path does not exist`) until dropped with `set --rm-volume`.

## Verify

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
container machine list                       # machines for testing
cm -l; cm -l -v; cm -l -v -v                 # WSL-shaped lists
echo hi | cm -d alpine -- cat                # stdin reaches the guest
cm -d alpine -e printf ':%s:\n' 'a b'        # -> :a b:
D=./target/debug/container-distro
$D create -n d1 -v /Volumes/X:/mnt/x -p 8080 alpine:latest
$D run -n d1 -- id                           # host uid, provisioned user
$D set d1 --cpus 2                           # recreate, state preserved
$D rm -f d1
```

Test machine: `container machine create --name alpine --set-default alpine:latest`.
Installing the plugin needs root:
`sudo container-distro install-plugin` (after `cargo install --path crates/container-distro`).
Man pages are generated from the clap definitions by `clap_mangen`:
`cm --install-man` / `container-distro install-man` write to
`<bin>/../share/man/man1` for the invoking binary (`--dir` overrides).
