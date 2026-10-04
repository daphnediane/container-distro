# Naming: where the project name appears, and how to rename

The project is **`container-distro`**.

This document lists every place a persistent name is used, so that
any future rename is deliberate and complete, and it describes how
existing installs would migrate.

## Single source of truth

Persistent, externally visible names come from
[`crates/cm-core/src/naming.rs`](../crates/cm-core/src/naming.rs):

| Constant                | Current value                            | Used for                                         |
| ----------------------- | ---------------------------------------- | ------------------------------------------------ |
| `APP_NAME`              | `container-distro`                       | state directory, plugin `config.toml` metadata   |
| `LABEL_PREFIX`          | `io.github.daphnediane.container-distro` | container label keys (`<prefix>.<suffix>`)       |
| `LEGACY_LABEL_PREFIXES` | _(empty)_                                | label prefixes still _read_ after a rename       |
| `LEGACY_APP_NAMES`      | _(empty)_                                | state directories migrated into place on startup |

`LABEL_PREFIX` follows the OCI/Docker convention that third-party label
keys use a reverse-DNS prefix for a namespace you control. With no
project domain, `io.github.<user>.<project>` is the usual choice.

## Where names persist

| What                     | Where                                                                                                                                                         | Derived from                |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------- |
| Distro marker label      | `<LABEL_PREFIX>.distro=<name>` on each distro container                                                                                                       | `LABEL_PREFIX`              |
| Home-mount label         | `<LABEL_PREFIX>.home-mount=rw\|ro\|none`                                                                                                                      | `LABEL_PREFIX`              |
| Admin label              | `<LABEL_PREFIX>.admin=true\|false\|never` (sudo/doas grant policy)                                                                                            | `LABEL_PREFIX`              |
| User label               | `<LABEL_PREFIX>.user=<name>:<uid>:<gid>`                                                                                                                      | `LABEL_PREFIX`              |
| State directory          | `~/Library/Application Support/<APP_NAME>/` (`sbin.distro*/` init assets, `default-distro`, `preserved/` staged rootfs+journal, `locks/` flock files)         | `APP_NAME`                  |
| Plugin metadata          | `<prefix>/libexec/container-plugins/distro/config.toml` (`author`, `abstract`)                                                                                | `APP_NAME`                  |
| Plugin / subcommand name | `container distro`, `bin/distro`                                                                                                                              | `plugin::PLUGIN_NAME`       |
| Snapshot / import images | `local/distro-<name>:<timestamp>` / `:imported-<timestamp>`                                                                                                   | hard-coded in `ops`         |
| In-guest files           | `/sbin.distro` mount, `/etc/.distro.initialized`, `/etc/.distro.user.<name>`, `/etc/.distro.admin.<name>`, `/etc/sudoers.d/<name>`, `/etc/doas.d/<name>.conf` | hard-coded in `spec`/assets |

The plugin subcommand (`container distro`) and the in-guest paths are
user-facing and baked into existing containers. Renaming them is a
larger decision than renaming the project.

## Names that are _not_ persistent (rename freely)

- Cargo package names (`cm`, `cm-core`, `container-distro`) and the
  repository name
- Prose in `README.md`, `doc/`, and `--help` text
- Temp-file prefixes (`cm-import-`, `distro-set-`, …)

## Migration strategy for a future rename

1. **Change the constants.** Set `APP_NAME` and `LABEL_PREFIX` to the
   new values. Prepend the old values to `LEGACY_APP_NAMES` and
   `LEGACY_LABEL_PREFIXES`.
2. **Labels: read old, write new.** All label reads go through
   `naming::label_lookup`, which tries the current prefix, then each
   legacy prefix. Existing distros keep working unchanged. Labels are
   immutable on a container, so they move to the new prefix whenever a
   distro is recreated — `container distro set` already recreates. A
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
