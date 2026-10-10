# `container distro` usage

`container-distro` creates **distros**: long-lived, machine-like
containers. A distro is a regular `container` container configured the
way `container machine` configures a machine -- your account provisioned
inside, home shared at the same path, the image's init as PID 1 -- plus
what machines lack: mounts outside `$HOME`, published ports,
export/import, and `set` for changing settings after creation.

It runs two ways:

- **`container distro <cmd>`** -- once registered as a `container` CLI
  plugin (`install-plugin`)
- **`container-distro <cmd>`** -- standalone; identical interface. `cm`
  links the same library, so `cm` distro features never need the plugin.

## What a distro is

- your macOS account is provisioned inside (same name/uid/gid,
  passwordless sudo/doas), with `SSH_AUTH_SOCK` forwarded
- `$HOME` is shared at the same path (`--home-mount rw|ro|none`)
- the image's own init runs as PID 1 -- or an idle PID 1 for images
  without one -- via our `assets/init` entrypoint (mounted read-only at
  `/sbin.distro`)
- `--cap-add ALL`, no masked or read-only paths, `--ssh`
- a regular container otherwise: inspectable, works with `container ls`,
  `container exec`, etc.

Distros are tracked with `io.github.daphnediane.container-distro.*`
labels and per-user state under `~/Library/Application
Support/container-distro/` -- see [naming.md]. The `container`
daemon is exercised through its CLI plus a few `appRoot` internals; a
once-per-process warning appears on unverified `container` releases.

Like `container machine`, a stopped distro is just durable state: a
*journal* directory `distros/<name>/` holding `spec.json` (the desired
configuration, creation `origin` metadata, and the per-distro lock),
`rootfs.ext4` (the disk), and `sbin.distro/` (the distro's own copy of
the init assets). The container exists only while running — `start`
materializes a fresh one from the journal and attaches the journaled
disk via `rootFsOverride`, `stop` folds the disk back and deletes the
container, so tampered runtime configuration never survives a
stop/start. The materialized container is named `<name>-<6hex>` and
gets a fresh suffix each time — our ops find it by its distro label,
so `container ls` shows e.g. `alpine-f4f8a1` rather than `alpine`.
Journal files are `uchg`-locked at rest (security.md T6).

## Subcommands

### `create [-n NAME] [OPTIONS] IMAGE`

Create a distro from an image and boot it. Name defaults to a derivation
from the image.

