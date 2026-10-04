# Security model and analysis

Written pre-1.0 (2026-10-03) as a checkpoint: what `cm`/`container-distro`
trusts, which trade-offs are deliberate, and what to re-evaluate before
calling anything stable. Section 2 lists each accepted trade-off with its
rationale so a future decision can be made on purpose rather than by
inertia.

## Threat model

`cm` and `container-distro` are local, single-host developer tools.
Everything runs as the invoking user; there is no remote surface, no
setuid, and no daemon of ours (`container`'s services are upstream's).
Guests are lightweight VMs via Apple Containerization, so the VM boundary
does real work — but virtiofs shares, published ports, and the forwarded
SSH agent are deliberate holes punched through it.

The frame to keep in mind: **a machine or distro is not a sandbox for
untrusted code — it is a second login session for the same user.** Code
running in a guest should be treated as roughly equivalent to code run on
the host as that user.

## Accepted trade-offs

Each of these is a deliberate choice. The rationale is recorded so the
decision can be revisited — flip any of them if it stops matching how the
tool is actually used.

### T1. Guest code can execute code on the host (rw `$HOME` share)

`container machine` and distros share `/Users/<name>` read-write at the
same path. Guest code can write `~/.zshrc`, `~/.ssh/authorized_keys`,
`~/.gitconfig` (`core.hooksPath`), `~/Library/LaunchAgents` — host code
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
can authenticate *as you* — `git push` to your repos, ssh to your hosts —
for as long as the guest runs.

- **Accepted because:** it matches `container machine` and is the feature
  that makes `git`/`ssh` "just work" in-guest, a core WSL-ism.
