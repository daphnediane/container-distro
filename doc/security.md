# Security model and analysis

Written pre-1.0 (2026-10-03) as a checkpoint: what
`cm`/`container-distro` trusts, which trade-offs are deliberate, and
what to re-evaluate before calling anything stable. Section 2 lists each
accepted trade-off with its rationale so a future decision can be made
on purpose rather than by inertia.

## Threat model

`cm` and `container-distro` are local, single-host developer tools.
Everything runs as the invoking user; there is no remote surface, no
setuid, and no daemon of ours (`container`'s services are upstream's).
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

- **Accepted because:** it is `container machine`'s own model and what
  makes the WSL-style "edit on host, build in guest" flow work. WSL has
  the same property in the other direction (guest reads/writes all of
  `C:\Users`).
- **Mitigations that exist:** `--home-mount ro|none` at create;
  `container machine set home-mount=` for machines.
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
  image pull, since the hash is pinned in the built-in catalog.
  Downloads are cached content-addressed (`<sha256>.wsl`) in the Darwin
  user cache dir; cache hits re-verify the hash, so a guest (who can
  write the cache) can delete files but cannot poison them. The
  imported rootfs still gets full provisioning,
  so `--restricted` remains the right choice for distributions you
  don't fully trust; making it the default for downloaded rootfses is
  still under consideration. `--from-file` imports local files where
  the user already has the bits, so the T4 argument above applies
  as-is. A `--catalog FILE` override can supply arbitrary `.wsl` URLs
  with attacker-chosen hashes — but a custom catalog is already an
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
  not be -- no setuid, and `sudo cm` writes state under root's home;
  consider refusing euid 0 except for `install-plugin`).

### T6. State directory lives inside the shared `$HOME` — closed ([#13])

`~/Library/Application Support/container-distro/` holds
`sbin.distro/init` and `create-user.sh` -- executed as PID 1 and as root
in every distro -- plus `default-distro` and the `preserved/` staging
area. With `home-mount=rw` all of it was guest-writable: TCC does not
protect `Application Support`, and virtiofs writes are made by the
daemon as the user, so no macOS layer intervened (verified on a running
distro). There is no standard per-user location outside `$HOME`, so the
answer was to take the files out of reach rather than move them:

- `install-plugin` writes both asset flavors next to the plugin binary
  (`<prefix>/libexec/container-plugins/distro/sbin.distro*`) -- outside
  `$HOME`, root-owned for the standard `/usr/local` install, so no share
  can reach them unless a user mounts that path on purpose.
- `assets_dir()` uses the installed copies only when byte-identical to
  this build's; a stale plugin copy warns once and falls back, so a
  newer binary never runs an older init.
- The per-user fallback (what `cm`-only installs use) is locked with the
  macOS user-immutable flag `uchg`, directory and files: guests get
  EPERM on write/unlink/rename, and virtiofs exposes no flag operations
  to clear it (verified on a running distro). Refresh unlocks, writes,
  re-locks; writes use `O_NOFOLLOW`, so a planted symlink can't turn a
  refresh into a clobber of an arbitrary user file.
- The same flag covers `preserved/<name>.ext4`/`.json` while staged --
  a guest could otherwise rewrite the spec `recover_interrupted`
  recreates from, or the filesystem it puts back -- `preserved/` stays
  locked at rest so nothing can be planted there, and `default-distro`
  is locked on write/refresh.
- `boot` refreshes whichever directory the container actually mounts:
  per-user copies upgrade (and lock) in place; installed copies warn
  once on drift.

Residuals, deliberately accepted: a bare `container start` skips the
refresh (contents can only be stale, never tampered -- the lock makes
that cosmetic); the plant window for `preserved/` shrinks to the
duration of a real `set`/`migrate`, when the dir is legitimately
unlocked; and everything *else* in `$HOME` stays guest-writable --
that's T1's model, unchanged.

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

## Restricted distros

`create --restricted` (alias `--untrusted`, also on `import` and
`cm --install`) flips the create-time defaults: `--home-mount none`,
`--network none` (loopback only -- no interface, DNS, or outbound),
`--no-ssh`, and `--no-sudo` (the assets dir mounted at `/sbin.distro`
lacks `grant-admin.sh`, so no privilege-granting code exists in the
guest). It exists for semi-trusted images -- generated code, random
images, imported rootfses (T4).

**It is not a panacea, and not a sandbox.** What it does not do:

- It's a defaults preset, not policy: `-v`/`--automount`/`-p`/
  `--home-mount`/`--network`/`--ssh`/`--sudo` reopen holes on the same
  command line, and `container distro set` reopens them later.
- It's your session either way: `container distro run --root` or
  `container exec -u 0:0` still yields guest root -- restricted only
  stops the provisioned *user* from being granted sudo.
