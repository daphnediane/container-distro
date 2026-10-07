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

## Machines and distros are one namespace

`-d NAME` resolves a **distro** first, then a machine -- a name that
exists as both shadows the machine (and `cm -l` warns). The default is
likewise: a default distro wins over the default machine, and
`cm -s <machine>` clears any default distro.

Distros are created by `cm --install --distro` (implied by `--share`,
`--publish`, or `--restricted`) or by `container distro create`. See
container-distro.md.

`CM_BACKEND=machine` makes `cm` ignore distros entirely.

## Running commands and shells

| Option               | Description                                                   |
| -------------------- | ------------------------------------------------------------- |
| `-d, --distribution` | Machine or distro to use (default if omitted)                 |
| `-u, --user`         | Run as the specified user (name, not uid)                     |
| `--cd`               | Working directory inside the guest                            |
| `--shell-type`       | `standard` (non-login), `login` (default), `none`             |
| `-e, --exec`         | Execute the rest of the line without a shell (exact argv)     |
| `-- <cmd>`           | Run the remaining command line via the guest shell            |
| bare positional args | Same as `--`: run through the guest shell                     |
| `--env KEY=VALUE`    | Set an env var in the guest (`KEY` alone inherits the host's) |

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
guest's very first write; see [gaps/exec-stdio].

## Listing

| Option             | Description                                                                        |
| ------------------ | ---------------------------------------------------------------------------------- |
| `-l, --list`       | Names under a header, `(Default)` marked                                           |
| `-l -q, --quiet`   | Bare names only                                                                    |
| `-l -v, --verbose` | WSL's exact `NAME STATE VERSION` table (`VERSION` is always `2`)                   |
| `-l -v -v`         | Plus `container`-specific columns incl. `KIND` (`machine`/`distro`), IP, resources |
| `-l --all`         | Accepted for WSL compatibility; `-l` already lists stopped entries                 |
| `-l --running`     | Only running machines and distros                                                  |
| `-l -o, --online`  | The installable-distro catalog (`NAME FRIENDLY NAME`) instead of installed ones    |
| `-l --cache`       | The `.wsl` download cache: SHA-256, size, catalog entry, and the image it loaded   |
| `--purge-cache`    | Empty the `.wsl` download cache                                                    |

Listing covers both kinds; an `interrupted`/`recreating` distro row
means a `distro set` was cut off mid-recreate -- see
[container-distro.md].

## Managing

| Option                | Description                                                          |
| --------------------- | -------------------------------------------------------------------- |
| `-s, --set-default`   | Set the default distro or machine                                    |
| `-t, --terminate`     | Stop a running distro or machine                                     |
| `--shutdown`          | Stop all running machines and distros; services stay up              |
| `--shutdown --system` | Also `container system stop` (stops *all* containers, not just ours) |
| `--status`            | `container system status`                                            |
| `--unregister`        | Delete a distro or machine and its storage                           |
| `--version`           | Print `cm` and `container` versions                                  |

`--unregister` and `-t` resolve distros first, like `-d` -- so they act
on a distro when a same-named machine exists.

## Installing (`--install`)

```text
cm --install [DISTRO|IMAGE] [--name NAME] [--no-launch]
                  [--cpus N] [--memory SIZE] [--home-mount rw|ro|none]
                  [--distro] [--restricted]
                  [--share SRC:DST[:ro]]... [--publish SPEC]...
cm --install --from-image IMAGE ...
cm --install --from-file ROOTFS.tar|.wsl ...
cm --catalog FILE --install DISTRO | -l -o
```

The positional argument is resolved against the curated catalog first
(case-insensitive; `cm -l -o` lists it) and otherwise passed through as
an image reference — catalog names contain no `/`/`:`/`@`, so a
fully-qualified ref always bypasses the catalog, and `--from-image`
skips name resolution entirely. A bare `cm --install` installs the
catalog default, Ubuntu, matching `wsl --install`.

The catalog is a compiled-in list (`crates/cm-core/src/catalog.json`)
whose schema is Microsoft's `DistributionInfo.json` — the file behind
`wsl --list --online`, in the `microsoft/WSL` repo — extended with an
`Image` field. Each `ModernDistributions` vendor group holds entries
with a `Name`, `FriendlyName`, and either `Image` (an OCI reference;
installs can create a machine or distro) or per-arch `Amd64Url` /
`Arm64Url` `.wsl` downloads (`{"Url", "Sha256"}`; the rootfs is
fetched, verified, and imported as a distro). Downloads are cached
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
for the host arch are hidden from `-l -o` and refused at install,
which is why amd64-only `.wsl` distributions (SLES, eLxr) don't appear
on Apple Silicon. `--catalog FILE` substitutes a custom file in the
same schema — Microsoft's own manifest works verbatim. It is an
explicit per-invocation override, never auto-loaded from `$HOME`,
since anything under `$HOME` is guest-writable and a default search
path would let a distro rewrite familiar names to hostile images (see
security.md). `container distro` itself never consults the catalog; it
takes image refs only.

`--from-file` imports a local rootfs tar or a `.wsl` distribution
package (WSL 2.4.4+ tar format) as a **distro** — machines can't boot a
bare rootfs, so it implies `--distro`. The name comes from `--name`,
then the package's `etc/wsl-distribution.conf` `oobe.defaultName`, then
the file stem. Other manifest keys are ignored: distros provision the
host user rather than honoring `oobe.defaultUid`, and `oobe.command`
never runs (security.md T4).

Without distro options, `--install` runs `container machine create` then
opens a shell in the new machine (`--no-launch` skips the shell). With
`--distro` -- or implied by
`--share`/`--publish`/`--restricted`/`--from-file` -- it creates a
distro instead; those options have no machine equivalent.
`--restricted` (alias `--untrusted`) is a defaults preset for
semi-trusted images: no home share, no network, no SSH-agent forward,
no sudo -- explicit flags still apply, and it is *not* a sandbox (see
security.md).

## Exporting and importing

| Option                     | Description                                                                              |
| -------------------------- | ---------------------------------------------------------------------------------------- |
| `--export NAME FILE`       | Write the distro's or machine's root filesystem as a tar (`-` = stdout)                  |
| `--import NAME [LOC] FILE` | Register a rootfs `tar`/`tar.gz` as a machine (`-` = stdin; `--distro` imports a distro) |

`--export` briefly stops a running machine while its filesystem is
snapshotted, then restarts it -- `container export` can't read a
machine's rootfs directly, so `cm` exports a clone (see
[gaps/export-import]).

