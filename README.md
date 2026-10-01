# wsl-compat (`cm`)

A [WSL](https://github.com/microsoft/WSL)-compatible command-line wrapper for
[Apple container machines](https://github.com/apple/container/blob/main/docs/container-machine.md).

With no arguments, `cm` opens a login shell in the default container machine:

```bash
cm                # container machine run
```

## Install

```bash
cargo install --path .
# optionally alias to `wsl`:
ln -s "$(which cm)" ~/.local/bin/wsl   # or any dir on PATH
```

## Usage

```text
cm [OPTIONS] [-- <COMMAND LINE>]
```

### Run commands and shells

| Option               | Description                                       |
| -------------------- | ------------------------------------------------- |
| `-d, --distribution` | Machine to use (default machine if omitted)       |
| `-u, --user`         | Run as the specified user                         |
| `--cd`               | Working directory inside the machine              |
| `--shell-type`       | `standard` (non-login), `login` (default), `none` |
| `-e, --exec`         | Execute the command line without a shell          |
| `-- <cmd>`           | Pass the remaining command line through verbatim  |
| `--env KEY=VALUE`    | Set an environment variable in the machine        |

### Manage machines

| Option              | Description                                       |
| ------------------- | ------------------------------------------------- |
| `-l, --list`        | List machines (`--all`, `--running`, `-q`, `-v`)  |
| `-s, --set-default` | Set the default machine                           |
| `-t, --terminate`   | Stop a running machine                            |
| `--shutdown`        | Stop all running machines                         |
| `--status`          | Show container system status                      |
| `--unregister`      | Delete a machine and its storage                  |
| `--install <image>` | Create + boot a machine (`--name`, `--no-launch`) |
| `--version`         | Show `cm` and `container` versions                |

If `container` services aren't running, `cm` runs `container system start`
first.

## Examples

```bash
cm -d alpine                      # shell in the alpine machine
cm -e uname -a                    # run a command
cm -- ls -la                      # verbatim passthrough
cm -d alpine --cd /tmp -e pwd     # command with a working dir
cm -l -v                          # list machines
cm -t alpine                      # stop it
cm --install alpine:latest --name dev   # create and launch a machine
```

## Notes

- `container machine run` requires a TTY for interactive shells (as does `cm`).
- `container` is resolved from `PATH`; set `CONTAINER_CLI` to override.
- `--shutdown` stops machines but leaves `container` services running
  (use `container system stop` for a full shutdown).

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
