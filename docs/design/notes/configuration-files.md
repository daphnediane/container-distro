# Gap: no configuration files (`wsl.conf` / `.wslconfig`)

- **Status:** open -- `cm` currently has no config file at all
- **Fix level:** L0 for a host-side `cm` config; guest-side
  `/etc/wsl.conf` support is harder and partially L2.5 territory
- **Tracking:** [#6]

## What WSL does

Two config files, split by scope:

- **`/etc/wsl.conf`** -- per-distro, inside the guest: `[automount]`
  (enabled, root, mountFsTab, drvfs options), `[network]` (hostname,
  generateHosts/generateResolvConf), `[interop]` (enabled,
  appendWindowsPath), `[user] default=<name>`, `[boot]` (systemd,
  command).
- **`%UserProfile%\.wslconfig`** -- global, on the host: `[wsl2]` VM
  resources (memory, processors, swap, defaultVhdSize), networking
  (networkingMode, localhostForwarding, dnsProxy), `[experimental]`
  features.
- **`/etc/wsl-distribution.conf`** -- inside the distro rootfs, shipped
  by the *distro author* in a `.wsl` package ([build-custom-distro]):
  `[oobe] defaultName`/`defaultUid`/`command` (the command runs once at
  first launch), `[shortcut] icon`/`name` for the Start-menu entry, and
  a `terminal profile` GUID. Metadata about the distribution, not user
  config.

The first two are the interesting split: things the *guest* needs to
know at boot live in the guest; things describing the *VM itself* live
on the host. The third is packaging metadata -- closest to an OCI
image's `Config` fields than to either config file.

## What we have today

Nothing. `cm` parses CLI args and execs `container`; the only knobs are
per-invocation flags and a `--dev`-gated `CONTAINER_CLI`. Machine
settings that do exist
(`cpus`, `memory`, `home-mount`) live in `MachineConfig` via
`container machine set`. `container` itself has
`container system config` (global defaults -- registry, kernel, image
init) -- global but container-wide, not cm/machine-specific.

## What a `cm` equivalent would be

Following WSL's split:

- **Host-side global config** -- `~/.cmconfig.toml` (or
  `$XDG_CONFIG_HOME/cm/config.toml` for XDG-conformant placement):
  default machine, default user, default `--shell-type`, env defaults,
  extra mounts (see [mounts-outside-home]), port-forward mappings,
  resource defaults (cpus/memory) to pass to `machine set` at create
  time. Pure L0 -- `cm` reads it before building `container` args.
  Serde+TOML, small new dep (`toml`).
- **Per-machine config** -- partially exists already as `MachineConfig`
  (cpus/memory/home-mount). `cm` could expose more of it plus our own
  extensions (extra mounts, forwards) once an L2.5 plugin exists; at L0,
  cm-side per-machine overrides could live under the host config's
  `[machine.<name>]` tables.
- **In-distro `/etc/wsl-distribution.conf`** -- partially honored
  already: `cm --install --from-file` and `catalog install` read
  `oobe.defaultName` to suggest the distro name, and deliberately
  ignore the rest. `oobe.command` never runs -- importing a rootfs
  shouldn't execute its code before you ask (security T4);
  `oobe.defaultUid` doesn't apply since machines and distros provision
  the *host* account; `shortcut.*`/terminal-profile keys have no macOS
  analogue.
- **Guest-side `/etc/wsl.conf`** -- honoring the real file would
  maximize compat for users copying WSL setups, but has a
  chicken-and-egg problem: reading it requires running a process in the
  guest, and some settings (default user, automount root) are needed
  *before* the first session. Feasible at L0 with a boot-then-read
  pattern (`machine run` `cat`), cacheable per machine; cleaner at L2.5
  where a plugin can read the image fs or query vminitd without
  user-visible boot. Honoring `[boot] command`/`systemd` is out of scope
  -- the machine's init already owns PID 1.

## Recommendation

Implement the host-side config file at L0 when a second
"flag-that-should-be-a-default" appears (extra mounts will force it).
Defer guest-side `/etc/wsl.conf` honoring unless real users port WSL
distro configs; if we do it, cache aggressively and treat it as
best-effort hints, not a contract.

[#6]: https://github.com/daphnediane/container-distro/issues/6
[build-custom-distro]: https://learn.microsoft.com/en-us/windows/wsl/build-custom-distro
[mounts-outside-home]: ../upstream-limitations.md#no-mounts-outside-home-for-machines
