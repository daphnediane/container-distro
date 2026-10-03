# container-distro (`cm`)

A [WSL](https://github.com/microsoft/WSL)-compatible command-line wrapper for
[Apple container machines](https://github.com/apple/container/blob/main/docs/container-machine.md).

With no arguments, `cm` opens a login shell in the default container machine:

```bash
cm                # container machine run
```

## Install

```bash
cargo install --path crates/cm
# optionally alias to `wsl`:
ln -s "$(which cm)" ~/.local/bin/wsl   # or any dir on PATH
```

## Usage

```text
cm [OPTIONS] [-- <COMMAND LINE>]
```

### Run commands and shells

| Option               | Description                                           |
| -------------------- | ----------------------------------------------------- |
| `-d, --distribution` | Machine to use (default machine if omitted)           |
| `-u, --user`         | Run as the specified user                             |
| `--cd`               | Working directory inside the machine                  |
| `--shell-type`       | `standard` (non-login), `login` (default), `none`     |
| `-e, --exec`         | Execute the command line without a shell (exact argv) |
| `-- <cmd>`           | Run the remaining command line via the shell          |
| `--env KEY=VALUE`    | Set an environment variable in the machine            |

### Manage machines

| Option              | Description                                                                             |
| ------------------- | --------------------------------------------------------------------------------------- |
| `-l, --list`        | List machines (`--all`, `--running`, `-q`, `-v`, `-v -v`)                               |
| `-s, --set-default` | Set the default machine                                                                 |
| `-t, --terminate`   | Stop a running machine                                                                  |
| `--shutdown`        | Stop all running machines (`--system`: also stop services)                              |
| `--status`          | Show container system status                                                            |
| `--unregister`      | Delete a machine and its storage                                                        |
| `--install <image>` | Create + boot a machine (`--name`, `--no-launch`, `--cpus`, `--memory`, `--home-mount`) |
| `--forward <p[:g]>` | Forward localhost port `p` to the machine's port `g`                                    |
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
```

## Differences from WSL

`cm` mirrors WSL's command line, but the underlying `container machine`
model differs in a few important ways.

### Filesystem sharing

WSL mounts every Windows drive under `/mnt/<letter>` (e.g. `C:\` →
`/mnt/c`). Container machines take a different approach: your macOS home
directory is shared into the guest via virtiofs **at the same absolute
path** — `/Users/<name>` on the Mac is `/Users/<name>` in the machine.

The flip side is that **nothing outside `$HOME` is shared**. There is no
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
so `cm -e` single-quotes each argument to cancel that out. Piped stdin
is forwarded (`cm` always passes `-i`).

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
  `2` (machines are full VMs). `-v -v` adds `container`-specific columns.
- Machine names must be lowercase DNS-style (`[a-z0-9-]`); WSL distro
  names are unrestricted.
- `--shutdown` stops machines but leaves `container` services running;
  `--shutdown --system` also runs `container system stop` (which stops
  every container, not just machines).
- No equivalents for `--export`/`--import` (see
  [doc/gaps/export-import.md](doc/gaps/export-import.md)), `--update`, `--manage`,
  `--mount` (VHDs), or `wsl.conf`.

## Notes

- `container machine run` requires a TTY for interactive shells (as does `cm`).
- `container` is resolved from `PATH`; set `CONTAINER_CLI` to override.

## License

[BSD-2-Clause License]

## AI Coding Declaration

Development of this project has been assisted by AI coding tools:

- [Devin]

Most of the code and documentation here was AI-generated and manually
reviewed. Take the documentation as a slightly out-of-date roadmap, no
matter the polish of the AI verbiage. Here be dragons.

[BSD-2-Clause License]: LICENSE
[Devin]: https://www.devin.ai/
