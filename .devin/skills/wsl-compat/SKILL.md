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

## Host filesystem

`container machine` mounts macOS `$HOME` into the guest via virtiofs at the
**same path** (`/Users/<name>`), mode `rw`/`ro`/`none` via `--home-mount` at
create or `machine set home-mount=<mode>` + restart. Nothing outside `$HOME`
is shared and no additional-mount option exists (container 1.5.0). `machine
run` auto-starts in the same path when host cwd is under `$HOME`, else guest
`/home/<name>`. Machines get their own IP — no localhost forwarding like WSL.
See "Differences from WSL" in README.md.

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