- `--cap-add ALL` and unmasked `/proc`/`/sys` stay: init systems need
  them and caps are contained by the VM -- but "no sudo" is a
  convenience boundary inside the guest, not a wall.
- Your identity still goes in: name/uid/gid are provisioned and readable
  in `/etc/passwd`, process lists, and `CONTAINER_*` env.
- `--no-sudo` (including via `set`) prevents future grants; it cannot
  remove sudoers/doas files already written into a rootfs.
- No network also means no published ports and no in-guest package
  installs -- `--restricted` distros are bring-your-own-bits.

What remains trusted: the VM boundary (the real containment), the
`container` daemon, and the host account running the commands.

## Findings and resolutions

Numbered C1–C12 from the 2026-10-03 review. Status reflects the fix
commits that follow this document.

| #   | Severity   | Issue                                                                                                                                                  | Status                                                                                                                                      |
| --- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | Medium     | `install-plugin` symlinks `bin/distro` → a user-writable binary; other users would exec it with their privileges                                       | **Fixed** -- binary is copied, not linked                                                                                                   |
| C2  | Medium     | `id`/`sysctl` spawned via `PATH` -- hijack → code exec as user                                                                                         | **Fixed** -- libc `getpwuid_r`/`getuid`/`getgid`/`sysctlbyname`; `container` prefers `/usr/local/bin/container`                             |
| C3  | Medium     | `CONTAINER_USER`/`UID`/`GID`/`HOME` interpolated unvalidated into root-run shell code (`create-user.sh` sudoers path traversal, passwd-line injection) | **Fixed** -- charset checks in `create-user.sh` and `host_user()`                                                                           |
| C4  | Medium     | `-d`/`-s`/`-t`/`--unregister` values passed unvalidated → flag smuggling into inner `container` CLI (`cm -t=-f` → `machine stop -f`)                   | **Fixed** -- `validate_name`/`validate_user` on all passthru args                                                                           |
| C5  | Medium     | `distro export -o <dir>` hits upstream [apple/container#2325] -- `export` deletes an existing directory                                                | **Fixed** -- existing dirs rejected in `ops::export`                                                                                        |
| C6  | Low        | Distro silently shadows a machine of the same name on `-d` (warning only in `cm -l`)                                                                   | **Fixed** -- resolution-time warning when a distro shadows a machine                                                                        |
| C7  | Low        | `init -u` re-provisions on every boot: sudoers re-added, owner can't lock down their distro                                                            | **Fixed** -- per-user `/etc/.distro.user.*` sentinel; admin edits preserved                                                                 |
| C8  | Low        | Idle-PID1 loop doesn't reap zombies                                                                                                                    | **Fixed** -- bare `wait` reaps orphans reparented to PID 1                                                                                  |
| C9  | Low        | Forwarder: unbounded thread per connection, no timeouts                                                                                                | **Fixed** -- 64-conn semaphore cap; backlog queues excess (no idle timeout by design)                                                       |
| C10 | Low        | `--automount` mounts every `/Volumes/*` rw (DMGs, USB, network shares); lowercase/`→`- collisions produce duplicate targets                            | **Partial** -- case preserved, target collisions deduped + warned, `--automount ro` available; rw-all-`/Volumes` remains an accepted opt-in |
| C11 | Info       | `uninstall` check-then-delete TOCTOU; snapshot images cleaned by name prefix not label                                                                 | **Fixed** -- uninstall removes only files it installed; imported images carry a `distro` label cleanup matches                              |
| C12 | Info (bug) | `resolve_shell` probe breaks under `machine run` re-eval → always falls back to `/bin/sh`                                                              | **Fixed** -- probe passed as one pre-joined string                                                                                          |

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
5. `cargo audit`/`cargo deny` in CI; fuzz the `FromStr` parsers
   (`MountSpec`, `PublishSpec`, `PortMapping`) -- **open** ([#12])
6. Decide init-assets location vs. shared home (T6) -- **done**:
   plugin-adjacent installed assets preferred, per-user fallback
   `uchg`-locked; `preserved/` staging and `default-distro` covered by
   the same flag ([#13])
7. C8 (PID 1 zombie reaping), C11 (uninstall TOCTOU / prefix cleanup) --
   **done**; C10's rw-all-`/Volumes` surface stays an accepted opt-in
   trade-off

[#12]: https://github.com/daphnediane/container-distro/issues/12
[#13]: https://github.com/daphnediane/container-distro/issues/13
[apple/container#2325]: https://github.com/apple/container/issues/2325
[configuration-files]: gaps/configuration-files.md
[gui-apps]: gaps/gui-apps.md
[remote-wsl-interop]: remote-wsl-interop.md
[Restricted distros]: #restricted-distros
