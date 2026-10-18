# Configuration files (`wsl.conf` / `.wslconfig`)

- **Status:** distro-side implemented ([#6]) — a bounded subset of
  `/etc/wsl.conf` plus journal snapshots of both files, honoring
  expressed as a stored *mode* (`all` living, a feature list frozen,
  or tier-relative `+`/`-` adjustments). Host-side `cm` config remains
  open.
- **Fix level:** distros shipped it at L1 (our own init already owns
  PID 1); machines don't get it — their init belongs to upstream.
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
  first launch), `[shortcut] enabled`/`icon` for the Start-menu entry,
  and `[windowsterminal] enabled`/`ProfileTemplate` for the terminal
  profile. Metadata about the distribution, not user config.

The first two are the interesting split: things the *guest* needs to
know at boot live in the guest; things describing the *VM itself* live
on the host. The third is packaging metadata -- closest to an OCI
image's `Config` fields than to either config file.

## What's honored for distros

Distros run our own `init` as PID 1, which is what makes guest-side
honoring feasible at all: the same file that gates init selection is
parsed by the thing that selects it. Machines keep upstream's init and
get none of this -- the feature is `container distro` only.

### The feature set and its modes

Honoring covers a fixed set of named features, resolved per distro
through the standard intent chain (`explicit` mode → `preset ∩ source`
→ empty default) and journaled as `wsl-conf`:

| Feature     | `/etc/wsl.conf` keys honored                                                                                        |
| ----------- | ------------------------------------------------------------------------------------------------------------------- |
| `hostname`  | `[network] hostname` -> `/etc/hostname` + `hostname(1)`                                                             |
| `fstab`     | `[automount] mountFsTab` -> `mount -a` unless `false`                                                               |
| `init`      | `[boot] systemd` and `boot.openrc` -> pick the guest's PID 1                                                        |
| `command`   | `[boot] command` -> `sh -c` as root, once per boot, before init                                                     |
| `automount` | `[automount] enabled=false` -> lower automount/home-mount to `none` at archive import (host-side, create-time only) |

Flags: `--use-wsl-conf[=FEATURE,...|all|none]` allows (a bare flag
means `all`), `--ignore-wsl-conf=FEATURE,...|all|none` denies, and
`--no-wsl-conf` honors nothing. All three exist on `create`/`import`/
`set` and `cm --install`; `set --reset wsl-conf` un-pins.

What the flags store is a **mode expression**, not the resolved list —
the journaled `wsl-conf` intent serializes as:

| Stored string         | Mode        | Resolves to                          | On feature growth                         |
| --------------------- | ----------- | ------------------------------------ | ----------------------------------------- |
| `fstab,hostname`      | `Only`      | exactly those features               | frozen                                    |
| `none`                | `Only{}`    | nothing                              | frozen                                    |
| `all`, `all,-command` | `AllExcept` | every feature except the `-` denials | living -- new features join automatically |
| `+hostname,-command`  | `Default`   | `(tier set + allow) - deny`          | living, and follows `set --preset` moves  |

How the flags pick the mode: an `all`/`none` keyword switches it —
`--use all` or `--ignore none` → `AllExcept`, `--use none` or
`--ignore all` → `Only{}`. With plain feature lists, `--use` alone
pins `Only`, `--ignore` alone leaves `AllExcept` (denying `command`
means "honor the rest"), and both together make `Default`
(tier-relative adjustments). On `set` the lists *fold into* the stored
mode — `use` allows, `ignore` denies — so `set --use-wsl-conf=fstab`
accumulates into an `Only` pin while `set --use-wsl-conf=all` re-opens
the distro to living-minus.

Defaults follow the trust tier: `trusted` and catalog installs ask for
`all` — a *living* ask, so the curated set tracks the build (the
package/image was already trusted to run code: `[boot] command` is
image shell running as root, but the image's own init already does
that — no extra privilege). `restricted` contributes the empty set so
a source ask can't lift it; everything else honors nothing.

`automount` is asymmetric by design: it only ever *lowers* sharing
(the T6 confused-deputy rule — a guest file can reduce exposure, never
raise it), applies only to archive imports (an image-based `create`
can't read the config before first boot), and loses to every explicit
flag — it's `source` intent, not an override.

### Guest-side behavior

`init` reads the live `/etc/wsl.conf` at every boot — the restart-to-
apply model WSL uses, so `rm`/`edit`/`echo` in the guest behaves the way
WSL users expect. An absent `CONTAINER_WSL_CONF` env (ad-hoc,
restricted, or pre-feature distros) means the file is never opened.

- `systemd=true` prefers `/lib/systemd/systemd`/`/usr/lib/…`; `false`
  skips init candidates that resolve to systemd. Best-effort: it picks
  the binary, and full systemd-under-container may still hit
  cgroup/mount gaps upstream.
- `boot.openrc` is a **project extension** (WSL can't swap inits —
  Gentoo-in-WSL runs `openrc default` as a `[boot] command`). `true`
  prefers `openrc-init` as real PID 1, then a non-systemd `init`, then
  the Gentoo recipe (`openrc default` under our idle PID 1). A
  `systemd=true` + `openrc=true` conflict warns and `openrc` wins.
- `[boot] command` runs before the init exec, as root; its failure
  warns and boot continues.

### Journal snapshots

Both files are also snapshotted into the distro's journal
(`distros/<name>/wsl.conf`, `wsl-distribution.conf`) via `container cp`
— runtime-mediated extraction, so no guest binary ever runs to produce
them. Refreshed at every boot and before every host-initiated stop
(`stop`, `set`, `export`, `rename`, `migrate --out`); `cp` needs a
running container, so a distro killed via bare `container stop` keeps
its last-boot snapshot — consistent with restart-to-apply anyway.
Snapshots are ≤64 KiB, `uchg`-locked, written `O_NOFOLLOW`, and are
**informational only** — nothing host-side consumes them yet (they're
the data foundation for the deferred `user.default` work), and they
never feed mounts/network/publish decisions.

A symlinked `/etc/wsl.conf` makes `cp` resolve some other guest file —
bounded by the size cap, parse-only, and never executed, so the
exposure is an odd file in the journal, nothing more.

### Per-key support matrix

`/etc/wsl.conf`:

| Key                                                   | Verdict                                                                                                                     |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `boot.systemd`                                        | honored (`init`)                                                                                                            |
| `boot.openrc` (**extension**)                         | honored (`init`)                                                                                                            |
| `boot.command`                                        | gated (`command`)                                                                                                           |
| `boot.protectBinfmt`, `boot.initTimeout`              | not honored                                                                                                                 |
| `automount.mountFsTab`                                | honored (`fstab`)                                                                                                           |
| `automount.enabled`                                   | honored lower-only (`automount`)                                                                                            |
| `automount.root/options/ldconfig/cgroups`             | not honored                                                                                                                 |
| `network.hostname`                                    | honored (`hostname`)                                                                                                        |
| `network.generateHosts`, `network.generateResolvConf` | not honored -- vminitd synthesizes both before our init runs                                                                |
| `interop.*`                                           | deferred -- needs a `\\wsl$`-equivalent ([#27]); tracked separately                                                         |
| `user.default`                                        | deferred -- interacts with auto-provisioning (when should our user setup be skipped?); snapshots already capture it ([#29]) |
| `gpu.*`, `time.*`                                     | not honored -- no GPU passthrough; the VM clock is host-synced                                                              |

`/etc/wsl-distribution.conf`:

| Key                               | Verdict                                                                    |
| --------------------------------- | -------------------------------------------------------------------------- |
| `oobe.defaultName`                | honored -- distro-name suggestion at `.wsl` import                         |
| `oobe.defaultUid`                 | not honored -- we provision the host account                               |
| `oobe.command`                    | **not run** -- deferred (account setup doesn't map; see security) ([#29])  |
| `windowsterminal.enabled`         | honored -- gates generated Terminal/iTerm2 profiles (default true) ([#30]) |
| `windowsterminal.ProfileTemplate` | **not read** -- deferred (WT-fragment styling doesn't map 1:1) ([#34])     |
| `shortcut.*`                      | deferred -- macOS launcher entries ([#33])                                 |

### Rejected alternative: host-side file/directory mount

Mounting a journaled `wsl.conf` into the guest was evaluated and
rejected: `containerization` turns every virtiofs share into a
`VZSharedDirectory`, which requires a *directory* URL — regular-file
mounts can't be expressed at all. Mounting a directory over `/etc`
hides the whole directory; and even if file mounts existed, editors
that save via write-temp+rename can't replace a mount point and the
guest couldn't `rm` the file — edits would silently not persist. The
snapshot model keeps `/etc/wsl.conf` a normal guest file.

## What remains open

- **Host-side global config** -- `~/.cmconfig.toml` (or
  `$XDG_CONFIG_HOME/cm/config.toml`): default backend, default
  `--shell-type`, env defaults, extra mounts, port-forwards, resource
  defaults. Pure L0 -- `cm` reads it before building `container` args.
  Still deferred ([#45]); see security.md for the
  guest-writable-config trap.
- **`user.default` + `oobe.command`** ([#29]) — need a decision on
  when auto-provisioning should be skipped (a distro declaring its
  own account model); the snapshots already land the data.
- **Windows-Terminal profile templates** ([#34]) —
  `windowsterminal.ProfileTemplate` styling (colors/fonts) isn't
  mapped to Terminal.app/iTerm2 profile fields yet; native
  `[macosterminal]`/`[iterm2]` extension sections are the other half
  of that issue.
- **Launcher entries** (`shortcut.*`) ([#33]) — a Start-menu
  equivalent: a clickable host-side launcher into the distro.
- **`interop.*`** — a `\\wsl$` host-side equivalent is the blocker
  ([#27]); likely hard to near-impossible.
- **`~/.wslconfig` per-machine VM config** — resources stay per-distro
  flags/spec fields; no global file planned.

[#6]: https://github.com/daphnediane/container-distro/issues/6
[#27]: https://github.com/daphnediane/container-distro/issues/27
[#29]: https://github.com/daphnediane/container-distro/issues/29
[#30]: https://github.com/daphnediane/container-distro/issues/30
[#33]: https://github.com/daphnediane/container-distro/issues/33
[#34]: https://github.com/daphnediane/container-distro/issues/34
[#45]: https://github.com/daphnediane/container-distro/issues/45
[build-custom-distro]: https://learn.microsoft.com/en-us/windows/wsl/build-custom-distro
