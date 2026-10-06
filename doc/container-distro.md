# `container distro` usage

`container-distro` creates **distros**: long-lived, machine-like
containers. A distro is a regular `container` container configured the
way `container machine` configures a machine — your account provisioned
inside, home shared at the same path, the image's init as PID 1 — plus
what machines lack: mounts outside `$HOME`, published ports,
export/import, and `set` for changing settings after creation.

It runs two ways:

- **`container distro <cmd>`** — once registered as a `container` CLI
  plugin (`install-plugin`)
- **`container-distro <cmd>`** — standalone; identical interface. `cm`
  links the same library, so `cm` distro features never need the plugin.

## What a distro is

- your macOS account is provisioned inside (same name/uid/gid,
  passwordless sudo/doas), with `SSH_AUTH_SOCK` forwarded
- `$HOME` is shared at the same path (`--home-mount rw|ro|none`)
- the image's own init runs as PID 1 — or an idle PID 1 for images
  without one — via our `assets/init` entrypoint (mounted read-only at
  `/sbin.distro`)
- `--cap-add ALL`, no masked or read-only paths, `--ssh`
- a regular container otherwise: inspectable, works with `container ls`,
  `container exec`, etc.

Distros are tracked with `io.github.daphnediane.container-distro.*`
labels and per-user state under `~/Library/Application
Support/container-distro/` — see [naming.md](naming.md). The `container`
daemon is exercised through its CLI plus a few `appRoot` internals; a
once-per-process warning appears on unverified `container` releases.

## Subcommands

### `create [-n NAME] [OPTIONS] IMAGE`

Create a distro from an image and boot it. Name defaults to a
derivation from the image.

