# Security model and analysis

Written pre-1.0 (2026-10-03) as a checkpoint: what
`cm`/`container-distro` trusts, which trade-offs are deliberate, and
what to re-evaluate before calling anything stable. Section 2 lists each
accepted trade-off with its rationale so a future decision can be made
on purpose rather than by inertia.

## Threat model

`cm` and `container-distro` are local, single-host developer tools.
Everything runs as the invoking user; nothing listens on the network
unless you publish a port (`cm --forward` binds loopback only, and a
non-loopback `--publish` warns but is honored), there is no setuid,
and no daemon of ours (`container`'s services are upstream's).
Outbound fetches do happen on your behalf: OCI image pulls, catalog
`.wsl` downloads, and the optional `container` installer.
Guests are lightweight VMs via Apple Containerization, so the VM
boundary does real work -- but virtiofs shares, published ports, and the
forwarded SSH agent are deliberate holes punched through it.

The frame to keep in mind: **a machine or distro is not a sandbox for
untrusted code -- it is a second login session for the same user.** Code
running in a guest should be treated as roughly equivalent to code run
on the host as that user.

## Accepted trade-offs

Each of these is a deliberate choice. The rationale is recorded so the
decision can be revisited -- flip any of them if it stops matching how
the tool is actually used.

### T1. Guest code can execute code on the host (rw `$HOME` share)

`container machine` and distros share `/Users/<name>` read-write at the
same path. Guest code can write `~/.zshrc`, `~/.ssh/authorized_keys`,
`~/.gitconfig` (`core.hooksPath`), `~/Library/LaunchAgents` -- host code
execution on next login/shell, plus direct access to everything in home.
That includes `container`'s own app support directory: a guest with an
rw share can tamper with runtime-managed state and the shared image
store that other containers and machines depend on, so an rw-home guest
is a cross-container integrity risk, not just a same-guest one.

- **Accepted because:** it is `container machine`'s own model and what
  makes the WSL-style "edit on host, build in guest" flow work. WSL has
  the same property in the other direction (guest reads/writes all of
  `C:\Users`).
- **Mitigations that exist:** `--home-mount ro|none` at create;
  `container machine set home-mount=` for machines. `ro`/`none` also
  cuts off tampering with runtime state under the share -- it is the
  actual boundary, not just privacy for your files.
- **Re-evaluate if:** users start running semi-trusted workloads
  (AI-generated code, random images, `import`ed rootfses) in distros and
  expect sandboxing. Options: default `ro`, scoped subdir shares instead
  of all of `$HOME`, or a "sandboxed distro" profile.

### T2. `SSH_AUTH_SOCK` is forwarded into every machine and distro

`create_args` passes `container`'s `--ssh` unconditionally; machines get
it from the machine plugin. Guest code cannot extract private keys, but
can authenticate *as you* -- `git push` to your repos, ssh to your
hosts -- for as long as the guest runs.

- **Accepted because:** it matches `container machine` and is the
  feature that makes `git`/`ssh` "just work" in-guest, a core WSL-ism.
- **Mitigations that exist:** `--no-ssh` at create or `set --no-ssh`;
  `--restricted` turns it off along with everything else.
- **Re-evaluate if:** T1 is re-evaluated, or agent confirmation /
  per-key scoping becomes desirable. macOS's `ssh-agent` supports
  per-use confirmation which would blunt this entirely.

### T3. Passwordless sudo + `cap-add ALL` + no masked/read-only paths

Distros are created with `--cap-add ALL --masked-path NONE --read-only-path NONE`
and the provisioned user gets `NOPASSWD:ALL` sudo/doas. Guest user →
instant guest root → rw access to every shared host path.

- **Accepted because:** mirrors `container machine`'s own configuration
  (the VM boundary, not capabilities, is what contains the guest); WSL
  users are full administrators in their distro. Kernel caps inside a VM
  are far less dangerous than in a shared-kernel container.
- **Mitigations that exist:** `--no-sudo`/`--restricted` skips the
  privilege grant -- restricted distros mount an init-assets dir that
  doesn't contain `grant-admin.sh`, so no provisioning code in the guest
  creates sudoers at all. Caps stay: init systems need them and the VM
  contains them -- see [Restricted distros] for why that's still not a
  sandbox.
- **Re-evaluate if:** T1 changes -- caps matter more once mounts are the
  only host surface, not less.

### T4. `import` fully provisions an untrusted rootfs

