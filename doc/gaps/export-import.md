# Gap: `--export` / `--import`

**Status:** solvable at **L0** today — no new machinery needed
**Fix level:** L0 (CLI composition); would get nicer at
[L2](../lower-level-integration.md#l2--swift-library-products-thin-c-shim-for-rust)/L2.5

## What WSL does

- `wsl --export <Distro> <FileName>` — writes the distro's root
  filesystem as a tar.
- `wsl --import <Distro> <InstallLocation> <FileName>` — registers a new
  distro whose rootfs comes from a tar (e.g. a previous export).

## What we have today

Nothing exposed via `cm`. But `container` has all the primitives — a
machine's persistent state lives in its backing container, and
`container machine inspect <m>` reports the `containerId`.

## L0 recipes

- **Export (rootfs tar — WSL-faithful):**
  `container machine inspect <m>` → `containerId` →
  `container export <cid> -o file.tar`. Takes a runtime snapshot
  automatically if the machine is running. Virtiofs mounts (`$HOME`,
  `/sbin.machine/init`) aren't part of the container filesystem —
  **verify** they're excluded; that's the desired behavior anyway since
  `$HOME` is host data. Guest-side mutations (provisioned user accounts,
  installed packages) live in the container layer and are captured.
- **Snapshot → image (OCI path):** `container commit <cid> <ref>` +
  `container image save`/`load` round-trips through standard OCI
  archives. Better fidelity (image config preserved) but not
  WSL-tar-compatible. Could be exposed as `cm --export --format oci`.
- **Import (raw rootfs tar → new machine):** `container image load`
  only accepts OCI-layout tars (from `image save`), not bare rootfs
  tars — so `cm --import` needs a wrap step:
  - `container build` with `FROM scratch` + `ADD rootfs.tar /`, or
  - emit OCI image layout ourselves — it's just `blobs/sha256/*` +
    `index.json`, easy to generate in Rust — then `image load`,
  - then `container machine create --name <m> <ref>`.
- **Import into an existing machine** is even simpler: stream the tar
  through `machine run`'s stdin to `tar -xf - -C /`. No OCI involved,
  but it mutates an existing distro rather than registering a new one —
  only half of WSL semantics.

## Caveats

- Machine names are lowercase DNS-style (`[a-z0-9-]`); `wsl --import`
  accepts arbitrary distro names — `cm` must validate/sanitize.
- WSL `--import` takes an install location (tar → vhd at a path); the
  machine equivalent keeps state inside `container`'s app root — no
  location choice, document `--install-location` as accepted-but-ignored
  or drop it.
- Verify `container export` on a *machine* container specifically — the
  machine label (`plugin: machine`) shouldn't matter, but confirm.

## Recommendation

Implement `--export` and `--import` at L0. `container export` gives the
tar path for free; for import, generating OCI layout in Rust is
deterministic and avoids depending on the builder's availability.