`--import` registers the tar as a machine -- or as a distro with
`--distro` (`container distro import`). `LOC` is WSL's
install-location argument: accepted for compatibility and ignored,
since storage is managed by `container`; the two-value form
`--import NAME FILE` omits it. Names are lowercase DNS-style, as
`machine create` requires. Like `wsl --import`, the result is left
stopped; `cm` boots it once to verify and retries the documented
first-boot flake before warning.

## Port forwarding

`cm --forward HOST[:GUEST]` (repeatable, `-d` picks the target) runs a
foreground forwarder: `127.0.0.1:HOST` on the Mac → `GUEST` on the
machine/distro's IP, until Ctrl-C. **Experimental** -- for distros,
prefer real port publishing (`--publish` at install, or
`container distro set --publish`/`--unpublish`); see
[gaps/port-forwarding].

## Man page and `wsl` alias

| Option                          | Description                                                                                                                                                         |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--install-man[=DIR]`           | Install `cm(1)` (generated from the CLI definition); bare, targets `share/man/man1` of the binary's install prefix -- pass `=DIR` when `cm` isn't under a `bin` dir |
| `--uninstall-man[=DIR]`         | Remove `cm(1)` again -- only files still looking like our generated pages                                                                                           |
| `--install-completions[=DIR]`   | Install bash/zsh/fish completions (generated from the CLI definition); bare, targets `share/` of the binary's install prefix                                        |
| `--uninstall-completions[=DIR]` | Remove the completions again -- only files still looking like generated scripts                                                                                     |
| `--install-alias NAME\|PATH`    | Symlink `NAME`/`PATH` to `cm` (e.g. `wsl`), plus `NAME(1)` → `cm.1` when the man page is installed                                                                  |
| `--uninstall-alias NAME\|PATH`  | Remove an alias -- only a symlink that actually resolves to this `cm`, and its man-page link                                                                        |

See [install.md] for where the defaults land. `--install-alias` is
idempotent: re-running it after `cm --install-man` adds just the missing
man-page link.

## Environment

| Variable        | Effect                                                                                  |
| --------------- | --------------------------------------------------------------------------------------- |
| `CM_BACKEND`    | `machine` → ignore distros                                                              |
| `CONTAINER_CLI` | Path/name for the `container` binary (default: `/usr/local/bin/container`, then `PATH`) |
| `SSH_AUTH_SOCK` | Forwarded into guests by the machine plugin / distro init                               |
| `HOME`          | Source of the shared home mount and the cwd mapping                                     |

`cm` forwards the child process's exit code (or 255-clamped status).

[`container`]: https://github.com/apple/container
[`wsl.exe`-compatible]: https://github.com/microsoft/WSL
[apple/container#1954]: https://github.com/apple/container/issues/1954
[container-distro.md]: container-distro.md#interrupted-set-recovery
[gaps/exec-stdio]: gaps/exec-stdio.md
[gaps/export-import]: gaps/export-import.md
[gaps/port-forwarding]: gaps/port-forwarding.md
[install.md]: install.md#man-pages
