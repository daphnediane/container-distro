---
name: wsl-compat
description: How the `cm` WSL-compatible wrapper maps arguments to `container machine`, and how to verify it
---

# wsl-compat (`cm`)

`cm` is a Rust CLI that emulates `wsl.exe` argument semantics on top of Apple's
`container` CLI (`container machine`). Source lives in `src/`; the binary is
built with `cargo build` and can optionally be aliased to `wsl` via symlink.

## Argument mapping

- No args / `-d <m>` / `-u <u>` / `--cd <dir>` / `--shell-type <t>` →
  `container machine run` (the wrapper `exec()`s it for TTY passthrough)
- `-e <cmd...>`, `-- <cmd...>`, or bare positional args →
  `container machine run [-n m] -- cmd...`
- `-l/--list` (`--all`, `--running`, `-q`, `-v`) → `container machine list`
- `-s <m>` → `container machine set-default <m>`
- `-t <m>` → `container machine stop <m>`
- `--shutdown` → stop all running machines (services keep running)
- `--status` → `container system status`
- `--unregister <m>` → `container machine rm <m>`
- `--install <image>` → `container machine create`

Before any machine operation the wrapper runs `container system start` if
`container system status` shows services down.

## Verify

```bash
cargo test                                        # arg-parsing unit tests
container machine list                            # machines available for testing
cm -l                                             # table
cm -l -q                                          # ids only
cm -l --running                                   # running only
cm -d alpine -e uname -a                          # exec in machine
cm -d alpine -- pwd                               # passthrough
cm --status                                       # system status
```

A test machine can be created with
`container machine create --name alpine --set-default alpine:latest`.
