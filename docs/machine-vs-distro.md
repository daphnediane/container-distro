# `container machine` vs `container distro`

Both give you a persistent, WSL-style Linux environment on top of
Apple's [`container`], and `cm` treats them as one namespace of
"distributions". They differ in who manages the lifecycle and what the
result can do.

## At a glance

|                              | `container machine`                                                             | `container distro`                                                   |
| ---------------------------- | ------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| What it is                   | A dedicated VM per machine, managed by `container`'s machine plugin             | A regular `container` container plus a journaled ext4 disk we manage |
| Your account                 | Provisioned inside (name/uid/gid, passwordless sudo), `SSH_AUTH_SOCK` forwarded | Same                                                                 |
| `$HOME` sharing              | Shared at the same path, `rw`/`ro`/`none`                                       | Same                                                                 |
| Mounts outside `$HOME`       | **No** ([apple/container#1805], [#2278])                                        | `-v SRC:DST[:ro]`, `--automount` for `/Volumes/*`                    |
| Published ports              | **No publish API** (`cm --forward` is a userspace workaround)                   | `-p`/`--publish` -- real runtime port publishing                     |
| Export / import              | **Not directly** (`cm --export`/`--import` works around it)                     | `distro export` / `distro import`                                    |
| Change settings after create | Only `home-mount` (`machine set` + restart)                                     | `distro set` -- cpus, memory, mounts, ports, network, ssh, sudo      |
| Rename                       | **None** (`cm --rename` recreates + clones the disk)                            | `distro rename`                                                      |
| Command exec                 | `machine run` shell-evals joined argv; first write can drop                     | `container exec` -- argv-exact                                       |
| Listing                      | `container machine list`                                                        | `container ls` (labeled regular containers)                          |
| Where state lives            | `container`'s appRoot (machine bundle)                                          | `~/Library/Application Support/container-distro/distros/<name>/`     |
| Extra overhead               | --                                                                              | One extra vCPU per distro (`nproc` shows `cpus + 1`)                 |

## The shared base

Whichever backend you use, the environment is the same WSL-style
shape:

- your macOS account is provisioned inside -- same name/uid/gid, with
  passwordless sudo/doas -- and `SSH_AUTH_SOCK` is forwarded in
- `$HOME` is shared at the same absolute path (`/Users/<name>` →
  `/Users/<name>`), unless you pick `ro`/`none`
- the image's own init runs as PID 1 (or an idle PID 1 for images
  without one)
- persistent ext4 disk across stop/start; boots on demand

And the same caveat: neither is a sandbox -- see [security.md].

## What a distro adds

`container machine` can't share anything outside `$HOME`, has no
port-publishing API, can't export its filesystem directly, and fixes
nearly all settings at creation. A distro is a regular `container`
container configured the way the machine plugin configures a machine
-- then extended with what machines lack:

- **Arbitrary mounts** -- `-v /Volumes/Code:/mnt/code[:ro]`, or
  `--automount` to map every `/Volumes/<X>` to `/mnt/<x>` (skipping
  hidden, `com.apple.*`, and unreadable volumes).
- **Published ports** -- `-p 3000`, `-p 8080:80`, `-p
  0.0.0.0:8080:80/udp` (host IP defaults to `127.0.0.1`), adjustable
  later with `set --publish`/`--unpublish`.
- **`set`** -- recreate on the same filesystem to change cpus, memory,
  mounts, ports, network, ssh-agent, and sudo provisioning.
- **Export/import/rename/migrate** -- first-class lifecycle commands.
- **Exact argv** -- `run` uses `container exec`, so arguments are
  never re-split through a shell.

The cost is that *we* manage the lifecycle: a stopped distro is only
its journal (`spec.json` + `rootfs.ext4` + init assets, `uchg`-locked),
and `start` materializes a fresh container from it -- so a distro
shows as `<name>-<6hex>` in `container ls`, and tampered runtime
configuration never survives a restart.

## One namespace in `cm`

`cm` resolves a shared name to the **distro** first, then the machine
-- and because that shadowing would be silent, creating a colliding
name is refused everywhere unless `--force` opts in. Listings never
hide a collision: `cm -l` shows both rows suffixed `(machine)`/
`(distro)`. A default distro wins over the default machine; `cm -s`
on a machine records the machine as default. `--distro`/`--machine`
are explicit selectors; `CM_BACKEND=machine` makes `cm` ignore distros
entirely. Details: [cm.md].

## Converting between them

```bash
container distro migrate --in my-machine    # machine → distro (rootfs carried over)
container distro migrate --out my-distro    # distro → machine
cm --migrate NAME                           # same, via cm (backend flags select direction)
```

The disk is cloned, not exported/re-imported -- your filesystem comes
across intact. `--keep` retains the source (and requires `-n NAME`,
since a kept source holds its name).

## Which should I use?

- **Distro (the default)** for anything real: mounts outside `$HOME`,
  published ports, export/import, settings that can change later. `cm
  --install` and `container distro create` build distros by default.
- **Machine** for upstream-parity testing, reporting bugs against
  `container` itself, or when you want only what Apple's plugin manages
  (`cm --install --machine`, `container machine create`).
- **Machine** when something *else* needs to drive it: tooling that
  lists or manages `container machine` objects (scripts, third-party
  plugins, anything calling `container machine` directly) sees
  machines, not distros -- a distro is a plain labeled container to
  them.

[`container`]: https://github.com/apple/container
[apple/container#1805]: https://github.com/apple/container/issues/1805
[#2278]: https://github.com/apple/container/issues/2278
[cm.md]: cm.md
[security.md]: security.md