| Option                 | Description                                                                                                                |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `-n, --name`           | Distro name (lowercase DNS-style)                                                                                          |
| `-v, --volume`         | `SRC:DST[:ro]` share a host directory (repeatable)                                                                         |
| `--automount[=MODE]`   | Share every `/Volumes/<X>` at `/mnt/<x>`; `MODE` is `rw` (default) or `ro` (skips hidden/`com.apple.*`/unreadable volumes) |
| `-p, --publish`        | `[HOST_IP:]HOST[:GUEST][/tcp\|udp]` (repeatable; HOST_IP defaults to `127.0.0.1`)                                          |
| `--cpus`               | Virtual CPUs (default: half the host's)                                                                                    |
| `--memory`             | e.g. `8G` (default: half the host's)                                                                                       |
| `--home-mount`         | `rw` (default), `ro`, `none`                                                                                               |
| `--network`            | Container network name; `none` for no network                                                                              |
| `--ssh` / `--no-ssh`   | Forward the host SSH agent socket (default: on)                                                                            |
| `--sudo` / `--no-sudo` | Provision passwordless sudo/doas (default: on)                                                                             |
| `--restricted`         | Defaults preset: `--home-mount none --network none --no-ssh --no-sudo` (alias `--untrusted`)                               |
| `--no-boot`            | Create without booting                                                                                                     |
| `--set-default`        | Make this the default distro (automatic when nothing is)                                                                   |
| `--force`              | Allow the name of an existing `container machine` — see below                                                              |

Creating a distro named after an existing `container machine` is
**refused** without `--force`: `cm` resolves a shared name to the
distro, silently shadowing the machine everywhere `cm` operates. The
same refusal applies to `import`, `catalog install`, `migrate`, and
`rename` — and in reverse to `cm --install --machine` /
`cm --import --machine` when a distro holds the name. `--force` makes
the collision deliberate (it still warns).

`--automount` is live, not create-once: the `/Volumes` scan reruns on
`set` and whenever a stopped distro starts, so attaching a volume adds
it and ejecting one drops it -- no `set --add-volume`/`--rm-volume`
chore. Only the canonical `/Volumes/<X>` → `/mnt/<x>` pairs are managed;
your own mounts (including differently-targeted `/Volumes` mounts) are
never reconciled away.

`--restricted` is a **defaults preset, not a sandbox**: explicit flags
still apply, `set` can reopen anything, and `run --root`/`exec` as `0:0`
always work. See security.md.

### `run [-n NAME] [OPTIONS] [-- CMD...]`

Run a command or interactive shell in a distro, booting it if needed.
Uses `container exec`, so argv is exact -- nothing is re-split.

| Option          | Description                                                                       |
| --------------- | --------------------------------------------------------------------------------- |
| `-n, --name`    | Distro (default distro if omitted)                                                |
| `-u, --user`    | User (name or `uid[:gid]`); default: your account                                 |
| `--root`        | Run as root                                                                       |
| `-w, --workdir` | Guest working dir (default: cwd if it's under a shared path, else the guest home) |
| `-e, --env`     | `KEY=VALUE` (or `KEY` to inherit), repeatable                                     |
| `--shell`       | Run the command line through the user's shell (`$SHELL -c`)                       |
| `--no-login`    | Interactive shell is non-login                                                    |
| `CMD...`        | Command (default: a login shell)                                                  |

### `list` (alias `ls`)

| Option        | Description                 |
| ------------- | --------------------------- |
| `-q, --quiet` | Names only                  |
| `--running`   | Only running distros        |
| `--format`    | `table` (default) or `json` |

The table shows NAME, IMAGE, CREATED, STATE, IP, CPUS, MEMORY, DISK,
MOUNTS, PORTS, and a `*` in DEFAULT. Stopped distros list from their
journals — CREATED is the distro's real creation time (the journal's
`origin`), not the container's. For distros created from a file or
catalog import, IMAGE shows the origin instead of the generated
`local/distro-*` snapshot reference (`file:<basename>`, `catalog:<name>`;
`--format json` carries the full `origin` object).

### `catalog`

The installable-distro catalog is the same compiled-in list
`cm -l -o` uses — Microsoft's `DistributionInfo.json` schema plus our
`Image` extension. `create` remains the explicit image path and
`import` remains the explicit local-file path; `catalog` resolves
friendly catalog names and `.wsl` downloads.

| Command                                                  | Description                                                                                                                                             |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `catalog list [-v ...] [--catalog FILE]`                 | `NAME`/`FRIENDLY NAME`; `-v` adds local state and instances (machines and distros), repeated `-v` adds the install source and image ref or download URL |
| `catalog info DISTRO [--catalog FILE] [--format FORMAT]` | Show the entry's catalog fields plus cache, image, and existing distro state (`FORMAT` is `table` or `json`)                                            |
| `catalog install [DISTRO] [OPTIONS]`                     | Install a catalog entry (`catalog list` shows names); bare installs the default entry                                                                   |
| `catalog cache [--catalog FILE]`                         | Show cached `.wsl` downloads, their catalog entry, and the image they were imported as                                                                  |
| `catalog purge`                                          | Delete cached `.wsl` downloads (imported distros/images are unaffected)                                                                                 |

`catalog install` accepts every `create` option (`-n`, mounts, ports,
resources, `--restricted`, `--no-boot`, `--set-default`, …). It only
resolves catalog entries — use `create IMAGE` for an image reference and
`import NAME FILE` for a local tar/`.wsl`. Image-backed entries become
the same distro `create` would make, named for the catalog entry unless
`-n` overrides it. `.wsl` entries are downloaded, verified against the
catalog's SHA-256, cached, then imported with `catalog:<name>`
provenance; the package's `oobe.defaultName` supplies the distro name
when it is present and `-n` is not.

`catalog info DISTRO` shows the entry's own `Default` flag, whether the
resolved catalog default selects it, every `Image`/`Amd64Url`/`Arm64Url`
field, the host-selected source, and the matching local state: a cached
`.wsl` file and its recorded source, local images (including the
verified `rootfs-sha256` link for imports), existing distro summaries,
and machines built from the same source. `--format json` emits the same
report as a structured JSON object. Like `catalog list -v`, the probes
are best-effort and never start `container` services just to answer the
query.

Catalog reporting never resolves a name to one backend: if a machine
and a distro share an entry-relevant name, `catalog list -v` shows both
instances suffixed `(machine)` / `(distro)` and `catalog info` adds a
`Name conflicts` section (`local.conflicts` in JSON). Selector-driven
resolution (`--machine`, `--distro`, or `cm`'s distro-first default)
only applies to operations like `start`, `stop`, `run`, or `unregister`.

`--catalog FILE` is an explicit per-invocation override — never
auto-loaded from `$HOME` — using the same schema as `cm --catalog`.
`catalog cache` uses it only to label entries; `catalog purge` does not
consult it.

### `inspect NAME...`

Dump the distros' `container` configuration as JSON.

### `start NAME` / `stop NAME...`

Boot a stopped distro / stop running ones. For an `automount` distro,
`start` (and the boot a `run` triggers) first reconciles `/Volumes`
mounts with what's actually attached.

### `delete` (alias `rm`) `-f|--force NAME...`

Delete distros and their storage; `-f` stops them first. Deletes
journal state even when the container itself is already gone.

### `set NAME [OPTIONS]`

Change settings after creation: stop, update the journal, start — the
same stop/update/start cycle a hand-edited restart would follow, so an
interrupted `set` and a deliberate stop are the same on-disk state.
The journaled `rootfs.ext4` reattaches via `rootFsOverride` — no image
round-trip. The journal's `spec.json` is the per-distro flock target,
serializing `set` against start/delete.

| Option                                        | Description                                                                                                                     |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `--cpus`                                      | Virtual CPUs                                                                                                                    |
| `--memory`                                    | e.g. `8G`                                                                                                                       |
| `--home-mount`                                | `rw`/`ro`/`none`                                                                                                                |
| `--network`                                   | Network name; `none` disables, `default` restores                                                                               |
| `--ssh`/`--no-ssh`                            | SSH-agent forwarding                                                                                                            |
| `--sudo`/`--no-sudo`                          | Sudo/doas provisioning -- `--no-sudo` stops *future* grants; sudoers/doas files already inside the guest remain                 |
| `--automount[=rw\|ro\|none]`/`--no-automount` | Auto-manage `/Volumes` mounts at `/mnt/<name>`; enabling reconciles now and at each start, `none`/`--no-automount` removes them |
| `--add-volume`                                | `SRC:DST[:ro]` add or replace a mount by guest path                                                                             |
| `--rm-volume`                                 | `DST` remove the mount at that guest path                                                                                       |
| `--publish`                                   | Add a published port (replaces one on the same host port)                                                                       |
| `--unpublish`                                 | `HOST_PORT` stop publishing that host port                                                                                      |

A **never-booted** distro has no `rootfs.ext4` yet — the disk
materializes on first boot, so `set` just rewrites the journal.

#### Interrupted `set` recovery

There is no distinct interrupted state: killing a `set` mid-flight
leaves the same journal-plus-disk rest state a clean stop does. The
next operation converges it — any leftover container is dismantled and
a fresh one materializes from the journal.

### `set-default [NAME] | --clear`

Set the default distro -- or unset it with `--clear`. The default distro
overrides the default machine in `cm`.

### `export NAME [-o FILE]`

Write the distro's root filesystem as a tar (stdout by default). Fails
on never-booted distros -- there is no `rootfs.ext4` yet.

### `import NAME FILE [OPTIONS]`

Create a distro from a rootfs `tar`/`tar.gz` (`-` = stdin) — a `.wsl`
distribution package works too (it's a rootfs tar; its
`wsl-distribution.conf` lands in the guest but is only honored by
`cm --install --from-file`'s name suggestion). Accepts all `create`
options including `--restricted`. Importing an image is trusting it
with your identity -- see security.md.

### `rename OLD NEW [--force]`

Rename a distro: the journal — `spec.json`, `rootfs.ext4`, and
`sbin.distro/` — moves to `NEW`, the default marker follows, and a
running distro restarts under the new name. The runtime container is
never renamed; it's dismantled before the move and materialized fresh
after, so nothing `container`-side ever carries the old name. `NEW`
must be free of distros, staged state, and — unless `--force` —
machines (the same shadow rule `create` enforces). An imported
distro's `local/distro-OLD` snapshot image keeps its name and labels,
so `rm` won't remove it automatically afterward.

### `migrate [--in] MACHINE | --out DISTRO [-n NAME] [--keep] [OPTIONS]`

Migrate between a `container machine` and a distro without an
export/import round-trip: the source's `rootfs.ext4` is cloned onto the
target — the journaled disk going in, written over the new machine's
plugin-state disk going out. The source is removed unless `--keep`;
`--keep` requires `-n NAME`, since a kept source holds its name and a
result sharing it would shadow it for `cm` (`--force` permits even
that, deliberately). Any target name that's taken in the other
namespace is refused unless `--force`.

`--in` (the default) turns a machine into a distro carrying the
machine's cpus/memory/home-mount/user; it accepts `create` options as
overrides. Naming a pre-journal distro container instead *adopts* it:
its live configuration becomes the journal — the explicit opt-in for
distros the one-time upgrade pass didn't (or couldn't) adopt, and the
only trust-the-container path afterwards. `--out` creates a machine
from the distro's image, boots it once to materialize its disk, then
replaces that disk with a clone of the distro's rootfs; only the
`machine create` subset applies (`--cpus`, `--memory`, `--home-mount`,
`--no-boot`, `--set-default`) and other `create` options are refused. A
never-booted source has no filesystem to carry, so the target starts
from the image.

### `install-plugin [--plugin-dir DIR] [--from BIN]`

Register `container-distro` under
`<container prefix>/libexec/container-plugins/distro/` (config.toml +
`bin/distro` copy) so `container distro` works. Needs root for the
default `/usr/local` prefix:

```bash
sudo container-distro install-plugin
```

`--from` updates via an already-installed copy; `--plugin-dir` targets a
custom root.

### `install-man [--dir DIR]`

Write `container-distro(1)` plus one page per subcommand
(`container-distro-create(1)`, …), generated from the CLI definition.
Default location: `share/man/man1` of the binary's install prefix --
`$CARGO_HOME/share/man/man1` after `cargo install`. See [install.md].
Pages for subcommands removed since the last install are pruned as well
(only files that still look like our generated pages).

### `uninstall-man [--dir DIR]`

Remove the pages `install-man` wrote, including stale
`container-distro-*.1` pages for subcommands that no longer exist.
Only files that still look like our generated pages are removed -- a
foreign or edited page of the same name is left in place with a
warning.

### `install-completions [--dir DIR]`

Write bash, zsh, and fish completions for `container-distro`, generated
from the CLI definition. The share root defaults to the binary's install
prefix (`$CARGO_HOME/share` after `cargo install`); files land in
`bash-completion/completions/`, `zsh/site-functions/`, and
`fish/vendor_completions.d/`. See [install.md][install.md-2].

### `uninstall-completions [--dir DIR]`

Remove the scripts `install-completions` wrote. Only files that still
look like generated completions are removed -- a foreign or edited file
of the same name is left in place with a warning.

### `uninstall-plugin [--plugin-dir DIR]`

Remove the plugin registration (only if it was installed by
`container-distro`).

## Mount and publish spec syntax

- **Mount**: `SRC:DST[:ro]` -- absolute host path, absolute guest path,
  optional `ro`.
- **Publish**: `[HOST_IP:]HOST_PORT[:GUEST_PORT][/tcp|udp]` -- `3000`,
  `8080:80`, `0.0.0.0:8080:80/udp`. HOST_IP defaults to `127.0.0.1`;
  guest port defaults to the host port; protocol to `tcp`.

Keep publishes on loopback unless you want LAN-reachable services -- a
`0.0.0.0` bind changes exposure for *other* machines too.

[install.md]: install.md#man-pages
[install.md-2]: install.md#shell-completions
[naming.md]: naming.md