- **Mitigations that exist:** none in-distro yet — `--no-ssh` is the
  cheap off-switch; tracked in the [checklist](#pre-10-checklist).
- **Re-evaluate if:** T1 is re-evaluated, or agent confirmation /
  per-key scoping becomes desirable. macOS's `ssh-agent` supports
  per-use confirmation which would blunt this entirely.

### T3. Passwordless sudo + `cap-add ALL` + no masked/read-only paths

Distros are created with `--cap-add ALL --masked-path NONE
--read-only-path NONE` and the provisioned user gets
`NOPASSWD:ALL` sudo/doas. Guest user → instant guest root → rw access to
every shared host path.

- **Accepted because:** mirrors `container machine`'s own configuration
  (the VM boundary, not capabilities, is what contains the guest); WSL
  users are full administrators in their distro. Kernel caps inside a VM
  are far less dangerous than in a shared-kernel container.
- **Re-evaluate if:** T1 changes — caps matter more once mounts are the
  only host surface, not less.

### T4. `import` fully provisions an untrusted rootfs

`container distro import NAME file.tar` wraps an arbitrary tar as an
image, then applies full provisioning: your uid/gid, passwordless sudo,
rw home (default), ssh agent, all caps. Importing a hostile rootfs hands
it your identity and home directory.

- **Accepted because:** WSL `--import` has the same shape and the user is
  expected to know what they're importing.
- **Mitigations that exist:** the same `--home-mount` knob; an import
  could pass `--home-mount none` + `--no-ssh` once those exist.
- **Re-evaluate if:** we add a curated/"online list" import path
  (remote-WSL interop doc mentions `--list --online`), at which point a
  safer default (`--home-mount none` for import specifically) is worth
  real consideration.

### T5. `container` is resolved from `PATH` (or `CONTAINER_CLI`)

`container`, and spawned helpers, are looked up in the environment. A
hijacked `PATH` or `CONTAINER_CLI` runs an arbitrary binary as the user.

- **Accepted because:** it is the standard Unix tool resolution model
  and `CONTAINER_CLI` is a documented feature for testing.
- **Mitigations that exist:** the installed location
  (`/usr/local/bin/container`) is preferred over PATH when present, and
  `id`/`sysctl` are no longer spawned — `getpwuid_r`, `getuid`/`getgid`,
  and `sysctlbyname` do the lookups in-process (C2). If we later need
  richer system introspection (CPU/memory/processes, e.g. watching guest
  listeners for auto port-forwarding), the `sysinfo` crate is the
  documented switch point — see `default_resources` in ops.rs.
- **Re-evaluate if:** `cm` is ever run in privileged contexts (it should
  not be — no setuid, and `sudo cm` writes state under root's home;
  consider refusing euid 0 except for `install-plugin`).

### T6. State directory lives inside the shared `$HOME`

`~/Library/Application Support/container-distro/` holds `sbin.distro/init`
and `create-user.sh` — executed as PID 1 and as root in every distro —
plus `default-distro`. With `home-mount=rw` all of it is guest-writable.

- **Accepted because:** there is no standard per-user location outside
  `$HOME` on macOS; anything user-writable is inside the share anyway,
  so T1 already covers it. `assets_dir()` rewrites the init assets on
  every boot if they differ, which breaks naive tampering but is a
  mitigation of convenience, not a boundary.
- **Re-evaluate if:** T1 changes, or the plugin install path could own
  root-owned assets (installed alongside the plugin under
  `/usr/local/libexec`) with the state-dir copy only as a fallback for
  `cm`-only installs.

### T7. Management acts on label-matched containers; no confirmations

Distro membership is `io.github.daphnediane.container-distro.distro`
labels on `container` objects; `--unregister`/`rm` delete without asking
(WSL semantics). A user can label any container and `cm` will manage it —
self-affecting only, since only the user can create containers.

- **Accepted because:** matches WSL's `--unregister` semantics and the
  label model is what makes distros discoverable at all.
- **Re-evaluate if:** shared/multi-user machines matter, or container
  gains a stronger ownership model.

## Findings and resolutions

Numbered C1–C12 from the 2026-10-03 review. Status reflects the fix
commits that follow this document.

| #   | Severity   | Issue                                                                                                                                                  | Status                                                                                                         |
| --- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| C1  | Medium     | `install-plugin` symlinks `bin/distro` → a user-writable binary; other users would exec it with their privileges                                       | **Fixed** — binary is copied, not linked                                                                       |
| C2  | Medium     | `id`/`sysctl` spawned via `PATH` — hijack → code exec as user                                                                                          | **Fixed** — libc `getpwuid_r`/`getuid`/`getgid`/`sysctlbyname`; `container` prefers `/usr/local/bin/container` |
| C3  | Medium     | `CONTAINER_USER`/`UID`/`GID`/`HOME` interpolated unvalidated into root-run shell code (`create-user.sh` sudoers path traversal, passwd-line injection) | **Fixed** — charset checks in `create-user.sh` and `host_user()`                                               |
| C4  | Medium     | `-d`/`-s`/`-t`/`--unregister` values passed unvalidated → flag smuggling into inner `container` CLI (`cm -t=-f` → `machine stop -f`)                   | Open — `validate_name` before passthru                                                                         |
| C5  | Medium     | `distro export -o <dir>` hits upstream [#2325](https://github.com/apple/container/issues/2325) — `export` deletes an existing directory                | Open — reject existing dirs in `ops::export`                                                                   |
| C6  | Low        | Distro silently shadows a machine of the same name on `-d` (warning only in `cm -l`)                                                                   | Open — warn at resolution time                                                                                 |
| C7  | Low        | `init -u` re-provisions on every boot: sudoers re-added, owner can't lock down their distro                                                            | Open — honor `/etc/.distro.initialized`                                                                        |
| C8  | Low        | Idle-PID1 loop doesn't reap zombies                                                                                                                    | Open — `CHLD` trap or `wait`-all loop                                                                          |
| C9  | Low        | Forwarder: unbounded thread per connection, no timeouts                                                                                                | Open — connection cap                                                                                          |
| C10 | Low        | `--automount` mounts every `/Volumes/*` rw (DMGs, USB, network shares); lowercase/`→`- collisions produce duplicate targets                            | Open — opt-in, document                                                                                        |
| C11 | Info       | `uninstall` check-then-delete TOCTOU; snapshot images cleaned by name prefix not label                                                                 | Open — minor                                                                                                   |
| C12 | Info (bug) | `resolve_shell` probe breaks under `machine run` re-eval → always falls back to `/bin/sh`                                                              | Open — pass probe pre-quoted                                                                                   |

## Risks introduced by gap-closing work

Each open gap adds attack surface; flagging the traps up front.

- **GUI apps ([gui-apps](gaps/gui-apps.md)).** The riskiest planned
  feature. `xhost +<guest-ip>` (the doc's "easy path") authorizes any
  VM-net client to drive the whole X session — input injection and
  screen capture. Do not ship it, even documented. The cookie path has a
  subtler problem: `~/.Xauthority` is inside the rw-shared home, so
  **every distro automatically gets host X access** (T1 + T4 combine
  badly here). And `nolisten_tcp=false` exposes X on TCP — verify the
  bind address (XQuartz historically listens on all interfaces → LAN
  exposure). Prefer: per-guest cookies in an Xauthority file outside the
  shared home, or a unix-socket forward, over TCP.
- **Remote-WSL interop ([remote-wsl-interop](remote-wsl-interop.md)).**
  `sshd` in-guest is safe bound to the machine-net IP; it becomes
  LAN-reachable the moment a user publishes 22 on `0.0.0.0`. The
  fake-`wsl.exe`-over-ssh path turns the Mac into an ssh-reachable exec
  endpoint — that *is* remote login; treat and document it as such.
- **Host config file ([configuration-files](gaps/configuration-files.md)).**
  A `~/.cmconfig.toml` is guest-writable (T6) → confused deputy: guest
  writes `mounts`/`env` into your config and the *next host `cm` call*
  grants it. Either scope dangerous keys to a file the guest can't
  reach, or treat config as hint-level trust.
- **Publishing beyond loopback.** Today nothing warns when a publish
  spec binds `0.0.0.0` — the one flag that changes exposure for *other*
  machines on the LAN. Gate it behind an explicit flag or a warning.
- **`container system start` auto-start** runs whatever `container`
  resolves to (T5) — preferring the installed path mitigates.
- **Machine `export`/`import`** (upstream-blocked): when it lands, T4's
  untrusted-rootfs warning applies to machines too.

## Pre-1.0 checklist

1. This document + README security section — **done**
2. `--no-ssh` flag (T2's off-switch; small) — **open**
3. C1–C5, C7, C9, C12 — each lands as its own commit and flips its
   status in the findings table (**done:** C1–C3; open: C4–C5, C7, C9, C12)
4. Non-loopback `--publish` warning — **open**
5. `cargo audit`/`cargo deny` in CI; fuzz the `FromStr` parsers
   (`MountSpec`, `PublishSpec`, `PortMapping`) — **open**
6. Decide init-assets location vs. shared home (T6) — **open**
7. C6/C8/C10/C11 — **open, low**
