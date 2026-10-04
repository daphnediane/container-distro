# `container` internals we depend on

Everything in this repo is a client of Apple's `container` CLI. Most of
what we consume is its documented command surface, but a few code paths
read — and one writes — state that is *not* a public interface. This
document inventories every assumption we make about `container` and
`container machine` internals: what we rely on, where the code relies on
it, and what breaks when upstream changes it. Verified against
`container` 1.5.x; see [Version gating](#version-gating).

Risk tiers used below:

- **CLI** — flags and subcommands upstream documents; safe to use.
- **JSON** — `--format json` output shapes; less stable than CLI text
  (the schemas aren't a documented contract).
- **Behavior** — observed semantics the docs don't promise.
- **Layout** — filesystem internals under `appRoot`; pure implementation
  detail, gated by `warn_unverified_version()`.

## Binary discovery

- `/usr/local/bin/container` is preferred over PATH
  (`INSTALLED_CONTAINER_PATH`, `container_binary()`) — the location
  upstream's installer uses; preferring it blunts PATH hijacking.
- `CONTAINER_CLI` overrides the binary for everything spawned through
  `container_cmd()` — tests and development use this.
- `install-plugin` resolves `container` the same way (plus PATH), then
  derives `<prefix>/libexec/container-plugins/` from its real path
  (`plugin_root_for`) — assumes the `<prefix>/bin/container` install
  shape upstream's pkg produces.

## CLI commands consumed

| Command                                               | Used by                                                          |
| ----------------------------------------------------- | ---------------------------------------------------------------- |
| `system status [--format json]`                       | `system_running`, `app_root`, `container_version`, `cm --status` |
| `system start` / `system stop`                        | `ensure_started`, `cm --shutdown --system`                       |
| `machine list --format json`                          | `list_machines` (`cm -l`, resolution, shutdown)                  |
| `machine inspect [id]`                                | `inspect_machine*` (`cm -e`, `-l -v -v`)                         |
| `machine run -i [-n -u -w -e …] [-- cmd]`             | interactive shells, `--` commands, `-e` fallback, boot probe     |
| `machine create --name --cpus --memory --home-mount`  | `cm --install`                                                   |
| `machine set-default` / `machine stop` / `machine rm` | `cm -s` / `-t` / `--unregister`                                  |
| `create` (many flags — see spec.rs `create_args`)     | distro create/set/import                                         |
| `start` / `stop` / `delete`                           | distro boot/stop/rm/`set`                                        |
| `exec -i [-t] --user --env --workdir ID cmd…`         | `cm -e` on machines, all distro `run`, `init -u` provisioning    |
| `list --all --format json`, `inspect`                 | distro enumeration and `distro inspect`                          |
| `export`                                              | `distro export`                                                  |
| `image load -i`, `image list --quiet`, `image delete` | `import`, snapshot-image cleanup                                 |
| `run --rm --entrypoint -v`                            | guest-script unit tests only                                     |
| `--version`                                           | `cm --version` passthrough                                       |

## JSON shapes parsed (tier: JSON)

Parsed in `crates/cm-core/src/container.rs`; unknown fields are ignored,
missing ones fall back to defaults.

- `system status --format json`: `status` (compared against `"running"`),
  `paths.appRoot`, `server.version`.
- `machine list`: array of `{id, status, default, ipAddress, cpus,
  memory, diskSize}` — `status == "running"` is the only state we match.
- `machine inspect`: **array** (we take element 0) of `{id, status,
  containerId, userSetup: {uid, gid, username}, homeMount ("rw"/"ro"/
  "none"), platform: {os, architecture}, image: {reference}}`.
- `list`/`inspect`: `[{configuration: {id, labels, mounts: [{source,
  destination, options}], publishedPorts: [{hostAddress, hostPort,
  containerPort, proto}], networks: [{network}], ssh, resources: {cpus,
  memoryInBytes}, image: {reference}, platform, creationDate}, status:
  {state, networks: [{ipv4Address}]}}]`.
  - `options` containing `"ro"` marks a read-only mount.
  - `ipv4Address` arrives CIDR-suffixed (`192.168.64.13/24`); we strip at
    the `/`.
  - `creationDate` is ISO-8601 UTC.

## Behavioral assumptions (tier: Behavior)

### `container machine`

- **A machine is a container.** `machine inspect`'s `containerId` names a
  regular container we can `container exec` into directly — this is how
  `cm -e` gets argv-exact exec (`machine_exec_command`). The machine
  plugin models it as a container with a custom init (`/sbin.machine/
  init`), the host uid/gid provisioned, home virtiofs at the same path,
  SSH-agent forwarding, all caps, no masked paths; our distro spec
  replicates that shape (`spec.rs create_args`).
- `machine run` **shell-evaluates**: the guest init execs
  `$SHELL -c "$*"`, so argv is re-split (apple/container#1954). `cm -e`
  bypasses via `container exec`; the fallback path single-quotes each
  argument (`ArgvMode::Exact`); the shell-probe is passed as one
  pre-joined string (C12).
- `machine run` **boots a stopped machine** — we use
  `machine run -- true` as the boot primitive before `exec` or IP
  lookup.
- With no `-n`, `machine run`/`inspect` target the **default machine**.
- **`exec -w` silently creates missing directories** in the guest —
  `machine_workdir` therefore only ever returns paths guaranteed to
  exist (cwd under the shared home, or `/home/<userSetup.username>`).
- `machine run` **requires a TTY** for interactive shells; we always
  pass `-i` so piped stdin reaches the guest.
- First-write drop: `machine run` can lose the guest's first write —
  open upstream, see [gaps/exec-stdio](gaps/exec-stdio.md).
- `homeMount` values are `rw`/`ro`/`none`; only `none` disables the
  same-path share.
- Machine defaults (half host CPUs/memory) are mirrored by
  `default_resources` for distros.

### `container`

- `container exec` is **argv-exact** — no shell re-evaluation (distros
  rely on this for `run`; `cm -e` relies on it for machines).
- `--user` accepts `uid[:gid]` numerics (`0:0` for root) as well as
  names.
- `create` accepts `--masked-path NONE` / `--read-only-path NONE` as
  "clear the defaults" magic values; `--cap-add ALL`; `--ssh` (forwards
  `SSH_AUTH_SOCK` into the guest environment); `--env`; `--label`;
  `--entrypoint` may point into a `--volume` mount (our
  `/sbin.distro/init`).
- `--network none` yields an empty `configuration.networks`; the default
  attachment is named `"default"`.
- `container delete` removes the container's whole data directory —
  including `rootfs.ext4` — which is why `set` stages the filesystem in
  our own `preserved/` dir first.
- `container export` **deletes an existing output directory** (upstream
  #2325) — `check_export_output` refuses directories first.
- `export` snapshots `containers/<id>/rootfs.ext4` literally, ignoring
  any configured rootfs mount — so it fails on machines and on
  never-booted containers. We deliberately point `rootFsOverride` at the
  conventional path so export keeps working on distros after `set`
  (details: [gaps/export-import](gaps/export-import.md)).
- `container create`'s stdout is the container ID — discarded via
  `run_container_quiet`.
- `image load -i` accepts an OCI image-layout tar; we tag manifests with
  all three name annotations — `org.opencontainers.image.ref.name`,
  `io.containerd.image.name`, and Apple's
  `com.apple.containerization.image.name` (`oci.rs`) — so the reference
  resolves under `container`.
- `image list --quiet` prints normalized references; we strip a
  `docker.io/` prefix when matching our `local/distro-*` snapshots.
- Regular containers get one extra vCPU of runtime overhead (`nproc`
  reports `cpus + 1`).
- Names must be lowercase DNS `[a-z0-9-]`, ≤63 chars (`validate_name`).
- `exec` exit codes propagate (with the 255-ambiguity caveat, upstream
  #2210).

## On-disk layout under `appRoot` (tier: Layout)

Everything in this section is an implementation detail of
`container`'s storage — not a promised interface. Call sites are marked
with `warn_unverified_version()`.

- `appRoot` is discovered from `system status --format json` at
  `paths.appRoot` — never hard-coded.
- `containers/<id>/` is a container's data directory. Until first start
  it holds only `runtime-configuration.json`; on first `start` the
  daemon copies the image's materialized rootfs
  (`snapshots/<image-digest>/snapshot`, an ext4 file) to
  **`containers/<id>/rootfs.ext4`**. A never-booted container therefore
  has no rootfs — which is also why `container export` fails on one.
- **`runtime-configuration.json`** is the per-container runtime spec.
  `container distro set` rewrites `options.rootFsOverride` in it:

  ```json
  {"type": {"block": {"sync": {"fsync": {}}, "format": "ext4",
                     "cache": {"on": {}}}},
   "source": "<abs path to rootfs.ext4>", "destination": "/",
   "options": []}
  ```

  so `start` mounts our preserved ext4 instead of copying the image
  snapshot over `rootfs.ext4` (the copy would fail on the existing
  file). This is the same mechanism `container machine` uses for its
  plugin-state disks — we reuse it, we didn't invent it.
- **Machine rootfses live elsewhere**: a machine's backing container
  points its rootfs mount at
  `plugin-state/machine-apiserver/machines/<name>/rootfs.ext4` — the
  reason `container export` can't snapshot machines. We never write to
  machine backing containers; the path matters only as prior art for
  `rootFsOverride` (and if a host-side ext4 export workaround lands).
- `rootfs.ext4` is **sparse** — allocated blocks (`st_blocks * 512`)
  are what `machine list` reports as DISK and what `distro list` reports
  (`container_disk_usage`).
- `fs::copy` on APFS is a clonefile, so staging a multi-GB rootfs into
  `preserved/` is instant and CoW.
- Only distro-owned containers are patched — identified by the
  `<prefix>.distro` label before `set` touches anything.

## Plugin registration internals

`container`'s plugin discovery is in-tree but undocumented
(`PluginLoader`); `install-plugin` depends on:

- `<prefix>/libexec/container-plugins/<name>/` containing `config.toml`
  plus `bin/<name>`.
- A config *without* `servicesConfig` is a **CLI plugin**: `container
  <name> …` `execvp`s `bin/<name>` (signal handling reset — the same
  passthrough trick `cm` uses).
- `config.toml` keys: `abstract`, `author`, `version`.
- The exec'd plugin may receive the subcommand name as `argv[1]` — we
  drop it when it equals `distro` (`main.rs`).
- `uninstall-plugin` identifies our install by the `abstract` text.

## Guest-side environment contract

- **virtiofs shares**: `$HOME` (or an arbitrary `-v` source) appears at
  the same absolute path in the guest; `"ro"` in mount options marks
  read-only; uid/gid follow the host mapping; host case-sensitivity
  carries through. VZ refuses to share volumes it can't enumerate
  (TCC/SIP-protected — Time Machine destinations, `com.apple.*` mounts),
  which is why `--automount` probes and skips them.
- **`--ssh` / `SSH_AUTH_SOCK`**: the agent socket appears in the guest
  environment and filesystem; our init chowns it to the provisioned
  uid:gid at boot.
- **`CONTAINER_*` env**: the machine plugin's init contract uses
  `CONTAINER_MACHINE_ID`, `CONTAINER_USER`, `CONTAINER_UID`,
  `CONTAINER_GID`, `CONTAINER_HOME` — we set the same names at `create`
  so our `init` behaves like the machine's (plus our own
  `CONTAINER_ADMIN` for the sudo grant). If upstream renames these,
  only our distro provisioning is affected — machines are untouched.
- **Guest home is `/home/<username>`** (a Linux home on the persistent
  disk), distinct from the shared macOS home — from
  `userSetup.username` on machines, our `HostUser` on distros.
- **Networking**: guests NAT through the vmnet `192.168.64.x` subnet;
  `192.168.64.1` is gateway *and* DNS. Not used by any code path today —
  recorded for the GUI/X11 work ([gaps/gui-apps](gaps/gui-apps.md)).
- **Starting directory**: `machine run` starts the guest process at the
  host cwd when it's under the shared `$HOME`, else the guest home.
  `cm` reproduces this for `exec` and for distros via `guest_path`
  (longest-matching mount source wins).

## Version gating

`VERIFIED_CONTAINER_MINOR = ["1.5"]` (cm-core/container.rs) records the
`major.minor` versions whose appRoot internals we've verified. Any code
that reads or writes `appRoot` contents — `container_disk_usage`,
`container_dir`/`preserve_rootfs`/`restore_rootfs`/`set_rootfs_override`,
`summaries` — calls `warn_unverified_version()`, which warns once per
process on stderr when `server.version` isn't a verified release.

**On a `container` upgrade, re-verify:**

1. `system status --format json` still exposes `paths.appRoot` and
   `server.version`.
2. `containers/<id>/rootfs.ext4` still appears at first `start`, sparse.
3. `runtime-configuration.json` still exists and `options.rootFsOverride`
   still drives which block file is mounted (diff a machine's file —
   its override still points into `plugin-state/`).
4. `machine inspect` still reports `containerId` + `userSetup`.
5. `container exec -w` still auto-creates missing dirs (else
   `machine_workdir` needs a re-think).
6. `machine run` argv handling — if #1954 is fixed, the
   single-quote-per-arg fallback and the pre-joined shell probe can be
   simplified.
7. `--masked-path NONE`/`--read-only-path NONE` and `--ssh` unchanged.
8. Plugin discovery path and the argv[1] convention.
9. `container export` semantics around `rootFsOverride` and existing
   output dirs.
10. Then bump `VERIFIED_CONTAINER_MINOR`.

## Related documents

- [naming](naming.md) — *our* persistent names (labels, state dir);
  the other side of the same audit
- [security](security.md) — trust model, including T5 (binary
  resolution) and T6 (state dir inside the shared home)
- [lower-level-integration](lower-level-integration.md) — the stack
  below the CLI (XPC, plugins, `containerization`, VZ)
- Gap docs: [exec-stdio](gaps/exec-stdio.md),
  [export-import](gaps/export-import.md),
  [mounts-outside-home](gaps/mounts-outside-home.md),
  [service-startup](gaps/service-startup.md),
  [subprocess-overhead](gaps/subprocess-overhead.md)
