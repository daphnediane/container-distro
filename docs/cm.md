# `cm` usage

`cm` is a [`wsl.exe`-compatible] command line over Apple's
[`container`]: `wsl` argument semantics, backed by `container machine`
*and* by `container distro` distros as one namespace. If you alias or
symlink it to `wsl`, WSL-shaped tooling drives it unchanged.

```text
cm [OPTIONS] [-- <COMMAND LINE>]
```

With no arguments, `cm` opens a login shell in the default distro or
machine. If `container` services aren't running, `cm` runs
`container system start` first -- matching WSL's transparent start.
When `container` itself isn't installed, an interactive `cm` offers to
download the signed installer for the release this build is verified
against and open it in Installer.app (the download is verified against
a SHA-256 pinned in the binary; `CONTAINER_DISTRO_NO_INSTALL_OFFER=1`
disables the offer). Non-interactive runs get the install instructions
as an error instead.

`cm` is an alias script for `container-distro cli`, installed by the
pkg or `container-distro install-all`; anything below works as
`container-distro cli …` (or `container distro cli …`) too.

## Machines and distros are one namespace

`-d NAME` resolves a **distro** first, then a machine -- a name that
exists as both shadows the machine *for operations* (and `cm -l`
warns). Listings never hide it though: `cm -l` shows both rows,
suffixing the ambiguous name with `(machine)` / `(distro)`. The
default is likewise operational: a recorded distro wins over the
default machine, `cm -s <machine>` records `machine:<name>` and sets
`container`'s own default, and an unset or stale record converges --
`machine:` follows `container`'s default machine, and a removed
default repoints to the best remaining distro (like WSL moving the
`*`) before it would leave nothing.

Because a shared name shadows, **creating** one is refused:
`--install`, `--import`, and `--rename` bail when the name is taken in
the other backend, as do `container distro create`/`import`/
`catalog install`/`migrate`/`rename`. `--force` permits the collision
deliberately (and still warns).

Distros are the default: `cm --install` and `cm --import` build them
unless `--machine` is given. `container distro create` makes them
directly. See container-distro.md.

`--distro` and `--machine` are backend selectors, not just creation
flags: they apply anywhere `cm` resolves a distro-or-machine name
(`-d`, `-t`, `-s`, `--unregister`, `--export`, `--forward`,
`--install`, `--import`, `--migrate`). Bare `cm --distro` targets the
default distro, `cm --machine` the default machine; an explicit
selector never falls through to the other kind — `-d x --distro` on a
machine-only name is an error, and `-d x --machine` picks the machine
even when a same-named distro exists.

`CM_BACKEND=machine` makes `cm` ignore distros entirely.

## Running commands and shells

