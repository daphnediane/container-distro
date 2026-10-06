# `cm` usage

`cm` is a [`wsl.exe`-compatible](https://github.com/microsoft/WSL)
command line over Apple's [`container`](https://github.com/apple/container):
`wsl` argument semantics, backed by `container machine` *and* by
`container distro` distros as one namespace. If you alias or symlink it
to `wsl`, WSL-shaped tooling drives it unchanged.

```text
cm [OPTIONS] [-- <COMMAND LINE>]
```

With no arguments, `cm` opens a login shell in the default distro or
machine. If `container` services aren't running, `cm` runs
`container system start` first — matching WSL's transparent start.

## Machines and distros are one namespace

`-d NAME` resolves a **distro** first, then a machine — a name that
exists as both shadows the machine (and `cm -l` warns). The default is
likewise: a default distro wins over the default machine, and
`cm -s <machine>` clears any default distro.

Distros are created by `cm --install --distro` (implied by `--share`,
`--publish`, or `--restricted`) or by `container distro create`. See
[container-distro.md](container-distro.md).

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

- `cm -e cmd args…` and `cm --shell-type none cmd…` deliver each argument
  **exactly** — nothing is re-split or re-quoted.
- `cm cmd…` and `cm -- cmd…` join the line and evaluate it through the
  guest shell, so `cm -- echo '$HOME'` expands in the guest.

For machines, exact argv can't go through `container machine run` (it
re-evaluates through a shell —
[apple/container#1954](https://github.com/apple/container/issues/1954)),
so `cm -e` execs `container exec` in the machine's backing container
instead (your provisioned user, same starting directory `machine run`
would pick). If the machine can't be exec'd into, `cm` falls back to
`machine run` with each argument single-quoted. Distros always use
`container exec`, so `-e` and `--` are both argv-exact there.

### Working directory

When the host working directory is under a path shared with the guest
(`$HOME` for machines; any shared mount for distros), the session starts
at the same path in the guest — the WSL `/mnt/c`-style mapping. Outside
a shared path it starts in the guest home (`/home/<name>`). `--cd` takes
a *guest* path; host paths only work if the same path is shared.

### stdio and TTY

Piped stdin is forwarded (`cm` always passes `-i`). Interactive shells
need a TTY — `machine run` requires one, and `cm` execs it for full
terminal passthrough. Known upstream wart: `machine run` can drop the
guest's very first write; see [gaps/exec-stdio](gaps/exec-stdio.md).

## Listing

| Option             | Description                                                                        |
| ------------------ | ---------------------------------------------------------------------------------- |
| `-l, --list`       | Names under a header, `(Default)` marked                                           |
| `-l -q, --quiet`   | Bare names only                                                                    |
| `-l -v, --verbose` | WSL's exact `NAME STATE VERSION` table (`VERSION` is always `2`)                   |
| `-l -v -v`         | Plus `container`-specific columns incl. `KIND` (`machine`/`distro`), IP, resources |
| `-l --all`         | Accepted for WSL compatibility; `-l` already lists stopped entries                 |
| `-l --running`     | Only running machines and distros                                                  |

Listing covers both kinds; an `interrupted`/`recreating` distro row means
a `distro set` was cut off mid-recreate — see
[container-distro.md](container-distro.md#interrupted-set-recovery).

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

`--unregister` and `-t` resolve distros first, like `-d` — so they act
on a distro when a same-named machine exists.

## Installing (`--install`)

```text
cm --install IMAGE [--name NAME] [--no-launch]
                  [--cpus N] [--memory SIZE] [--home-mount rw|ro|none]
                  [--distro] [--restricted]
                  [--share SRC:DST[:ro]]... [--publish SPEC]...
```

Without distro options, `--install` runs `container machine create` then
opens a shell in the new machine (`--no-launch` skips the shell). With
`--distro` — or implied by `--share`/`--publish`/`--restricted` — it
creates a distro instead; those three options have no machine
equivalent. `--restricted` (alias `--untrusted`) is a defaults preset
for semi-trusted images: no home share, no network, no SSH-agent
forward, no sudo — explicit flags still apply, and it is *not* a
sandbox (see [security.md](security.md)).

## Port forwarding

`cm --forward HOST[:GUEST]` (repeatable, `-d` picks the target) runs a
foreground forwarder: `127.0.0.1:HOST` on the Mac → `GUEST` on the
machine/distro's IP, until Ctrl-C. **Experimental** — for distros,
prefer real port publishing (`--publish` at install, or
`container distro set --publish`/`--unpublish`); see
[gaps/port-forwarding](gaps/port-forwarding.md).

## Man page and `wsl` alias

| Option                          | Description                                                                                                                                                        |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `--install-man[=DIR]`           | Install `cm(1)` (generated from the CLI definition); bare, targets `share/man/man1` of the binary's install prefix — pass `=DIR` when `cm` isn't under a `bin` dir |
| `--uninstall-man[=DIR]`         | Remove `cm(1)` again — only files still looking like our generated pages                                                                                           |
| `--install-completions[=DIR]`   | Install bash/zsh/fish completions (generated from the CLI definition); bare, targets `share/` of the binary's install prefix                                       |
| `--uninstall-completions[=DIR]` | Remove the completions again — only files still looking like generated scripts                                                                                     |
| `--install-alias NAME\|PATH`    | Symlink `NAME`/`PATH` to `cm` (e.g. `wsl`), plus `NAME(1)` → `cm.1` when the man page is installed                                                                 |
| `--uninstall-alias NAME\|PATH`  | Remove an alias — only a symlink that actually resolves to this `cm`, and its man-page link                                                                        |

See [install.md](install.md#man-pages) for where the defaults land.
`--install-alias` is idempotent: re-running it after
`cm --install-man` adds just the missing man-page link.

## Environment

| Variable        | Effect                                                                                  |
| --------------- | --------------------------------------------------------------------------------------- |
| `CM_BACKEND`    | `machine` → ignore distros                                                              |
| `CONTAINER_CLI` | Path/name for the `container` binary (default: `/usr/local/bin/container`, then `PATH`) |
| `SSH_AUTH_SOCK` | Forwarded into guests by the machine plugin / distro init                               |
| `HOME`          | Source of the shared home mount and the cwd mapping                                     |

`cm` forwards the child process's exit code (or 255-clamped status).
