# `cm` vs `wsl`

`cm` mirrors WSL's command line -- `wsl.exe` argument semantics,
optionally symlinked to `wsl` (`cm --install-alias wsl`) -- but the
underlying `container machine` model differs from WSL in a few
important ways. This is the user-facing summary; per-item analysis and
upstream tracking live in [design/upstream-limitations.md] and
[design/notes/]. The `cm` command reference is [cm.md].

## Filesystem sharing

WSL mounts every Windows drive under `/mnt/<letter>` (e.g. `C:\` →
`/mnt/c`). Container machines take a different approach: your macOS
home directory is shared into the guest via virtiofs **at the same
absolute path** -- `/Users/<name>` on the Mac is `/Users/<name>` in the
machine.

The flip side is that **a machine shares nothing outside `$HOME`**
(use a [distro] for that). There is no `/mnt/...` equivalent and, as
of `container` 1.5.0, `container machine` has no option for additional
mounts -- the only configurable share is the home mount (`rw` by
default; `ro` or `none` via `container machine create --home-mount`
or `container machine set -n <m> home-mount=<mode>` plus a restart).
Directories on other volumes (e.g. `/Volumes/...`) or elsewhere
outside your home directory are unreachable from inside the machine.
Pick the home mode at creation with `cm --install <image>
--home-mount ro`, or change it later with `container machine set`.

Distros close this gap: `-v SRC:DST[:ro]` mounts any host directory,
and `--automount` maps each `/Volumes/<X>` to `/mnt/<x>` -- the closest
thing to WSL's drive mounting.

Sharing runs the other direction too in WSL: every distro's root
filesystem is browseable from Windows at `\\wsl$\<distro>` (aka
`\\wsl.localhost\<distro>`) -- Explorer, editors, and `Copy-Item` can
all reach in. **There is no equivalent here**: nothing on the Mac can
see inside a machine's or distro's filesystem -- virtiofs shares only
expose the host to the guest, not vice versa. To get files out you
copy them through a shared directory, or take a full rootfs tar with
`cm --export` ([#27]).

Also note the guest's `~` is `/home/<name>` -- a pure Linux home on
the machine's persistent disk -- distinct from your macOS home at
`/Users/<name>`.

## Starting directory

WSL starts a session in the `/mnt/c/...` mapping of your Windows
working directory. `container machine run` does the equivalent
automatically: when the host working directory is under `$HOME`, the
guest process starts at the same path; anywhere else it starts in the
guest's `/home/<name>`. `cm` inherits this behavior -- no flag needed;
for distros the same rule applies to any shared mount
(`/Volumes/Code/x` → `/mnt/code/x`).

`cm --cd <dir>` maps to `container`'s `-w` and expects a guest path.
Host paths under `$HOME` work verbatim since the mount is at the same
path; host paths outside `$HOME` do not exist in a machine's guest.

## Networking

WSL2 forwards guest ports to Windows localhost and offers other
`networkingMode`s (`mirrored`, bridged on older builds); a container
machine always gets its own IP on the `machine` network (shown by
`cm -l`) -- there is no mode knob ([#43]). Services
in the guest are reachable at that IP only -- nothing is bridged to
macOS localhost automatically. `cm --forward 3000` (or `--forward
8080:80`, repeatable, with `-d` to pick the machine) runs a
foreground forwarder from `127.0.0.1` on the Mac to the machine until
you press Ctrl-C. `--forward` is experimental and may be replaced by
automatic listener-based forwarding; for distros, prefer `--publish`
at install time or `container distro set --publish`/`--unpublish`,
which use the runtime's real port publishing (no userspace proxy).

## Command execution

As with WSL, `cm -e cmd args…` delivers each argument exactly, while
`cm cmd…` and `cm -- cmd…` run the command line through the guest
shell (so `cm -- echo '$HOME'` expands in the guest). `container
machine run` always shell-evaluates its arguments
([apple/container#1954]), so `cm -e` bypasses it: it runs the command
with `container exec` in the machine's backing container (as your
user, starting in the same directory `machine run` would). If that
isn't possible, `cm` falls back to `machine run` with each argument
single-quoted. Shells and `--` commands still use `machine run`.
Piped stdin is forwarded (`cm` always passes `-i`). Distros always
use `container exec`, so `-e` and `--` are both argv-exact there.

WSL shares selected host environment variables via `WSLENV` (with
path translation); there is no persistent env-passing equivalent --
`cm --env KEY[=VALUE]` is per-invocation, and `KEY` alone inherits
from the host (the `container` CLI resolves it, so the value never
rides `cm`'s own command line). Where `--env` values are visible, and
why secrets deserve a better channel, is [#44].

Known upstream issue: `container machine run` can drop the guest's
very first write to stdout/stderr -- see
[design/upstream-limitations.md].

## Interop and user mapping

WSL can execute Windows binaries from Linux via binfmt interop; there
is no macOS-binary interop in a container machine. On the other hand,
the guest account is auto-provisioned to match your macOS
username/uid/gid (with passwordless sudo) and `SSH_AUTH_SOCK` is
forwarded into the guest, so `git`/`ssh` operations use your host
agent with no extra setup.

Unlike `wsl --install`, there is no first-run OOBE prompt asking for a
new Unix username and password -- machines and distros provision your
macOS account automatically, so a fresh install drops straight into a
shell.

## Management differences

- `cm -l -v` prints WSL's `NAME STATE VERSION` table; VERSION is
  always `2` (machines and distros are full VMs). `-v -v` adds
  `container`-specific columns including KIND (`machine`/`distro`).
- Machine and distro names must be lowercase DNS-style (`[a-z0-9-]`);
  WSL distro names are unrestricted.
- `--shutdown` stops machines and distros but leaves `container`
  services running; `--shutdown --system` also runs `container system
  stop` (which stops every container, not just machines).
- `cm --export`/`--import` cover machines and distros; `--import`'s
  install-location argument is accepted and ignored (details:
  [design/upstream-limitations.md]). Tar is the only format: WSL's
  `--export --vhd`, `--import --vhd`, and `--import-in-place`, and
  vhdx-based `.wsl` packages, are unsupported. `wsl --install
  --location <dir>` has no equivalent either -- installs always live
  under `container`'s app-root. Exporting a *running* machine briefly
  stops it (crash-consistent disk clone) and restarts it; WSL exports
  live.
- `cm` resolves one namespace: a distro and a machine can share a name
  only via an explicit `--force`; `-d` resolves distros first. See
  [machine-vs-distro.md].
- The `*` default is an *effective* default: normally a distro, but it
  can follow (or lazily point at) a `container` machine when no
  distros are installed. As in WSL, deleting the default distro moves
  `*` to the best remaining distro rather than leaving none — the
  marker converges on the next command that reads it. See
  [machine-vs-distro.md].
- `cm -t`/`--terminate` with no name stops the effective default —
  WSL requires a distro name there. A bare `cm -t` that resolves to a
  machine errors unless `--machine` makes the target explicit.
- `cm --update [VERSION]` exists and, like `wsl --update`, updates the
  tooling itself (the release `.pkg`, via Installer.app) -- not
  installed distros, and not `container` (that's Apple's
  `update-container.sh`).
- `--arch arm64|amd64` is a `cm` extension with no WSL equivalent: it
  selects the guest userspace arch at `--install`/`--import` (`amd64`
  picks x86-64 catalog `.wsl`s or image variants and runs translated
  under Rosetta -- amd64-only entries like SLES become installable)
  and filters `-l`/`-l -o` to that arch. The arch is recorded with the
  distro, so a recreated container keeps it.
- No GUI app support (WSLg's X11/Wayland) -- see
  [design/notes/gui-apps.md] for what it would take.
- No equivalents for `--manage` or `--mount` (VHDs), `wsl --system`
  (the system distro), or `--set-version`/`--set-default-version`
  (VERSION is always `2` -- there's nothing to convert to).
- No idle teardown: WSL powers off the VM shortly after the last
  guest process exits (`vmIdleTimeout`, ~60s default); a machine or
  distro here stays `Running` -- memory included -- until `-t` or
  `--shutdown`.
- `cm --status` prints `container system status` (services and
  runtime state), not WSL's defaults/version summary.
- No `.wslconfig` equivalent: WSL's host-side VM settings (`memory`,
  `processors`, `networkingMode`, `localhostForwarding`, …) don't
  apply -- resource defaults come from `container`, overridden
  per-install with `--cpus`/`--memory` and later via
  `container machine set`/`container distro set`. A host config file
  is investigated in [#45].
- Distros honor a subset of `wsl.conf` (hostname, mountFsTab,
  systemd/`boot.openrc` init selection, gated `[boot] command`) behind
  `--use-wsl-conf`; machines don't -- see
  [design/notes/configuration-files.md] for the per-key matrix.
- Installing a distro generates **terminal profiles** — the
  macOS-native equivalent of a `.wsl` package's `windowsterminal`
  metadata (which Windows Terminal reads): a `.terminal` launcher file
  plus an iTerm2 dynamic profile when installed, launching
  `container distro run -n NAME`. On by default; a package's
  `windowsterminal.enabled = false` opts out, and
  `cm --install --terminal-profile`/`--no-terminal-profile` (or
  `container distro set`) pins the answer. `cm --terminal` (or
  `container distro terminal NAME`) opens the distro in a Terminal.app
  window via that launcher — Terminal windows close themselves when
  the guest shell exits cleanly. Profiles import at generation time
  (silently when Terminal isn't running, via a momentary self-closing
  window when it is); deleting one in Terminal's settings is honored
  by routine refreshes, while `sync-profiles` re-imports by design.
  Machines never get profiles. Details: [container-distro.md].

[#27]: https://github.com/daphnediane/container-distro/issues/27
[#43]: https://github.com/daphnediane/container-distro/issues/43
[#44]: https://github.com/daphnediane/container-distro/issues/44
[#45]: https://github.com/daphnediane/container-distro/issues/45
[apple/container#1954]: https://github.com/apple/container/issues/1954
[cm.md]: cm.md
[container-distro.md]: container-distro.md
[design/notes/]: design/notes/
[design/notes/configuration-files.md]: design/notes/configuration-files.md
[design/notes/gui-apps.md]: design/notes/gui-apps.md
[design/upstream-limitations.md]: design/upstream-limitations.md
[distro]: machine-vs-distro.md
[machine-vs-distro.md]: machine-vs-distro.md
