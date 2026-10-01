# Gap: no mounts outside `$HOME`

**Status:** open — the biggest functional gap vs WSL
**Cheapest fix level:** [L2.5](../lower-level-integration.md#the-plugin-route--l25)
(own daemon plugin) · also fixable at [L3](../lower-level-integration.md#l3--containerization-directly)
· cheapest of all might be an upstream PR

## What WSL does

WSL mounts every Windows drive under `/mnt/<letter>` (`C:\` → `/mnt/c`),
so any host path is reachable from the guest. `wsl --mount` can also
attach VHDs and physical disks.

## What we have today

`container machine` shares the macOS home directory into the guest via
virtiofs **at the same path** (`/Users/<name>` → `/Users/<name>`), mode
`rw`/`ro`/`none` via `machine create --home-mount` or `machine set`.
Nothing outside `$HOME` is shared and — as of `container` 1.5.x — the
`machine` surface exposes no additional-mount option. Repos on
`/Volumes/...` or outside the home directory are unreachable in-guest.

## Why it matters

Users keeping working copies outside `$HOME` (external drives, separate
APFS volumes, a case-sensitive volume like this repo lives on) cannot
use the WSL-style "edit on host, build in guest" flow at all.

## What the ideal mount model should look like

WSL's model is *every host drive appears at `/mnt/<letter>`*. The macOS
analogue isn't drive letters — it's volumes. Worth deciding before we
build anything:

- **Per-volume mounts at `/mnt/<volume-name>`** is the closest WSL
  analogue: `/Volumes/CaseSens` → `/mnt/casesens`, `/Volumes/Photos` →
  `/mnt/photos`. Lowercased to keep names DNS/filesystem boring, or
  preserve case for predictability (`/mnt/CaseSens`) — pick one and
  document it.
- **The root disk is not one volume.** macOS's `/` is a firmlinked
  volume *group*: a sealed, read-only `Macintosh HD` (system) plus a
  writable `Macintosh HD - Data` volume mounted at
  `/System/Volumes/Data`, with `/Users`, `/etc`, `/tmp` etc. firmlinked
  into it. A WSL user thinks of `C:` as "the disk" — the honest
  equivalent here is mounting the **Data volume** as `/mnt/c` (or
  `/mnt/data`): that's where user-relevant content lives. Sharing `/`
  itself would drag in the sealed system volume and is a needlessly huge
  attack surface; sharing `/System/Volumes/Data` gets `/Users`, `/opt`,
  `/usr/local`, `/etc`, `/private` in one shot.
  - So `/mnt/c` ≈ `/mnt/data`, not the whole root — worth a doc note,
    since it diverges from naive expectations both ways: users get more
    than "C:\Users" (all of Data) but less than "/" (no system volume).
- **Keep `$HOME` where it is.** The same-path `/Users/<name>` convention
  is already established and is better than `/mnt/c/Users/<name>` for
  tooling that encodes absolute paths (repos, editors, SSH config).
  Extra mounts should be *additive* at `/mnt/…`, not a move of `$HOME`.
- **Configurability.** WSL exposes `[automount] root=`, `enabled`,
  `mountFsTab`, and drvfs `options` (`uid`, `gid`, `umask`, `metadata`,
  `case`) in `wsl.conf`. Our equivalent should take `source`,
  `destination`, `ro`/`rw` at minimum — virtiofs has its own uid/gid
  semantics (mapped to host uid by default), so per-mount uid/umask
  options may not translate; document what we *can't* honor rather than
  silently ignoring it. See [configuration-files](configuration-files.md)
  for where such options would live.
- **Case sensitivity is a host property.** Case-sensitive APFS volumes
  (like this repo's) vs the default case-insensitive — nothing for us to
  do, but worth noting that a volume shared at `/mnt/x` keeps its host
  case behavior.

A reasonable default set: `/mnt/data` (or `/mnt/c`) →
`/System/Volumes/Data` read-only-by-default?, each `/Volumes/<name>` →
`/mnt/<name>` — plus user-specified `source:dest` pairs. Whether any of
this should be *on by default* is a trust question: WSL shares all
drives because the guest is "your" machine; a container machine is the
same trust level, but we should still make it opt-in and easy to scope
down (`machine set home-mount=none` style).

## Options

- **Upstream PR (preferred long-term).** Add an extra-mounts flag to
  `container machine create` / `machine set`. The plugin already builds
  `ContainerConfiguration.mounts` with `.virtiofs` entries — exposing
  more is a small change. Worth filing before we build anything.
- **L2.5 — `container-distro` plugin.** A `core` daemon plugin modeled
  on `MachineAPIServer` calls `ContainerClient.create()` with extra
  `.virtiofs(source: <any host path>)` mounts. No entitlements needed —
  the VM is still spawned by Apple's signed `container-runtime-linux`.
  Machines created this way are ours (`container-distro list/run/...`),
  not visible to `container machine`.
- **L0 approximation — compose `container create` ourselves.** The
  machine container is just a container whose `init` is bind-mounted in
  (`MachineBundle` virtiofs mounts: `/sbin.machine` ro, an
  `initializedFile`, plus the home share). A `cm` CLI plugin — or `cm`
  itself — could replicate `toContainerConfig` with `container create
  -v <extra>:<dest>` and get extra mounts with *zero* new daemon code.
  Downside: we're replicating machine semantics (user provisioning is
  done by the mounted init — verify it works when invoked this way) and
  the result isn't tracked by `machine list`/`inspect`.
- **L3 — `containerization` directly.** Full control, full maintenance
  burden (reimplement provisioning, networking, signing).

## Recommendation

File the upstream feature request first — it's a small ask and the
plugin's own code shows it's trivially expressible. If it stalls or is
rejected, prototype the L0 `container create` composition (cheap spike,
proves the semantics) and graduate to an L2.5 plugin if we want machine
lifecycle management alongside it.