| Option                 | Description                                                                                  |
| ---------------------- | -------------------------------------------------------------------------------------------- |
| `-n, --name`           | Distro name (lowercase DNS-style)                                                            |
| `-v, --volume`         | `SRC:DST[:ro]` share a host directory (repeatable)                                           |
| `--automount`          | Share every `/Volumes/<X>` at `/mnt/<x>` (skips hidden/`com.apple.*`/unreadable volumes)     |
| `-p, --publish`        | `[HOST_IP:]HOST[:GUEST][/tcp                                                                 | udp]` (repeatable; HOST_IP defaults to `127.0.0.1`) |
| `--cpus`               | Virtual CPUs (default: half the host's)                                                      |
| `--memory`             | e.g. `8G` (default: half the host's)                                                         |
| `--home-mount`         | `rw` (default), `ro`, `none`                                                                 |
| `--network`            | Container network name; `none` for no network                                                |
| `--ssh` / `--no-ssh`   | Forward the host SSH agent socket (default: on)                                              |
| `--sudo` / `--no-sudo` | Provision passwordless sudo/doas (default: on)                                               |
| `--restricted`         | Defaults preset: `--home-mount none --network none --no-ssh --no-sudo` (alias `--untrusted`) |
| `--no-boot`            | Create without booting                                                                       |
| `--set-default`        | Make this the default distro (automatic when nothing is)                                     |

`--automount` resolves once at create: volumes attached later need
`set --add-volume`, and an ejected volume makes `start` fail
(`path does not exist`) until dropped with `set --rm-volume`.

`--restricted` is a **defaults preset, not a sandbox**: explicit flags
still apply, `set` can reopen anything, and `run --root`/`exec` as `0:0`
always work. See [security.md](security.md).

### `run [-n NAME] [OPTIONS] [-- CMD...]`

Run a command or interactive shell in a distro, booting it if needed.
Uses `container exec`, so argv is exact — nothing is re-split.

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
MOUNTS, PORTS, and a `*` in DEFAULT. Orphaned filesystems from an
interrupted `set` appear as pseudo-rows with state `recreating` (a
recovery is in flight) or `interrupted` (waiting for one).

### `inspect NAME...`

Dump the distros' `container` configuration as JSON.

### `start NAME` / `stop NAME...`

Boot a stopped distro / stop running ones.

### `delete` (alias `rm`) `-f|--force NAME...`

Delete distros and their storage; `-f` stops them first. Also deletes
staged `set` state when the container itself is already gone.

### `set NAME [OPTIONS]`

Change settings after creation. Recreates the container while keeping
its filesystem: the ext4 rootfs is cloned to a staging area and the
recreated container boots from it via `rootFsOverride` — no image
round-trip. A per-distro lock serializes `set` against boot/delete.

| Option               | Description                                                                                                    |
| -------------------- | -------------------------------------------------------------------------------------------------------------- |
| `--cpus`             | Virtual CPUs                                                                                                   |
| `--memory`           | e.g. `8G`                                                                                                      |
| `--home-mount`       | `rw`/`ro`/`none`                                                                                               |
| `--network`          | Network name; `none` disables, `default` restores                                                              |
| `--ssh`/`--no-ssh`   | SSH-agent forwarding                                                                                           |
| `--sudo`/`--no-sudo` | Sudo/doas provisioning — `--no-sudo` stops *future* grants; sudoers/doas files already inside the guest remain |
| `--add-volume`       | `SRC:DST[:ro]` add or replace a mount by guest path                                                            |
| `--rm-volume`        | `DST` remove the mount at that guest path                                                                      |
| `--publish`          | Add a published port (replaces one on the same host port)                                                      |
| `--unpublish`        | `HOST_PORT` stop publishing that host port                                                                     |

A **never-booted** distro has no `rootfs.ext4` yet, so `set` recreates
it from its recorded image instead.

#### Interrupted `set` recovery

If a `set` is killed mid-recreate, the staged filesystem heals itself:
the next `set`, `run`/`exec`, or `start` finishes or rolls back the
recovery; `list` marks the orphan `recreating`/`interrupted`; `rm`
discards it; `create`/`import` of the same name refuse until it's
resolved.

### `set-default [NAME] | --clear`

Set the default distro — or unset it with `--clear`. The default distro
overrides the default machine in `cm`.

### `export NAME [-o FILE]`

Write the distro's root filesystem as a tar (stdout by default).
Fails on never-booted distros — there is no `rootfs.ext4` yet.

### `import NAME FILE [OPTIONS]`

Create a distro from a rootfs `tar`/`tar.gz` (`-` = stdin); accepts all
`create` options including `--restricted`. Importing an image is
trusting it with your identity — see [security.md](security.md).

### `migrate MACHINE [-n NAME] [--keep] [OPTIONS]`

Turn a `container machine` into a distro without an export/import
round-trip: the machine's `rootfs.ext4` is staged through the same
machinery as `set`, then recreated as a distro carrying the machine's
cpus/memory/home-mount/user. The machine is removed unless `--keep`;
accepts `create` options as overrides. Distro → machine is
[planned](TODO.md#distros-container-distro) but not implemented.

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
Default location: `share/man/man1` of the binary's install prefix —
`$CARGO_HOME/share/man/man1` after `cargo install`. See
[install.md](install.md#man-pages).

### `uninstall-man [--dir DIR]`

Remove the pages `install-man` wrote. Only files that still look like
our generated pages are removed — a foreign or edited page of the same
name is left in place with a warning.

### `uninstall-plugin [--plugin-dir DIR]`

Remove the plugin registration (only if it was installed by
`container-distro`).

## Mount and publish spec syntax

- **Mount**: `SRC:DST[:ro]` — absolute host path, absolute guest path,
  optional `ro`.
- **Publish**: `[HOST_IP:]HOST_PORT[:GUEST_PORT][/tcp|udp]` — `3000`,
  `8080:80`, `0.0.0.0:8080:80/udp`. HOST_IP defaults to `127.0.0.1`;
  guest port defaults to the host port; protocol to `tcp`.

Keep publishes on loopback unless you want LAN-reachable services — a
`0.0.0.0` bind changes exposure for *other* machines too.