| Option               | Description                                                   |
| -------------------- | ------------------------------------------------------------- |
| `-d, --distribution` | Machine or distro to use (default if omitted)                 |
| `-u, --user`         | Run as the specified user (name, not uid)                     |
| `--cd`               | Working directory inside the guest (`~`/`~/x` = guest home)   |
| `--shell-type`       | `standard` (non-login), `login` (default), `none`             |
| `-e, --exec`         | Execute the rest of the line without a shell (exact argv)     |
| `-- <cmd>`           | Run the remaining command line via the guest shell            |
| bare positional args | Same as `--`: run through the guest shell                     |
| `--env KEY=VALUE`    | Set an env var in the guest (`KEY` alone inherits the host's) |
| `--terminal`         | Open the distro in a Terminal.app window instead              |

`--terminal` opens the distro's `.terminal` launcher — Terminal
imports the profile (first time only) and runs the distro in a new
window, so it's distro-only and the run-shaping flags don't apply
(`--user`, `--cd`, `--shell-type`, `--env`, `-e`, a command line).
On `--install`/`--import` it replaces the console launch entirely.
Asking to open is an opt-in — it pins `--terminal-profile` on the
distro's journal. The first run asks macOS's permission for the app
you ran `cm` from to control Terminal; see [Terminal automation
permission](container-distro.md#terminal-automation-permission) if it
fails with `-1743`.

`--shell-type none` requires a command (it's the exec mode implied by
`-e`); `--shell-type standard`/`login` choose the interactive shell.

### Shell vs. exec semantics (WSL rules)

- `cm -e cmd args…` and `cm --shell-type none cmd…` deliver each
  argument **exactly** -- nothing is re-split or re-quoted.
- `cm cmd…` and `cm -- cmd…` join the line and evaluate it through the
  guest shell, so `cm -- echo '$HOME'` expands in the guest.

For machines, exact argv can't go through `container machine run` (it
re-evaluates through a shell -- [apple/container#1954]), so `cm -e`
execs `container exec` in the machine's backing container instead (your
provisioned user, same starting directory `machine run` would pick). If
the machine can't be exec'd into, `cm` falls back to `machine run` with
each argument single-quoted. Distros always use `container exec`, so
`-e` and `--` are both argv-exact there.

### Working directory

When the host working directory is under a path shared with the guest
(`$HOME` for machines; any shared mount for distros), the session starts
at the same path in the guest -- the WSL `/mnt/c`-style mapping. Outside
a shared path it starts in the guest home (`/home/<name>`). `--cd` takes
a *guest* path; host paths only work if the same path is shared.

### stdio and TTY

Piped stdin is forwarded (`cm` always passes `-i`). Interactive shells
need a TTY -- `machine run` requires one, and `cm` execs it for full
terminal passthrough. Known upstream wart: `machine run` can drop the
guest's very first write; see [exec-stdio].

## Listing

| Option             | Description                                                                                       |
| ------------------ | ------------------------------------------------------------------------------------------------- |
| `-l, --list`       | Names under a header, `(Default)` marked; same-name machine+distro pairs show both, kind-suffixed |
| `-l -q, --quiet`   | Bare names only (conflicted names keep the `(machine)`/`(distro)` suffix)                         |
| `-l -v, --verbose` | WSL's exact `NAME STATE VERSION` table (`VERSION` is always `2`)                                  |
| `-l -v -v`         | Plus `container`-specific columns incl. `KIND` (`machine`/`distro`), IP, resources                |
| `-l --all`         | Accepted for WSL compatibility; `-l` already lists stopped entries                                |
| `-l --running`     | Only running machines and distros                                                                 |
| `-l --arch ARCH`   | Only machines and distros of that arch; `-l -o --arch` lists that arch's catalog                  |
| `-l -o, --online`  | The installable-distro catalog (`NAME FRIENDLY NAME`) instead of installed ones                   |
| `-l -o -v`         | Plus `LOCAL`/`INSTANCES` status columns; `-v -v` adds `SOURCE`/`REF` (image ref or download URL)  |
| `-l --cache`       | The `.wsl` download cache: SHA-256, size, catalog entry, and the image it loaded                  |
| `--purge-cache`    | Empty the `.wsl` download cache                                                                   |

Listing covers both kinds. A stopped distro is just its journal — the
row has no CREATED time because there is no container; starting
materializes a fresh one (see [container-distro.md]).

## Managing

| Option                | Description                                                                                                               |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `-s, --set-default`   | Set the default distro or machine                                                                                         |
| `-t, --terminate`     | Stop a running distro or machine (the effective default when no name is given)                                            |
| `--shutdown`          | Stop all running machines and distros; also stops `container` services when `cm` started them and nothing is left running |
| `--shutdown --system` | Always `container system stop` (stops *all* containers, not just ours)                                                    |
| `--status`            | `container system status`                                                                                                 |
| `--update [VERSION]`  | Update container-distro itself via the release `.pkg` + Installer.app                                                     |
| `--unregister`        | Delete a distro or machine and its storage                                                                                |
| `--rename OLD NEW`    | Rename a machine or distro; `--machine`/`--distro` selects on a clash                                                     |
| `--migrate NAME`      | Make NAME an instance of the selected backend (see below)                                                                 |
| `--version`           | Print `cm` and `container` versions (`not installed` when missing)                                                        |

`--unregister` and `-t` resolve distros first, like `-d` -- so they act
on a distro when a same-named machine exists.

`--rename OLD NEW` moves a distro's journal (spec, disk, init assets)
to the new name and restarts it under it; a machine is recreated
(`container machine` has no rename) with its rootfs cloned across,
boot-verified, and the old machine removed -- a default machine or
distro keeps its default. `NEW` must be free in both backends unless
`--force` opts into the shadow.

`--migrate` converges NAME onto the selected backend: a machine
becomes a distro (carrying its rootfs; the machine is removed), a
pre-journal distro container is adopted -- its live config becomes the
journal, the only opt-in past the one-time migration -- and a journaled
distro is already home. `cm --migrate --machine NAME` goes the other
way: the distro's disk carries into a new `container machine`.

`--update [VERSION]` is the `wsl --update` analog: it updates
container-distro itself (not machines or distros, and not `container`
-- that has Apple's own `update-container.sh`). It fetches the latest
GitHub release, or an explicit `VERSION` (`0.5.0` or `v0.5.0`),
prefers the signed `.pkg`, verifies the release-reported SHA-256 plus
a Developer ID signature and a signing-team match against the
installed copy, then `exec`s `open` so Installer.app owns the
privileged install. Same-version, downgrades, the unsigned CI package,
a different signing team, and non-pkg installs all ask first --
`--force` skips the asking. `--dry-run` resolves, downloads, and
verifies the package, then reports instead of installing. Nothing
needs stopping first.
`update-container-distro.sh` (installed to `/usr/local/bin` by the
pkg) is the same update as a script.

## Installing (`--install`)

```text
cm --install [DISTRO|IMAGE] [--name NAME] [--no-launch] [--terminal]
                  [--arch arm64|amd64]
                  [--cpus N] [--memory SIZE] [--home-mount rw|ro|none]
                  [--preset default|restricted|trusted] [--ssh|--no-ssh]
                  [--terminal-profile|--no-terminal-profile]
                  [--machine] [--restricted|--trusted] [--force]
                  [--share SRC:DST[:opt,...]]... [--publish SPEC]...
cm --install --from-image IMAGE ...
cm --install --from-file ROOTFS.tar|.wsl ...
cm --catalog FILE --install DISTRO | -l -o
```

The positional argument is resolved against the curated catalog first
(case-insensitive; `cm -l -o` lists it) and otherwise passed through as
an image reference — catalog names contain no `/`/`:`/`@`, so a
fully-qualified ref always bypasses the catalog, and `--from-image`
skips name resolution entirely. A bare `cm --install` installs the
catalog default, Ubuntu, matching `wsl --install`. Like
`wsl --install <Distro>`, the optional positional binds only directly
after the flag — `cm --install --no-launch Ubuntu` leaves `Ubuntu`
dangling as a command line — so put it first or write
`--install=Ubuntu`.

The created distro is named `--name` if given, else the catalog
entry's NAME sanitized to a lowercase DNS-style name: `wsl --install`
registers `Ubuntu`, and cm registers `ubuntu`. An install from a bare
image reference (or `--from-image`) derives the name from the image
instead (`alpine:latest` → `alpine-latest`).

The catalog is compiled in and merged from two files: a pinned,
verbatim copy of Microsoft's `DistributionInfo.json` — the catalog
behind `wsl --list --online`, MIT-licensed in `vendor/microsoft/` —
and a small overlay (`src/catalog/catalog.json`) that adds entries Microsoft
doesn't publish (Alpine, Rocky, Oracle, UBI) and fills extension
fields on entries it does. The schema is Microsoft WSL's plus our
`Image`/`HomeMount` extensions. Each `ModernDistributions` vendor
group holds entries with a `Name`, `FriendlyName`, an optional `Image`
(an OCI reference) and per-arch `Amd64Url` / `Arm64Url` `.wsl`
downloads (`{"Url", "Sha256"}`; the rootfs is fetched, verified, and
imported as a distro). The `.wsl` download wins when one exists for
the selected arch — a verified, content-addressed fetch — and `Image` is
the fallback for archs the download doesn't cover (`archlinux` has no
upstream arm64 `.wsl`, so Apple Silicon installs its image). An
optional `HomeMount` field (`rw`/`ro`/`none`) pins the entry's
home-share default. Downloads are cached
content-addressed (`<sha256>.wsl`) under the Darwin per-user cache
directory (`cm -l --cache` lists it, `cm --purge-cache` empties it),
so a second install skips the fetch; the OS may reclaim the cache
under storage pressure. The imported image is labeled with the source
URL and rootfs SHA-256, which is what links a cached file to its image
in `--list --cache`. Concurrent installs and purges are serialized by
per-file flocks (`<sha256>.lock`), the same pattern as distro locks —
a second fetch waits rather than re-downloading, and purge waits for
an in-flight import. The lockfile doubles as a sidecar: it records the
source URL and catalog NAME as JSON, so `--list --cache` identifies a
file even after the catalog points that entry at a newer SHA-256 (such
entries show as `NAME (superseded)`). Entries with no source
for the selected arch are hidden from `-l -o` and refused at install —
amd64-only `.wsl` distributions (SLES, eLxr) appear once `--arch amd64`
selects the x86-64 catalog. `--catalog FILE` substitutes a custom file in the
same schema, replacing the built-in merge. It is an
explicit per-invocation override, never auto-loaded from `$HOME`,
since anything under `$HOME` is guest-writable and a default search
path would let a distro rewrite familiar names to hostile images (see
design/security.md). The same catalog is available to the plugin through
`container distro catalog`; `container distro create` itself still
takes image refs only.

`--arch amd64` installs for x86-64 instead of the host's arm64:
catalog entries resolve their `Amd64Url` `.wsl` (amd64-only entries
like SLES become installable), image-backed entries fetch the
`linux/amd64` variant, and `container` auto-enables Rosetta so the
amd64 userspace runs translated on the arm64 kernel — this is userland
translation, not a full x86 VM, so no AVX. One wrinkle:
`systemd-binfmt.service` flushes all `binfmt_misc` registrations at
boot, which would drop the Rosetta `x86_64` entry mid-boot, so init
masks it (a `/dev/null` symlink, what `systemctl mask` writes) whenever
the Rosetta registration is armed. `--arch` also applies to
`--import` (the imported rootfs is labeled `linux/amd64`, so
`container` treats it as an amd64 image) and to `-l`: `-l -o
--arch amd64` lists the amd64 catalog, and `-l --arch amd64` filters
installed machines and distros down to amd64 ones (`container distro
list`/`catalog list` take the same flag).

`cm -l -o -v` annotates the catalog with local state: `LOCAL` is
`pulled` for an image already in `container image list`, `cached` for
a `.wsl` whose hash sits verified in the download cache, and
`downloading` while another process's fetch holds the entry's
lockfile; `INSTANCES` lists the machines and distros built from the
entry (`name (running)`), traced through image references and the
`rootfs-sha256` label. When a name exists as both a machine and a
distro both instances are shown, suffixed `(machine)` / `(distro)` —
the listing reports, it doesn't resolve. The probes are best-effort
and never block — cache state is read without flocking and `container`
is queried only when already running — so a download in another
process can't stall the listing.

`--from-file` imports a local rootfs tar or a `.wsl` distribution
package (WSL 2.4.4+ tar format) as a **distro** — machines can't boot a
bare rootfs, so it's rejected with `--machine`. The name comes from
`--name`, then the package's `etc/wsl-distribution.conf`
`oobe.defaultName`, then the catalog NAME for catalog downloads, then
the file stem. Other manifest keys are
ignored: distros provision the host user rather than honoring
`oobe.defaultUid`, and `oobe.command` never runs (design/security.md
T4). Both packaged config files are snapshotted into the journal, and
where the distro honors `wsl-conf` (catalog installs ask for `all`;
see `--use-wsl-conf`), `wsl.conf`'s `automount.enabled=false` lowers
the mount defaults at create.

`--install` creates a **distro** (`container distro create`), then
opens a shell in it (`--no-launch` skips the shell). There is no
first-run setup: `wsl --install` launches an OOBE prompt asking for a
new Unix username and password, while cm distros and machines
provision your macOS account automatically (same name/uid/gid), so a
fresh install drops straight into a shell. `--machine` runs
`container machine create` instead — for WSL-parity testing and
upstream comparisons — and is rejected alongside the distro-only
options `--preset`/`--share`/`--publish`/`--restricted`/`--from-file`/
`--use-wsl-conf`,
which have no machine equivalent (`--ignore-wsl-conf` and
`--no-wsl-conf` are allowed — machines already behave that way). A machine forwards your SSH agent at
every boot (upstream has no opt-out), so `--machine` itself is the
opt-in: an install whose resolved agent setting is off just warns, and
only an explicit `--no-ssh` pin is refused — `--ssh`, `--trusted`, or
`--home-mount rw` all count as opting in.

`--install` (and `container distro create`/`import`) generates
**terminal profiles** for the distro — WSL's `windowsterminal` metadata
made macOS-native: a `.terminal` launcher file plus, when iTerm2 is
installed, a dynamic profile, both launching `container distro run -n NAME`. Generation is
on by default and yields to a package opt-out (a `.wsl` whose
`wsl-distribution.conf` sets `windowsterminal.enabled = false`);
`--terminal-profile`/`--no-terminal-profile` pin the answer explicitly
on the distro's journal (`--terminal-profile` is distro-only — machines
never get profiles; `--no-terminal-profile` is a harmless no-op there).
The Terminal profile is imported at generation time — silently into
prefs when Terminal isn't running, or via a momentary self-closing
window when it is — and `cm --terminal` (with `-d`, the default distro,
or on `--install`/`--import` in place of the console launch) opens a
window from it, as does `container distro terminal NAME`. Deleting the
imported profile in Terminal's settings is honored by routine
refreshes; `sync-profiles` and `set --terminal-profile` re-import it
(that's their convergent job). See container-distro.md.

`--trusted` is the mirror preset of `--restricted`: rw home, rw
`/Volumes` automount, and the SSH agent — for images you'd give your
own login session. Explicit flags still override it. `--restricted` (alias `--untrusted`) is a
defaults preset for semi-trusted images: no home share, no network, no
SSH-agent forward, no sudo -- explicit flags still apply, and it is
*not* a sandbox (see security.md). `--preset TIER` names the same
three tiers; whichever selects it is journaled on the distro, so
`container distro set --preset` can re-derive it later — flags you
passed stay pinned, everything else follows the tier.

The home share follows trust: a name that resolves to a catalog entry
defaults to `--home-mount rw` (resolving through a catalog is treated
as an act of trust — including a `--catalog FILE`, which implies trust
in that manifest); a bare image ref, `--from-image`, or `--from-file`
defaults to `none`. An explicit `--home-mount` always wins. The
equivalent on `container distro create`/`import` also lets the
effective `--automount` mode set the home default (with a warning).

## Exporting and importing

| Option                     | Description                                                                               |
| -------------------------- | ----------------------------------------------------------------------------------------- |
| `--export NAME FILE`       | Write the distro's or machine's root filesystem as a tar (`-` = stdout)                   |
| `--import NAME [LOC] FILE` | Register a rootfs `tar`/`tar.gz` as a distro (`-` = stdin; `--machine` imports a machine) |

`--export` briefly stops a running machine while its filesystem is
snapshotted, then restarts it -- `container export` can't read a
machine's rootfs directly, so `cm` exports a clone (see
[export-import]).

`--import` registers the tar as a distro (`container distro import`) --
or as a machine with `--machine`. `LOC` is WSL's
install-location argument: accepted for compatibility and ignored,
since storage is managed by `container`; the two-value form
`--import NAME FILE` omits it. Names are lowercase DNS-style, as
`machine create` requires. Like `wsl --import`, the result is left
stopped; `cm` boots it once to verify and retries the documented
first-boot flake before warning. `--arch` declares the rootfs's
architecture (default: the host's) — `--arch amd64` labels the imported
image `linux/amd64` so `container` runs it under Rosetta.

The converted image is keyed on the rootfs's SHA-256
(`local/rootfs:<sha256>`): importing the same archive again — another
distro, a `--machine` import, a retry — reuses it instead of
re-converting, which usually took longer than the download did. Each
import adds a per-name tag (`local/distro-<name>:imported-<sha>` /
`local/machine-<name>:imported-<sha>`) that `rm`/`--unregister` untags;
shared `local/rootfs:*` tags no importer still references are evicted
oldest-first past a size budget, like the `.wsl` download cache.

## Port forwarding

`cm --forward HOST[:GUEST]` (repeatable, `-d` picks the target) runs a
foreground forwarder: `127.0.0.1:HOST` on the Mac → `GUEST` on the
machine/distro's IP, until Ctrl-C. **Experimental** -- for distros,
prefer real port publishing (`--publish` at install, or
`container distro set --publish`/`--unpublish`); see
[port-forwarding].

## Man page and `wsl` alias

| Option | Description |
| ------ | ----------- |
| `--install-man[=DIR]` | Install `cm(1)` (generated from the CLI definition); bare targets `share/man/man1` of the install prefix -- `=DIR` when `cm` isn't under `bin/` |
| `--uninstall-man[=DIR]` | Remove `cm(1)` again -- only files still looking like our generated pages |
| `--install-completions[=DIR]` | Install bash/zsh/fish completions (generated from the CLI definition); bare targets `share/` of the install prefix |
| `--uninstall-completions[=DIR]` | Remove the completions again -- only files still looking like generated scripts |
| `--install-alias NAME\|PATH` | Write an alias script `NAME`/`PATH` running `cm` (e.g. `wsl`); adds `NAME(1)` -> `cm.1` when the man page is installed |
| `--uninstall-alias NAME\|PATH` | Remove an alias -- only one of our alias scripts (or a legacy symlink), and its man-page link |

See [install.md] for where the defaults land. `--install-alias` is
idempotent: re-running it after `cm --install-man` adds just the missing
man-page link, and refreshes an alias whose binary moved.

An alias is a two-line `/bin/sh` script, not a symlink:
`exec '<binary>' --invoked-as "$0" [cli] "$@"`. The script names the
binary that wrote it (canonicalized) and which interface it runs, so
`container-distro cli --install-alias wsl` makes a `cm`-style alias of
`container-distro` too. A bare `NAME` lands next to the alias you ran
the command through — `wsl --install-alias wsl2` puts `wsl2` beside
`wsl` — else next to the binary. Aliases made by older releases
(symlinks to the old `cm` binary) are upgraded to scripts by
`container-distro install-all --alias NAME` or a re-run of
`--install-alias`.

`cm -v` on its own prints the version, as `wsl -v` does; with `--list`
it is `--verbose`.

## Environment

| Variable                        | Effect                                                                                                                  |
| ------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `CM_BACKEND`                    | `machine` -> ignore distros                                                                                             |
| `CONTAINER_CLI`                 | `container` binary override -- a non-default path/name needs `--dev` (default: `/usr/local/bin/container`, then `PATH`) |
| `CONTAINER_DISTRO_RELEASES_URL` | Release-API override for `--update` -- honored only with `--dev` (see DEV.md fixtures)                                  |
| `SSH_AUTH_SOCK`                 | Forwarded into guests by the machine plugin / distro init                                                               |
| `HOME`                          | Source of the shared home mount and the cwd mapping                                                                     |

`--dev` is the opt-in for the test seams in that table — it exists so a
variable planted in a shell profile can't redirect a privileged
install or the `container` binary. `CONTAINER_CLI` values equal to the
defaults (`container`, `/usr/local/bin/container`) never needed it —
they can't redirect anything.

`cm` forwards the child process's exit code (or 255-clamped status).

[`container`]: https://github.com/apple/container
[`wsl.exe`-compatible]: https://github.com/microsoft/WSL
[apple/container#1954]: https://github.com/apple/container/issues/1954
[container-distro.md]: container-distro.md#interrupted-set-recovery
[exec-stdio]: design/upstream-limitations.md#machine-run-exec-fidelity-stdin-argv-first-write
[export-import]: design/upstream-limitations.md#container-export-cant-snapshot-a-machine-or-distro-rootfs
[port-forwarding]: design/upstream-limitations.md#no-localhost-port-forwarding-for-machines
[install.md]: install.md#man-pages
