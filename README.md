# container-distro (`cm`)

A [WSL](https://github.com/microsoft/WSL)-compatible command line for
Apple's [`container`](https://github.com/apple/container), in two parts:

- **`cm`** — `wsl.exe`-style commands over
  [container machines](https://github.com/apple/container/blob/main/docs/container-machine.md)
  _and_ distros.
- **`container distro`** — a `container` plugin (and library) that
  creates **distros**: machine-like containers (your account provisioned
  inside, home shared at the same path, the image's init as PID 1) plus
  what machines lack — host directories outside `$HOME`, published
  ports, export/import, and changing settings after creation. See
  [Distros](#distros).

With no arguments, `cm` opens a login shell in the default machine (or
the default distro, if one is set):

```bash
cm                # container machine run
```

## Install

```bash
cargo install --path crates/cm
# optionally alias to `wsl`:
ln -s "$(which cm)" ~/.local/bin/wsl   # or any dir on PATH

# optional: the `container distro` subcommand (cm doesn't need it)
cargo install --path crates/container-distro
sudo container-distro install-plugin   # -> /usr/local/libexec/container-plugins/distro
```

## Usage

```text
cm [OPTIONS] [-- <COMMAND LINE>]
```

### Run commands and shells

| Option               | Description                                           |
| -------------------- | ----------------------------------------------------- |
| `-d, --distribution` | Machine or distro to use (default if omitted)         |
| `-u, --user`         | Run as the specified user                             |
| `--cd`               | Working directory inside the machine                  |
| `--shell-type`       | `standard` (non-login), `login` (default), `none`     |
| `-e, --exec`         | Execute the command line without a shell (exact argv) |
| `-- <cmd>`           | Run the remaining command line via the shell          |
| `--env KEY=VALUE`    | Set an environment variable in the machine            |

### Manage machines

| Option              | Description                                                                             |
| ------------------- | --------------------------------------------------------------------------------------- |
| `-l, --list`        | List machines and distros (`--all`, `--running`, `-q`, `-v`, `-v -v`)                   |
| `-s, --set-default` | Set the default machine or distro                                                       |
| `-t, --terminate`   | Stop a running machine or distro                                                        |
| `--shutdown`        | Stop all running machines and distros (`--system`: also stop services)                  |
| `--status`          | Show container system status                                                            |
| `--unregister`      | Delete a machine or distro and its storage                                              |
| `--install <image>` | Create + boot a machine (`--name`, `--no-launch`, `--cpus`, `--memory`, `--home-mount`) |
|                     | …or a distro with `--distro`, `--share SRC:DST[:ro]`, `--publish [IP:]HOST[:GUEST]`     |
| `--forward <p[:g]>` | Forward localhost port `p` to port `g` of the machine or distro                         |
| `--version`         | Show `cm` and `container` versions                                                      |

If `container` services aren't running, `cm` runs `container system start`
first.

## Examples

```bash
cm -d alpine                      # shell in the alpine machine
cm -e uname -a                    # run a command
cm -- ls -la                      # verbatim passthrough
cm -d alpine --cd /tmp -e pwd     # command with a working dir
cm -l                             # list machines (WSL style)
cm -l -v                          # NAME STATE VERSION table
cm -l -v -v                       # ...plus IP, resources, image
echo hi | cm -- cat               # stdin is piped through
cm -t alpine                      # stop it
cm --forward 3000                 # localhost:3000 -> machine:3000
cm --install alpine:latest --name dev   # create and launch a machine
cm --install ubuntu:24.04 --name work --share /Volumes/Code:/mnt/code --publish 3000
                                  # ...a distro with an extra mount and a port
```

## Distros

`container machine` can't share anything outside `$HOME` or publish
ports, and its settings are fixed at creation. A **distro** is a regular
`container` container configured the way the machine plugin configures a
machine:

- the host account is provisioned inside (same name/uid/gid,
  passwordless sudo), with `SSH_AUTH_SOCK` forwarded
- `$HOME` is shared at the same path (`--home-mount rw|ro|none`)
- the image's own init (`/sbin/init`) runs as PID 1 — or an idle PID 1
  for images without one
- all capabilities, no masked paths

On top of that:

| Feature                | How                                                                                               |
| ---------------------- | ------------------------------------------------------------------------------------------------- |
| Mounts outside `$HOME` | `-v /Volumes/Code:/mnt/code[:ro]`; `--automount` maps every `/Volumes/<X>` to `/mnt/<x>`          |
| Published ports        | `-p 3000`, `-p 8080:80`, `-p 0.0.0.0:8080:80/udp` (host IP defaults to `127.0.0.1`)               |
| Change settings later  | `container distro set NAME --cpus 4 --add-volume … --publish …` (recreates, keeps the filesystem) |
| Export / import        | `container distro export NAME -o f.tar`, `container distro import NAME f.tar[.gz]`                |
| Exact argv             | `run` uses `container exec`, so arguments are never re-split                                      |

`cm` sees distros and machines as one set of WSL distributions:
`cm -d NAME` resolves a distro first, then a machine. A default distro
(`cm -s NAME`) wins over the default machine; setting a machine as
default clears it. When the cwd is under a shared path, sessions start
at its guest path (`/Volumes/Code/x` → `/mnt/code/x`). `cm` links the
distro library directly, so the plugin install is optional.
`CM_BACKEND=machine` makes `cm` ignore distros.

```bash
container distro create -n dev -v /Volumes/Code:/mnt/code -p 3000 alpine:latest
container distro run -n dev -- uname -a
container distro ls
container distro set dev --memory 8G --unpublish 3000
container distro rm -f dev
```

Distros are tracked with `io.github.daphnediane.container-distro.*`
labels (see [doc/naming.md](doc/naming.md)). Regular containers get one
extra vCPU of overhead (`nproc` shows `cpus + 1`).

## Differences from WSL

`cm` mirrors WSL's command line, but the underlying `container machine`
model differs in a few important ways.

### Filesystem sharing

WSL mounts every Windows drive under `/mnt/<letter>` (e.g. `C:\` →
`/mnt/c`). Container machines take a different approach: your macOS home
directory is shared into the guest via virtiofs **at the same absolute
path** — `/Users/<name>` on the Mac is `/Users/<name>` in the machine.

The flip side is that **a machine shares nothing outside `$HOME`** (use a
[distro](#distros) for that). There is no
`/mnt/...` equivalent and, as of `container` 1.5.0, `container machine`
has no option for additional mounts — the only configurable share is the
home mount (`rw` by default; `ro` or `none` via
`container machine create --home-mount` or
`container machine set -n <m> home-mount=<mode>` plus a restart).
Directories on other volumes (e.g. `/Volumes/...`) or elsewhere outside
your home directory are unreachable from inside the machine. Pick the
home mode at creation with `cm --install <image> --home-mount ro`, or
change it later with `container machine set`.

Also note the guest's `~` is `/home/<name>` — a pure Linux home on the
machine's persistent disk — distinct from your macOS home at
`/Users/<name>`.

### Starting directory

WSL starts a session in the `/mnt/c/...` mapping of your Windows working
directory. `container machine run` does the equivalent automatically:
when the host working directory is under `$HOME`, the guest process
starts at the same path; anywhere else it starts in the guest's
`/home/<name>`. `cm` inherits this behavior — no flag needed.

`cm --cd <dir>` maps to `container`'s `-w` and expects a guest path.
Host paths under `$HOME` work verbatim since the mount is at the same
path; host paths outside `$HOME` do not exist in the guest.

### Networking

WSL2 forwards guest ports to Windows localhost. A container machine gets
its own IP on the `machine` network (shown by `cm -l`); services in the
guest are reachable at that IP only — nothing is bridged to macOS
localhost automatically. `cm --forward 3000` (or `--forward 8080:80`,
repeatable, with `-d` to pick the machine) runs a foreground forwarder
from `127.0.0.1` on the Mac to the machine until you press Ctrl-C.

### Command execution

As with WSL, `cm -e cmd args…` delivers each argument exactly, while
`cm cmd…` and `cm -- cmd…` run the command line through the guest shell
(so `cm -- echo '$HOME'` expands in the guest). `container machine run`
always shell-evaluates its arguments
([apple/container#1954](https://github.com/apple/container/issues/1954)),
so `cm -e` bypasses it: it runs the command with `container exec` in the
machine's backing container (as your user, starting in the same
directory `machine run` would). If that isn't possible, `cm` falls back
to `machine run` with each argument single-quoted. Shells and `--`
commands still use `machine run`. Piped stdin is forwarded (`cm` always
passes `-i`).

Known upstream issue: `container machine run` can drop the guest's very
first write to stdout/stderr — see
[doc/gaps/exec-stdio.md](doc/gaps/exec-stdio.md).

### Interop and user mapping

WSL can execute Windows binaries from Linux via binfmt interop; there is
no macOS-binary interop in a container machine. On the other hand, the
guest account is auto-provisioned to match your macOS username/uid/gid
(with passwordless sudo) and `SSH_AUTH_SOCK` is forwarded into the guest,
so `git`/`ssh` operations use your host agent with no extra setup.

### Management gaps

- `cm -l -v` prints WSL's `NAME STATE VERSION` table; VERSION is always
  `2` (machines and distros are full VMs). `-v -v` adds
  `container`-specific columns including KIND (`machine`/`distro`).
- Machine names must be lowercase DNS-style (`[a-z0-9-]`); WSL distro
  names are unrestricted.
- `--shutdown` stops machines but leaves `container` services running;
  `--shutdown --system` also runs `container system stop` (which stops
  every container, not just machines).
- `cm --export`/`--import` are not implemented: machine export is
  blocked upstream (see [doc/gaps/export-import.md](doc/gaps/export-import.md)).
  Distros support it via `container distro export`/`import`.
- No equivalents for `--update`, `--manage`, `--mount` (VHDs), or
  `wsl.conf`.

## Notes

- `container machine run` requires a TTY for interactive shells (as does `cm`).
- `container` is resolved from `PATH`; set `CONTAINER_CLI` to override.

## License

[BSD-2-Clause License]

## AI Coding Declaration

Development of this project has been assisted by AI coding tools:

- [Devin]
- [Claude Code]

Most of the code and documentation here was AI-generated and manually
reviewed. Take the documentation as a slightly out-of-date roadmap, no
matter the polish of the AI verbiage. Here be dragons.

[BSD-2-Clause License]: LICENSE
[Devin]: https://www.devin.ai/
[Claude Code]: https://claude.ai/code
