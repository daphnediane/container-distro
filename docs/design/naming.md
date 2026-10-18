# Naming: where the project name appears, and how to rename

The project is **`container-distro`**.

This document lists every place a persistent name is used, so that any
future rename is deliberate and complete, and it describes how existing
installs would migrate.

## Single source of truth

Persistent, externally visible names come from
[`src/naming.rs`]:

| Constant                | Current value                            | Used for                                                                                          |
| ----------------------- | ---------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `APP_NAME`              | `container-distro`                       | state directory, plugin `config.toml` metadata, `libexec/<APP_NAME>/` helper dir, host socket dir |
| `LABEL_PREFIX`          | `io.github.daphnediane.container-distro` | container label keys (`<prefix>.<suffix>`)                                                        |
| `LEGACY_LABEL_PREFIXES` | _(empty)_                                | label prefixes still _read_ after a rename                                                        |
| `LEGACY_APP_NAMES`      | _(empty)_                                | state directories migrated into place on startup                                                  |

`LABEL_PREFIX` follows the OCI/Docker convention that third-party label
keys use a reverse-DNS prefix for a namespace you control. With no
project domain, `io.github.<user>.<project>` is the usual choice.

## Where names persist

| What                     | Where                                                                                                                     | Derived from                                     |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| Distro marker label      | `<LABEL_PREFIX>.distro=<name>` -- the distro's _logical_ name (the runtime container id is ephemeral; see below)          | `LABEL_PREFIX`                                   |
| Home-mount label         | `<LABEL_PREFIX>.home-mount=rw\|ro\|none`                                                                                  | `LABEL_PREFIX`                                   |
| Admin label              | `<LABEL_PREFIX>.admin=true\|false\|never` (sudo/doas grant policy)                                                        | `LABEL_PREFIX`                                   |
| User label               | `<LABEL_PREFIX>.user=<name>:<uid>:<gid>`                                                                                  | `LABEL_PREFIX`                                   |
| Automount label          | `<LABEL_PREFIX>.automount=rw\|ro\|none` -- how the distro auto-manages `/Volumes` mounts                                  | `LABEL_PREFIX`                                   |
| State directory          | `~/Library/Application Support/<APP_NAME>/`                                                                               | `APP_NAME`                                       |
| Journal directory        | `~/Library/Application Support/<APP_NAME>/distros/<name>/`                                                                | `APP_NAME`                                       |
| Plugin metadata          | `<prefix>/libexec/container-plugins/distro/`                                                                              | `APP_NAME`                                       |
| Plugin / subcommand name | `container distro`, `bin/distro`                                                                                          | `plugin::PLUGIN_NAME`                            |
| Packaged helpers         | `<prefix>/libexec/<APP_NAME>/` beside an install's `bin/`                                                                 | `APP_NAME`                                       |
| Leftover distrofs files  | `<prefix>/bin/distrofs`, `<prefix>/libexec/<APP_NAME>/distrofs-server-<arch>`                                             | `APP_NAME`, hard-coded `distrofs`                |
| Distro import tag        | `local/distro-<name>:imported-<sha256>`                                                                                   | hard-coded `ops`                                 |
| Machine import tag       | `local/machine-<name>:imported-<sha256>` (from `cm --import`)                                                             | hard-coded `ops`                                 |
| Import image labels      | `<LABEL_PREFIX>.imported-from=<tar\|url>`, `<LABEL_PREFIX>.rootfs-sha256=<sha>`                                           | `LABEL_PREFIX`                                   |
| Legacy snapshot refs     | `imported-<timestamp>` imports, `:<timestamp>` `set` snapshots                                                            | `LABEL_PREFIX` (label), hard-coded `ops` (names) |
| Export scratch label     | `<LABEL_PREFIX>.export-scratch=<machine>` on the short-lived scratch container `cm --export` creates for a machine export | `LABEL_PREFIX`                                   |
| In-guest mount           | `/sbin.distro`                                                                                                            | hard-coded in `spec`/assets                      |
| In-guest sentinels       | `/etc/.distro.initialized`, `/etc/.distro.user.<name>`, `/etc/.distro.admin.<name>`                                       | hard-coded in `spec`/assets                      |
| In-guest auth files      | `/etc/sudoers.d/<name>`, `/etc/doas.d/<name>.conf`                                                                        | hard-coded in `spec`/assets                      |

The state directory holds `default-distro`, the `journalized` marker,
and the legacy shared `sbin.distro*/` assets, all `uchg`-locked (see
T6). Each journal directory holds `spec.json` — the flock target —
`rootfs.ext4`, and the distro's own `sbin.distro/` init assets, also
`uchg`-locked. The plugin `config.toml` carries `author` and `abstract`
metadata; uninstall also cleans the `sbin.distro*/` assets that
pre-per-distro installs wrote there. Packaged helpers are located by
`layout` from the running binary; installs made while `container distro
fs` was on main may also hold a `distrofs` client beside `bin/` and a
`distrofs-server-<arch>` build under `libexec/<APP_NAME>/`, which
uninstall removes (the plugin copy's prefix is its `distro/` dir).

The `imported-<sha256>` tags are per-distro and per-machine tags on a
single shared `local/rootfs:<sha256>` image, so identical rootfs
imports deduplicate across distros and machines. The legacy refs
predate that scheme: pre-#32 imports used `imported-<timestamp>` refs
labeled `<LABEL_PREFIX>.distro=<name>`, and pre-0.2 `set` snapshots
used `:<timestamp>` refs.

The plugin subcommand (`container distro`) and the in-guest paths are
user-facing and baked into existing containers. Renaming them is a
larger decision than renaming the project.

## Names that are _not_ persistent (rename freely)

- **Runtime container ids** — a distro's materialized container is
  `<name>-<6 lowercase hex>` (arc4random, like `container machine`'s
  `poc-a07fee`), fresh on every materialize and never recorded in the
  journal. Everything keys off the `<LABEL_PREFIX>.distro` label
  instead, so the id's shape is an implementation detail — users only
  see it in `container list`, `container inspect <id>`, and the
  container's `containers/` dir.
- The Cargo package name (`container-distro`) and the repository name
- Prose in `README.md`, `docs/`, and `--help` text
- Temp-file prefixes (`cm-import-`, `distro-set-`, …)

## Migration strategy for a future rename

1. **Change the constants.** Set `APP_NAME` and `LABEL_PREFIX` to the
   new values. Prepend the old values to `LEGACY_APP_NAMES` and
   `LEGACY_LABEL_PREFIXES`.
2. **Labels: read old, write new.** All label reads go through
   `naming::label_lookup`, which tries the current prefix, then each
   legacy prefix. Existing distros keep working unchanged. Labels are
   immutable on a container, so they move to the new prefix whenever a
   distro is recreated -- `container distro set` already recreates. A
   bulk `container distro migrate` command could force this if needed.
3. **State directory: move on first use.** `naming::state_dir` renames
   the first existing legacy directory into place when the new one
   doesn't exist, and leaves a symlink at the old path. Existing distros
   still reference the _old_ init-assets path until they are recreated,
   and the symlink keeps it valid. Verify that virtiofs follows it on
   the first real rename.
4. **Plugin registration.** `uninstall-plugin` recognizes our
   `config.toml` by its `abstract` text, which does not include
   `APP_NAME`, so it still works after a rename. Reinstall after
   upgrading so the metadata shows the new name.
5. **Drop legacy entries** after a deprecation window (at least one
   release), once nothing should still carry the old labels.

Before there are real users, the cheaper path is fine: change the
constants and recreate any test distros.

[`src/naming.rs`]: ../../src/naming.rs