`container distro import NAME file.tar` wraps an arbitrary tar as an
image, then applies full provisioning: your uid/gid, passwordless sudo,
rw home (default), ssh agent, all caps. Importing a hostile rootfs hands
it your identity and home directory.

- **Accepted because:** WSL `--import` has the same shape and the user
  is expected to know what they're importing.
- **Mitigations that exist:** `import --restricted` wraps the rootfs
  with no mounts, no network, no agent, and no privilege grant -- the
  intended path for rootfses you didn't build.
- **Remote `.wsl` catalog entries** (landed): catalog entries without
  an `Image` carry `Amd64Url`/`Arm64Url` downloads with a **mandatory
  `Sha256`**, fetched in-process via ureq (platform trust store, env
  proxies) and verified before import — the trust equivalent of an
  image pull, since the hash is pinned in the built-in catalog. Both
  `cm --install` and `container distro catalog install` use that path.
  Downloads are cached content-addressed (`<sha256>.wsl`) in the Darwin
  user cache dir; cache hits re-verify the hash, so a guest (who can
  write the cache) can delete files but cannot poison them. The
  imported rootfs still gets full provisioning, so `--restricted`
  remains the right choice for distributions you don't fully trust;
  making it the default for downloaded rootfses is still under
  consideration. `--from-file` and `container distro import` handle
  local files where the user already has the bits, so the T4 argument
  above applies as-is. A `--catalog FILE` override on either CLI can
  supply arbitrary `.wsl` URLs with attacker-chosen hashes — but a custom
  catalog is already an
  explicit act of trust (it can equally point `Image` at a hostile
  registry), so pinning to a caller-supplied manifest adds no new
  trust.

### T5. `container` is resolved from `PATH` (or `CONTAINER_CLI`)

`container`, and spawned helpers, are looked up in the environment. A
hijacked `PATH` or `CONTAINER_CLI` runs an arbitrary binary as the user.

- **Accepted because:** it is the standard Unix tool resolution model
  and `CONTAINER_CLI` is a documented feature for testing.
- **Mitigations that exist:** the installed location
  (`/usr/local/bin/container`) is preferred over PATH when present, and
  `id`/`sysctl` are no longer spawned -- `getpwuid_r`,
  `getuid`/`getgid`, and `sysctlbyname` do the lookups in-process (C2).
  If we later need richer system introspection (CPU/memory/processes,
  e.g. watching guest listeners for auto port-forwarding), the `sysinfo`
  crate is the documented switch point -- see `default_resources` in
  ops.rs.
