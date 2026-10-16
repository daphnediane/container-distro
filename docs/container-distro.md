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
- `$HOME` can be shared at the same path (`--home-mount rw|ro|none`);
  the default depends on where the distro comes from — `rw` for
  catalog installs, `none` for ad-hoc `create`/`import` (see
  [security.md](security.md))
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
Journal files are `uchg`-locked at rest (design/security.md T6).

Creating, installing, or importing a distro also generates **terminal
profiles** — the `windowsterminal` metadata equivalent from a `.wsl`
package's `wsl-distribution.conf`. The artifacts, all under the state
dir and all launching `container distro run -n NAME`:

- `terminal-cmds/NAME.sh` — a launcher script (`exec
  '<container-distro>' run -n NAME -w '~' "$@"`, naming the binary
  that wrote it — `container-distro` or the plugin copy;
  `NAME.sh CMD…` runs CMD as exact argv, like `run`). Imported profiles point at the *script*, so command
  changes propagate without re-importing. Profiles reach it through
  `~/.container-distro`, a symlink to the state dir, as the run
  command `~/.container-distro/terminal-cmds/NAME.sh` — Terminal
  expands `~` but whitespace-tokenizes `CommandString` rather than
  shell-parsing it, so the tilde stays space-free even where a
  resolved `Application Support` path would truncate however it was
  quoted. (iTerm2's Command is shell-parsed and falls back to the
  quoted real path if the alias can't be made.) The script, the
  `.terminal` files, and the symlink are
  `uchg`-locked like the journal assets — a guest with a rw-shared
  home can't rewrite them (design/security.md);
- `terminal-profiles/NAME.terminal` — the settings-set document.
  Opening it — `container distro terminal NAME`, `cm --terminal`, or
  a double-click — makes Terminal *import* the profile through its
  own model (surviving the prefs flushes that drop profiles written
  behind a running Terminal's back). The `distro terminal`/`cm
  --terminal` commands also guarantee a window running the distro:
  `open` launches one when Terminal is responsive, and when a dormant
  Terminal queues or drops it the commands spawn the window directly
  (adopting the imported profile for the title and close-on-exit).
  The window closes itself when the shell exits cleanly
  (`shellExitAction`); a failed launch stays open so the error is
  readable;
- when iTerm2 is installed, a dynamic profile under
  `~/Library/Application Support/iTerm2/DynamicProfiles/container-distro.json`,
  which iTerm2 loads live.

Generation is on by default and skipped when the packaged conf sets
`windowsterminal.enabled = false`; an explicit `--terminal-profile` /
`--no-terminal-profile` on `create`, `import`, `catalog install`,
`migrate --in`, or `set` overrides the package's answer and is
journaled as your pin (`set --reset terminal-profile` un-pins it).
`rename` refreshes the artifacts, and `rm`/`delete --all` and
`migrate --out` remove them — including imported Terminal profiles —
all best-effort, never failing the distro operation.

Generation also *imports* the profile — silently: straight into
`Window Settings` when Terminal isn't running, and when it is, via a
momentary `open` of a self-replacing no-op command (the window
flashes open and closes itself as the real command is swapped in —
that's the import, not a bug). Routine refreshes (`set` of other
options) only rewrite the launcher artifacts, so deleting the
profile in Terminal's settings is honored — nothing *automatically*
re-adds it. Convergent operations do re-import, by design:
`set --terminal-profile`, `sync-profiles`, `rename`, and an explicit
open. Imported profiles carry a `ContainerDistroDistro` marker so
removal only touches what we made, and a foreign profile holding the
distro's name is never overwritten (Terminal itself renames a
colliding import to `NAME 1`; a `defaults` write uses `NAME (cm)`).

## Subcommands

### `create [-n NAME] [OPTIONS] IMAGE`

Create a distro from an image and boot it. Name defaults to a derivation
from the image.

| Option                                         | Description                                                                                                                                                                                                                                                                                                             |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `-n, --name`                                   | Distro name (lowercase DNS-style)                                                                                                                                                                                                                                                                                       |
| `-v, --volume`                                 | `SRC:DST[:opt,...]` share a host directory (repeatable); opts: `ro`/`rw` plus guest mount flags `nosuid`,`nodev`,`noexec`,`noatime`,`nodiratime`,`relatime`,`sync`,`dirsync`                                                                                                                                            |
| `--automount[=MODE]`                           | Share every `/Volumes/<X>` at `/mnt/<x>`; `MODE` is `rw` (default) or `ro` (skips hidden/`com.apple.*`/unreadable volumes). Shares get `nosuid,nodev`; with no `--home-mount`, home follows `MODE` (warned)                                                                                                             |
| `-p, --publish`                                | `[HOST_IP:]HOST[:GUEST][/tcp\|udp]` (repeatable; HOST_IP defaults to `127.0.0.1`)                                                                                                                                                                                                                                       |
| `--cpus`                                       | Virtual CPUs (default: half the host's)                                                                                                                                                                                                                                                                                 |
| `--memory`                                     | e.g. `8G` (default: half the host's)                                                                                                                                                                                                                                                                                    |
| `--arch`                                       | `arm64`/`amd64` — the guest arch (default: the host's). `amd64` selects x86-64 sources and runs translated under Rosetta on Apple silicon. Journaled — `set` can't change it                                                                                                                                            |
| `--home-mount`                                 | `rw`/`ro`/`none` — default `none` for `create`/`import` (`rw` via `catalog install`); the *effective* `--automount` mode is used if unset                                                                                                                                                                               |
| `--network`                                    | Container network name; `none` for no network                                                                                                                                                                                                                                                                           |
| `--ssh` / `--no-ssh`                           | Forward the host SSH agent socket (default: on only when the effective home-mount is `rw`)                                                                                                                                                                                                                              |
| `--sudo` / `--no-sudo`                         | Provision passwordless sudo/doas (default: on)                                                                                                                                                                                                                                                                          |
| `--preset`                                     | The defaults tier: `default`, `restricted`, or `trusted` — journaled so `set --preset` can re-derive it later                                                                                                                                                                                                           |
| `--files` / `--no-files`                       | Record host file-access intent for the planned `container distro fs` (default: on, off under `--restricted`)                                                                                                                                                                                                            |
| `--restricted`                                 | `--preset restricted`: `--home-mount none --network none --no-ssh --no-sudo --no-files` (alias `--untrusted`)                                                                                                                                                                                                           |
| `--trusted`                                    | `--preset trusted`: `--home-mount rw --automount rw --ssh`                                                                                                                                                                                                                                                              |
| `--use-wsl-conf`                               | bare flag = `all`, or `FEATURE,...`/`all`/`none` — honor the guest's `/etc/wsl.conf`: `hostname`, `fstab`, `init` (systemd/openrc), `command` ([boot] command, as root), `automount` (lower mounts for `enabled=false`). A bare list pins exactly those; `all` tracks new features as they're added                     |
| `--no-wsl-conf`                                | Honor nothing from `/etc/wsl.conf` (the same as `--use-wsl-conf=none`)                                                                                                                                                                                                                                                  |
| `--ignore-wsl-conf`                            | Deny features — with no `--use-wsl-conf`, leaves `all` minus the list                                                                                                                                                                                                                                                   |
| `--no-boot`                                    | Create without booting                                                                                                                                                                                                                                                                                                  |
| `--set-default`                                | Make this the default distro (automatic when nothing is)                                                                                                                                                                                                                                                                |
| `--terminal-profile` / `--no-terminal-profile` | Generate the `.terminal` launcher + iTerm2 dynamic profile (default: on, unless the package's `wsl-distribution.conf` sets `windowsterminal.enabled = false`) — journaled as an explicit pin (also honored by `import`, `catalog install`, and `migrate --in`). Open the launcher with `container distro terminal NAME` |
| `--force`                                      | Allow the name of an existing `container machine` — see below                                                                                                                                                                                                                                                           |

Creating a distro named after an existing `container machine` is
**refused** without `--force`: `cm` resolves a shared name to the
distro, silently shadowing the machine everywhere `cm` operates. The
same refusal applies to `import`, `catalog install`, `migrate`, and
`rename` — and in reverse to `cm --install --machine` /
`cm --import --machine` when a distro holds the name. `--force` makes
the collision deliberate (it still warns).

`--automount` is live, not create-once: automounts are computed at
every materialization (create, `set`, every start) from what's
actually attached — the journal only stores *your* mounts, so
attaching a volume adds it and ejecting one drops it with no
`set --add-volume`/`--rm-volume` chore. A user mount wins a target
collision against a computed automount, and disabling automount
never removes a mount you added yourself — even one that looks like
`/Volumes/<X>` → `/mnt/<x>`. `set --reset mounts` clears all user
mounts.

`--restricted` is a **defaults preset, not a sandbox**: explicit flags
still apply, `set` can reopen anything, and `run --root`/`exec` as `0:0`
always work. See security.md.

`/etc/wsl.conf` honoring is a **distro-only** feature (machines never
read it): init reads the guest's own file at every boot for the enabled
features — WSL's restart-to-apply model — and `container cp` snapshots
both `wsl.conf` and `wsl-distribution.conf` into the uchg-locked journal
at boot and before every stop. `none` (the default for ad-hoc creates)
means the file is never opened and nothing is snapshotted; `trusted`
and catalog installs ask for `all`. `command` is the dangerous feature —
it runs image-supplied shell as root — so it lands only where the image
was already trusted to run code. See
[design/notes/configuration-files.md] for the per-key support matrix.

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

| Option        | Description                                 |
| ------------- | ------------------------------------------- |
| `-q, --quiet` | Names only                                  |
| `--running`   | Only running distros                        |
| `--arch`      | Only distros of that arch (`arm64`/`amd64`) |
| `--format`    | `table` (default) or `json`                 |

The table shows NAME, IMAGE, CREATED, STATE, IP, CPUS, MEMORY, DISK,
MOUNTS, PORTS, and a `*` in DEFAULT. MOUNTS lists your mounts plus a
`~`/`~(ro)` token for the home share and `automounts`/`automounts(ro)`
when enabled. A CPUS/MEMORY you pinned shows plainly (`4`); a value
the host will pick at boot shows bracketed (`[8]`); `-` means nothing
is recorded. Stopped distros list from their
journals — CREATED is the distro's real creation time (the journal's
`origin`), not the container's. For distros created from a file or
catalog import, IMAGE shows the origin instead of the generated
`local/distro-*` snapshot reference (`file:<basename>`, `catalog:<name>`;
`--format json` carries the full `origin` object). In JSON, `mounts`
is user mounts only, `automounts` the computed auto-shares (home
included), `effectiveMounts` the merged set handed to `container
create`, and `preset` the recorded tier.

### `catalog`

The installable-distro catalog is the same compiled-in list
`cm -l -o` uses — Microsoft's vendored `DistributionInfo.json`
(`vendor/microsoft/`, MIT-licensed) merged with our `catalog.json`
overlay, which adds entries and `Image`/`HomeMount` fields.
`create` remains the explicit image path and
`import` remains the explicit local-file path; `catalog` resolves
friendly catalog names and `.wsl` downloads.

| Command                                                  | Description                                                                                                                                                                                                                         |
| -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `catalog list [-v ...] [--catalog FILE] [--arch ARCH]`   | `NAME`/`FRIENDLY NAME`; `-v` adds local state and instances (machines and distros), repeated `-v` adds the install source and image ref or download URL. `--arch` renders for that arch — `amd64` reveals amd64-only `.wsl` entries |
| `catalog info DISTRO [--catalog FILE] [--format FORMAT]` | Show the entry's catalog fields plus cache, image, and existing distro state (`FORMAT` is `table` or `json`)                                                                                                                        |
| `catalog install [DISTRO] [OPTIONS]`                     | Install a catalog entry (`catalog list` shows names); bare installs the default entry                                                                                                                                               |
| `catalog cache [--catalog FILE]`                         | Show cached `.wsl` downloads, their catalog entry, and the image they were imported as                                                                                                                                              |
| `catalog purge`                                          | Delete cached `.wsl` downloads (imported distros/images are unaffected)                                                                                                                                                             |

`catalog install` accepts every `create` option (`-n`, mounts, ports,
resources, `--restricted`, `--no-boot`, `--set-default`, `--arch`, …).
`--arch amd64` resolves the entry's x86-64 source — its `Amd64Url`
`.wsl` when it has one (amd64-only entries become installable), else
the `linux/amd64` image — and the distro runs translated under Rosetta;
init masks `systemd-binfmt.service` on translated guests, since its
boot-time `binfmt_misc` flush would drop the Rosetta `x86_64`
registration mid-boot. `catalog install` only
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

Boot a stopped distro / stop running ones. An `automount` distro's
`/Volumes` shares are recomputed at every boot (and the boot a `run`
triggers) from what's actually attached.

### `delete` (alias `rm`) `-f|--force [--all] NAME...`

Delete distros and their storage; `-f` stops them first. Deletes
journal state even when the container itself is already gone.
`--all` deletes every distro — journaled ones (stopped distros exist
only there) and labeled containers alike — and is what the `.pkg`
uninstaller's `-d` path invokes before removing the binaries.

### `set NAME [OPTIONS]`

Change settings after creation: stop, update the journal, start — the
same stop/update/start cycle a hand-edited restart would follow, so an
interrupted `set` and a deliberate stop are the same on-disk state.
The journaled `rootfs.ext4` reattaches via `rootFsOverride` — no image
round-trip. The journal's `spec.json` is the per-distro flock target,
serializing `set` against start/delete.

| Option                                         | Description                                                                                                                                                                                                                     |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--cpus`                                       | Virtual CPUs                                                                                                                                                                                                                    |
| `--memory`                                     | e.g. `8G`                                                                                                                                                                                                                       |
| `--home-mount`                                 | `rw`/`ro`/`none` — replaces the create-time default                                                                                                                                                                             |
| `--network`                                    | Network name; `none` disables, `default` restores                                                                                                                                                                               |
| `--ssh`/`--no-ssh`                             | SSH-agent forwarding                                                                                                                                                                                                            |
| `--sudo`/`--no-sudo`                           | Sudo/doas provisioning -- `--no-sudo` stops *future* grants; sudoers/doas files already inside the guest remain                                                                                                                 |
| `--automount[=rw\|ro\|none]`/`--no-automount`  | Auto-manage `/Volumes` mounts at `/mnt/<name>` — computed at each start from what's attached                                                                                                                                    |
| `--preset`                                     | `default`/`restricted`/`trusted` — change the distro's tier, re-deriving every option you haven't pinned                                                                                                                        |
| `--restricted`/`--trusted`                     | `--preset restricted`/`--preset trusted` (alias `--untrusted`)                                                                                                                                                                  |
| `--reset`                                      | Un-pin an option back to derived (`--reset home-mount`, `--reset terminal-profile`), clear a list (`--reset mounts`, `--reset publish`), or reset every pin to a tier (`--reset restricted` — also sets the preset); repeatable |
| `--terminal-profile` / `--no-terminal-profile` | Generate/remove the `.terminal` launcher + iTerm2 dynamic profile — re-pinning on also refreshes a launcher whose imported profile you deleted (re-import it with `container distro terminal NAME`)                             |
| `--files` / `--no-files`                       | Record/drop host file-access intent for the planned `container distro fs` — on by default except under `--restricted`                                                                                                           |
| `--use-wsl-conf`                               | bare flag = `all`, or `FEATURE,...`/`all`/`none` — fold features into the honored set (`use` allows; `all`/`none` reset the mode; see design/notes/configuration-files.md)                                                      |
| `--no-wsl-conf`                                | Honor nothing — drops the snapshots                                                                                                                                                                                             |
| `--ignore-wsl-conf`                            | Deny features from what the stored mode honors — `all` is honoring's off switch                                                                                                                                                 |
| `--add-volume`                                 | `SRC:DST[:opt,...]` add or replace a mount by guest path (same option set as `-v`)                                                                                                                                              |
| `--rm-volume`                                  | `DST` remove the mount at that guest path                                                                                                                                                                                       |
| `--publish`                                    | Add a published port (replaces one on the same host port)                                                                                                                                                                       |
| `--unpublish`                                  | `HOST_PORT` stop publishing that host port                                                                                                                                                                                      |

Options are tracked as **explicit pins** versus **derived** values: a
flag you pass on `create` or `set` is pinned and survives `set
--preset` re-derivation; everything else follows the tier (and chains
like home-mount ← automount). `set` reports what the tier change
re-derived and what's still pinned. Catalog installs also record the
entry's curated request — it fills options nothing else sets and
caps what a tier would raise, but your pins always win.

A **never-booted** distro has no `rootfs.ext4` yet — the disk
materializes on first boot, so `set` just rewrites the journal.

#### Interrupted `set` recovery

There is no distinct interrupted state: killing a `set` mid-flight
leaves the same journal-plus-disk rest state a clean stop does. The
next operation converges it — any leftover container is dismantled and
a fresh one materializes from the journal.

### `sync-profiles`

Converge every journaled distro's terminal-profile artifacts to its
recorded intent — the upgrade/repair path for distros created before
terminal-profile support existed, launchers lost to a fresh macOS
install, or iTerm2 installed after the distros. This re-imports
missing Terminal profiles (silently where possible — a momentary
self-closing window while Terminal runs), so a profile you deleted
by hand comes back; `set NAME --no-terminal-profile` is the opt-out.
Honors each distro's opt-out and removes launchers and imported
profiles whose distro no longer exists.

### `terminal NAME`

Open the distro in a new Terminal.app window: refreshes its
`.terminal` launcher and `open`s it — Terminal imports the settings
set (a no-op if already imported under that name) — then opens a
window running `container distro run -n NAME`, spawning it directly when a dormant
Terminal queues the `open`'s launch. This is also how you re-import a
profile you deleted, and `cm --terminal` does the same for the
`-d`/default distro.
Asking to open is an opt-in: it pins `terminal_profile` like
`set --terminal-profile` would, so `sync-profiles` keeps the
artifacts rather than sweeping them.

### `set-default [NAME] | --clear`

Set the default distro -- or unset it with `--clear`. The default distro
overrides the default machine in `cm`.

### `export NAME [-o FILE]`

Write the distro's root filesystem as a tar (stdout by default). Fails
on never-booted distros -- there is no `rootfs.ext4` yet.

### `import NAME FILE [OPTIONS]`

Create a distro from a rootfs `tar`/`tar.gz` (`-` = stdin) — a `.wsl`
distribution package works too (it's a rootfs tar; its
`wsl-distribution.conf` supplies `oobe.defaultName` when no name is
given and, where the distro honors its config, both packaged config
files are snapshotted into the journal and `automount.enabled=false`
lowers the mount defaults). Accepts all `create`
options including `--restricted`. `--arch` declares the archive's
architecture (default: the host's) — `--arch amd64` labels the
imported image `linux/amd64` so `container` runs it under Rosetta.
Importing an image is trusting it
with your identity -- see security.md.

The converted image is content-keyed (`local/rootfs:<sha256>`): a
repeat import of the same archive reuses it rather than re-running
the OCI wrap and `image load`, and each distro/machine gets its own
`local/distro-<name>:imported-<sha>` tag on it, which `rm`/
`--unregister` untags. Unused shared tags are evicted oldest-first
past a size budget — reclaim them sooner with `container image
delete` or `image prune`.

### `rename OLD NEW [--force]`

Rename a distro: the journal — `spec.json`, `rootfs.ext4`, and
`sbin.distro/` — moves to `NEW`, the default marker follows, and a
running distro restarts under the new name. The runtime container is
never renamed; it's dismantled before the move and materialized fresh
after, so nothing `container`-side ever carries the old name. `NEW`
must be free of distros, staged state, and — unless `--force` —
machines (the same shadow rule `create` enforces). An imported
distro's `local/distro-OLD` image tag keeps the old name,
so `rm` won't untag it automatically afterward.

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
machine forwards `SSH_AUTH_SOCK` at every boot with no opt-out, so
migrating a distro whose resolved agent setting is off warns — and
refuses outright when the distro has it explicitly pinned off
(`set --ssh` it first). A
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

### `cli [CM OPTIONS] [-- COMMAND LINE]`

Run the WSL-compatible `cm` interface from this binary: `container
distro cli -d NAME` is `cm -d NAME`, every flag included (see
[cm.md](cm.md)). `cli --install-alias NAME` makes a `cm`-style alias
of this binary, so a plugin-only install still gets one.

### `version`

Print the `container-distro` version, then `container --version` —
the same report as `cm --version`.

### `install-all` / `uninstall-all [--prefix DIR] [--alias NAME]... [--no-plugin | --plugin-dir DIR]`

Install (or remove) everything around an installed binary in one
step — run it after `cargo install container-distro`:

- man pages and shell completions for both interfaces
  (`container-distro*` and `cm`);
- the `cm` alias script, plus each `--alias NAME` (for example `wsl`)
  as another `cm`-style alias, each with a `NAME(1)` link to `cm.1`;
  a `cm` *binary* from an older release is replaced (it's recognized
  by our embedded label prefix, never run);
- the `container distro` plugin registration (`install-plugin`,
  elevating when the plugin dir needs root) unless `--no-plugin`.

`--prefix` defaults to the prefix of the running binary; files go to
`<prefix>/bin`, `<prefix>/share/man/man1`, and `<prefix>/share`.
`install-all --stage DIR` (packaging) writes that layout under `DIR`
while the aliases still run `<prefix>/bin/container-distro`. Every step
runs even when one fails, and the command then fails, naming the
failed steps. `uninstall-all` removes only what is provably ours; run
it before `cargo uninstall`.

### `install-alias NAME|PATH` / `uninstall-alias NAME|PATH`

Write (or remove) an alias script running `container-distro`, plus a
`NAME(1)` → `container-distro.1` man-page link when that page is
installed. Aliases are `/bin/sh` scripts that name this binary
(canonicalized) and record which interface they run; a bare `NAME`
lands next to the alias you ran the command through, else next to the
binary — the plugin copy (`<plugins>/distro/bin/distro`) needs an
explicit `PATH`. Removal only touches one of our alias scripts and its
man-page symlink. For a `cm`-style alias, use `cli --install-alias`.

### `update [VERSION] [--force] [--dry-run]`

Update container-distro itself — the same self-update `cm --update`
runs (`update-container-distro.sh` in the pkg's `bin/` wraps this
command). Fetches the latest GitHub release — or an explicit `VERSION`
(`0.5.0`/`v0.5.0`) — prefers the signed
`container-distro-<ver>.pkg`, verifies the release-reported SHA-256
plus a Developer ID signature and a signing-team match against the
installed copy, then `exec`s `open` so Installer.app owns the
privileged install. Same-version, downgrades, the unsigned CI package,
a different signing team, and non-pkg installs ask first; `--force`
skips the asking; running as root is refused; `--dry-run` resolves
and verifies without installing. (The global `--dev` flag gates the
`CONTAINER_DISTRO_RELEASES_URL` fixture-server override used for
testing — DEV.md.) Nothing needs stopping first.

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
[naming.md]: design/naming.md
[design/notes/configuration-files.md]: design/notes/configuration-files.md