- **Re-evaluate if:** `cm` is ever run in privileged contexts (it should
  not be -- no setuid, and `sudo cm` writes state under root's home).
  `install-plugin`/`uninstall-plugin` are the deliberate exception:
  when the plugin dir isn't user-writable they re-exec
  `/usr/bin/sudo <self> … --plugin-dir <root>` so *only* the root-owned
  file ops run as root -- `container` resolution, and the T8 install
  offer, always run as the invoking user first (under `sudo` the offer
  can't fire and `env_reset`/`secure_path` would resolve a different
  `container`). The re-exec'd binary is the user-writable
  cargo-installed one -- identical trust to the documented
  `sudo container-distro install-plugin` it replaced.

### T6. State directory lives inside the shared `$HOME` — closed ([#13])

`~/Library/Application Support/container-distro/` holds
`sbin.distro/init` and `create-user.sh` -- executed as PID 1 and as root
in every distro -- plus `default-distro`, the `distros/` journals, and
the `journalized` migration marker. With `home-mount=rw` all of it was
guest-writable: TCC does not
protect `Application Support`, and virtiofs writes are made by the
daemon as the user, so no macOS layer intervened (verified on a running
distro). There is no standard per-user location outside `$HOME`, so the
answer was to take the files out of reach rather than move them:

- Each distro's init assets live **inside its journal dir** at
  `distros/<name>/sbin.distro/` -- refreshed on every materialize from
  this build's embedded copies, so every distro mounts assets that
  match the running binary. Like a machine bundle's `sbin.machine`,
  the assets are per-distro, not shared: nothing outside the journal
  needs to agree with the installed version, and the restricted
  flavor is just that same dir without `grant-admin.sh`.
- The per-distro assets are locked with the macOS user-immutable flag
  `uchg`, directory and files: guests get EPERM on write/unlink/rename,
  and virtiofs exposes no flag operations to clear it (verified on a
  running distro). Refresh unlocks, writes, re-locks; writes use
  `O_NOFOLLOW`, so a planted symlink can't turn a refresh into a
  clobber of an arbitrary user file.
- The same flag covers each distro's journal —
  `~/Library/Application Support/container-distro/distros/<name>/`
  (`spec.json` + `rootfs.ext4`), all `uchg`-locked at rest. (Not
  `appRoot/plugin-state/` — that's another app's data folder, and an
  unentitled client writing there trips macOS's app-data consent
  prompt.) A guest could otherwise
  rewrite the spec a distro recreates from, or plant journal entries a
  later read would trust. `spec.json` doubles as the `flock` target —
  the fd is opened `O_RDWR|O_NOFOLLOW` while unlocked, then re-locked,
  and writes through the held fd are still honored, so the journal
  stays locked *and* unlinkable mid-operation (a freestanding lockfile
  could be unlinked under the lock, voiding exclusion). `default-distro`
  is locked on write/refresh.
- `boot` refreshes whichever directory the container actually mounts:
  the per-distro `sbin.distro` is refreshed in place (and re-locked);
  containers materialized before the change may still mount the legacy
  shared copies (per-user `sbin.distro*/` or the plugin-dir installs),
  which get the old upgrade-and-lock or warn-on-drift treatment until a
  `set`/recreate moves them onto per-distro assets.

The pre-journal world gets exactly one trusted upgrade: a
`journalized` marker (itself `uchg`-locked, so unforgeable) gates a
one-time pass that adopts journal-less distro containers and folds
staged `preserved/` pairs — and only pairs whose `.json` carries
`uchg`, since the flag proves ours. Afterwards a journal-less labeled
container is refused: it can only be planted, so `container distro
migrate NAME` is the explicit opt-in. The pass sunsets after 1.1.x.
Separately, `start` diffs a found-running container's reconstructed
spec against the journal and warns per-field — drift is loud, not
silently adopted.

Residuals, deliberately accepted: a bare `container start` skips the
refresh (contents can only be stale, never tampered -- the lock makes
that cosmetic); journal dirs unlock briefly during real mutations; and
everything *else* in `$HOME` stays guest-writable -- that's T1's model,
unchanged.

Alternatives considered and rejected: `container cp` into the rootfs
(`cp` requires a running container, and `init` must exist before the
first start), and masking the state dir behind an inner mount (guest
root has `CAP_SYS_ADMIN` under `--cap-add ALL` and can simply `umount`
it -- cosmetic, not a boundary).

### T7. Management acts on label-matched containers; no confirmations

Distro membership is `io.github.daphnediane.container-distro.distro`
labels on `container` objects; `--unregister`/`rm` delete without asking
(WSL semantics). A user can label any container and `cm` will manage
it -- self-affecting only, since only the user can create containers.

- **Accepted because:** matches WSL's `--unregister` semantics and the
  label model is what makes distros discoverable at all.
- **Re-evaluate if:** shared/multi-user machines matter, or container
  gains a stronger ownership model.

### T8. `cm` can fetch and open Apple's `container` installer itself

When `container` can't be resolved, an interactive non-root session
offers to download the signed installer for the pinned release
(`PINNED_CONTAINER_VERSION`) and `open` it in Installer.app -- a
root-requiring installer is the largest trust step this codebase takes
on the user's behalf.

- **Accepted because:** `container` is a hard prerequisite (nothing
  works without it), and the offer is strictly opt-in -- one y/N
  prompt per invocation, never in a pipeline or as root.
- **Mitigations that exist:** the release and its SHA-256 are pinned
  in the binary (`PINNED_CONTAINER_VERSION`/`PKG_SHA256`), so a
  tampered or replaced asset fails verification before anything runs;
  `pkgutil --check-signature` must report a Developer ID signature
  before `open`; and the handoff is to Installer.app -- which
  re-verifies notarization and collects admin credentials itself --
  never `sudo installer`. The platform is checked first (arm64 +
  macOS 26 via `sysctlbyname`), `CONTAINER_DISTRO_NO_INSTALL_OFFER`
  opts out, and the "newer release exists" heads-up only reads
  api.github.com JSON -- nothing from the response executes. The pkg
  lands in the per-user Darwin cache as a *sibling* of the `.wsl`
  cache, invisible to `--list --cache`/`--purge-cache`, and is
  re-verified on reuse.
- **Residuals:** the pin deliberately can lag upstream -- the offer
  installs the last *verified* release and warns when it's behind
  ("check for an updated container-distro"), so freshness is a
  release-cadence property, not a runtime check. `open`ing the pkg is
  the user's explicit yes, but the installer's contents are still
  upstream's trust: the pinned hash ties that to what we verified.

## Restricted distros

The user-facing description of `--restricted` -- what it sets and what
it does *not* do -- lives in [the security guide](../security.md#restricted-distros).
Design-side, the trade-off it manages is T4 (untrusted rootfs import),
and T3 covers why "no sudo" is a convenience boundary rather than a
wall.

## Findings and resolutions

Numbered C1–C12 from the 2026-10-03 review. Status reflects the fix
commits that follow this document.

| #   | Severity   | Issue                                                                                                                                                  | Status                                                                                                                                                                                                                                                                   |
| --- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| C1  | Medium     | `install-plugin` symlinks `bin/distro` → a user-writable binary; other users would exec it with their privileges                                       | **Fixed** -- binary is copied, not linked                                                                                                                                                                                                                                |
| C2  | Medium     | `id`/`sysctl` spawned via `PATH` -- hijack → code exec as user                                                                                         | **Fixed** -- libc `getpwuid_r`/`getuid`/`getgid`/`sysctlbyname`; `container` prefers `/usr/local/bin/container`                                                                                                                                                          |
| C3  | Medium     | `CONTAINER_USER`/`UID`/`GID`/`HOME` interpolated unvalidated into root-run shell code (`create-user.sh` sudoers path traversal, passwd-line injection) | **Fixed** -- charset checks in `create-user.sh` and `host_user()`                                                                                                                                                                                                        |
| C4  | Medium     | `-d`/`-s`/`-t`/`--unregister` values passed unvalidated → flag smuggling into inner `container` CLI (`cm -t=-f` → `machine stop -f`)                   | **Fixed** -- `validate_name`/`validate_user` on all passthru args                                                                                                                                                                                                        |
| C5  | Medium     | `distro export -o <dir>` hits upstream [apple/container#2325] -- `export` deletes an existing directory                                                | **Fixed** -- existing dirs rejected in `ops::export`                                                                                                                                                                                                                     |
| C6  | Low        | Distro silently shadows a machine of the same name on `-d` (warning only in `cm -l`)                                                                   | **Fixed** -- resolution-time warning, both backends annotated in listings/info; creating a same-named pair is now refused everywhere (`create`/`import`/`catalog install`/`migrate`/`rename`, `cm --install/--import/--rename`), with `--force` as the deliberate opt-in |
| C7  | Low        | `init -u` re-provisions on every boot: sudoers re-added, owner can't lock down their distro                                                            | **Fixed** -- per-user `/etc/.distro.user.*` sentinel; admin edits preserved                                                                                                                                                                                              |
| C8  | Low        | Idle-PID1 loop doesn't reap zombies                                                                                                                    | **Fixed** -- bare `wait` reaps orphans reparented to PID 1                                                                                                                                                                                                               |
| C9  | Low        | Forwarder: unbounded thread per connection, no timeouts                                                                                                | **Fixed** -- 64-conn semaphore cap; backlog queues excess (no idle timeout by design)                                                                                                                                                                                    |
| C10 | Low        | `--automount` mounts every `/Volumes/*` rw (DMGs, USB, network shares); lowercase/`→`- collisions produce duplicate targets                            | **Partial** -- case preserved, target collisions deduped + warned, `--automount ro` available; rw-all-`/Volumes` remains an accepted opt-in                                                                                                                              |
| C11 | Info       | `uninstall` check-then-delete TOCTOU; snapshot images cleaned by name prefix not label                                                                 | **Fixed** -- uninstall removes only files it installed; imported images carry a `distro` label cleanup matches                                                                                                                                                           |
| C12 | Info (bug) | `resolve_shell` probe breaks under `machine run` re-eval → always falls back to `/bin/sh`                                                              | **Fixed** -- probe passed as one pre-joined string                                                                                                                                                                                                                       |

## Risks introduced by gap-closing work

Each open gap adds attack surface; flagging the traps up front.

- **GUI apps ([gui-apps]).** The riskiest planned feature.
  `xhost +<guest-ip>` (the doc's "easy path") authorizes any VM-net
  client to drive the whole X session -- input injection and screen
  capture. Do not ship it, even documented. The cookie path has a
  subtler problem: `~/.Xauthority` is inside the rw-shared home, so
  **every distro automatically gets host X access** (T1 + T4 combine
  badly here). And `nolisten_tcp=false` exposes X on TCP -- verify the
  bind address (XQuartz historically listens on all interfaces → LAN
  exposure). Prefer: per-guest cookies in an Xauthority file outside the
  shared home, or a unix-socket forward, over TCP.
- **Remote-WSL interop ([remote-wsl-interop]).** `sshd` in-guest is safe
  bound to the machine-net IP; it becomes LAN-reachable the moment a
  user publishes 22 on `0.0.0.0`. The fake-`wsl.exe`-over-ssh path turns
  the Mac into an ssh-reachable exec endpoint -- that *is* remote login;
  treat and document it as such.
- **Host config file ([configuration-files]).** A `~/.cmconfig.toml` is
  guest-writable (T6) → confused deputy: guest writes `mounts`/`env`
  into your config and the *next host `cm` call* grants it. Either scope
  dangerous keys to a file the guest can't reach, or treat config as
  hint-level trust. The same trap is why the `--catalog` distro-catalog
  override is an explicit per-invocation flag with no `$HOME` search
  path — a guest-rewritten default catalog would point familiar names
  at hostile images.
- **Publishing beyond loopback.** `create`/`import`/`migrate`/`set` warn
  when a publish spec binds a non-loopback address, and
  `start`/first-run reports each published listener (loopback as info,
  wider as a warning). Accepted as warn-only: `0.0.0.0` is sometimes the
  point.
- **`container system start` auto-start** runs whatever `container`
  resolves to (T5) -- preferring the installed path mitigates.
- **Machine `export`/`import`** (upstream-blocked): when it lands, T4's
  untrusted-rootfs warning applies to machines too.

## Pre-1.0 checklist

1. This document + README security section -- **done**
2. `--no-ssh` flag (T2's off-switch; small) -- **done**
   (`--ssh`/`--no-ssh` plus `--restricted`; also `--network`,
   `--sudo`/`--no-sudo`, and the `grant-admin.sh` asset split)
3. C1–C5, C7, C9, C12 -- each lands as its own commit and flips its
   status in the findings table (**done:** C1–C5, C6, C7, C9, C10
   partial, C12)
4. Non-loopback `--publish` warning -- **done** (warn at create/set; a
   listening report at boot surfaces every published port)
5. `cargo audit`/`cargo deny`; fuzz the `FromStr` parsers
   (`MountSpec`, `PublishSpec`, `PortMapping`) -- **done**: proptest
   targets run inside `cargo test` ([#17]), the audit/deny gate is
   scripted in `scripts/release-gate.sh` ([#16]), and CI automates
   both on push/PR plus a weekly advisory scan ([#12])
6. Decide init-assets location vs. shared home (T6) -- **done**:
   plugin-adjacent installed assets preferred, per-user fallback
   `uchg`-locked; journal state and `default-distro` covered by the
   same flag ([#13])
7. C8 (PID 1 zombie reaping), C11 (uninstall TOCTOU / prefix cleanup) --
   **done**; C10's rw-all-`/Volumes` surface stays an accepted opt-in
   trade-off
8. Nudge semi-trusted workloads off rw home shares (T1) -- **open**
   ([#19])
9. Adopt the machine model's anti-persistence defenses for distros --
   **done**: stopped distros are journal + disk under `plugin-state/
   container-distro/`, materialized fresh at every start ([#18])
10. Integrity-check distro-visible state at `boot`/`start` -- **done**:
    `start` diffs a running container's reconstructed spec against the
    journal and warns per-field ([#20])

[#12]: https://github.com/daphnediane/container-distro/issues/12
[#13]: https://github.com/daphnediane/container-distro/issues/13
[#16]: https://github.com/daphnediane/container-distro/issues/16
[#17]: https://github.com/daphnediane/container-distro/issues/17
[#18]: https://github.com/daphnediane/container-distro/issues/18
[#19]: https://github.com/daphnediane/container-distro/issues/19
[#20]: https://github.com/daphnediane/container-distro/issues/20
[apple/container#2325]: https://github.com/apple/container/issues/2325
[configuration-files]: notes/configuration-files.md
[gui-apps]: notes/gui-apps.md
[remote-wsl-interop]: remote-wsl-interop.md
[Restricted distros]: ../security.md#restricted-distros
