# Interop with Remote-WSL-style editor extensions

Research notes on what it would take to make `cm`-managed container
machines a target for remote-development extensions -- Microsoft's
`ms-vscode-remote.remote-wsl`, `codeium.windsurf-remote-wsl`, Cursor's
equivalent, and the open-source `jeanp413/open-remote-wsl` (used by
VSCodium / Gitpod).

Written against `container` 1.5.x with findings verified by experiment
on an `alpine` machine. Microsoft's and Windsurf's extensions are closed
source; `open-remote-wsl` is MIT-licensed and is the readable reference
implementation -- Microsoft's extension uses the same resolver protocol
(VS Code's `RemoteAuthorityResolver`) and the same `wsl.exe` call
pattern, and Windsurf's uses the same `wsl+<distro>` authority URI
scheme.

**Tracking:** [#8]

## TL;DR

- **All of these extensions are Windows-gated.** `open-remote-wsl`
  literally starts with `if (!isWindows) return;`. They spawn `wsl.exe`
  on the machine running the editor -- they can never invoke `cm` on
  macOS directly.
- The `wsl.exe` surface they need is small (~6 invocation patterns) and
  `cm` already covers most of it. The gaps: UTF-16LE output for list
  commands, WSL-shaped `-l -v` output, `-i` stdin wiring in `cm`'s exec
  path, and a **stdout attach race in `container machine run`** that
  eats the first writes -- measured and documented below; tracked in
  [gaps/exec-stdio].
- The hard problem isn't the CLI -- it's **transport**: the server binds
  guest `127.0.0.1:<port>` and the editor connects to *its own*
  localhost, relying on WSL2 `localhostForwarding`. We have no
  forwarding ([gap doc]), so that port has to be bridged by *something*.
- Four viable architectures, roughly in increasing effort:
  1. **Don't emulate WSL at all** -- `sshd` in the guest + Remote-SSH,
     or `code tunnel` inside the machine. Works today, zero `cm`
     changes.
  2. **Fork `open-remote-wsl`** → a `cm`-native resolver extension. Runs
     on macOS, controls the install script (can bind the server to the
     machine IP directly -- no bridging), ~3 files to change. The honest
     fix.
  3. **Fork `open-remote-wsl` into an Apple `container`-native
     extension** -- same fork, but the extension talks to
     `container machine` directly instead of `cm`'s WSL façade. Strictly
     better if you're forking anyway.
  4. **Fake `wsl.exe` on Windows** proxying `ssh mac cm ...`. Works with
     *unmodified* proprietary extensions, but you own UTF-16LE encoding,
     per-call SSH overhead, and a manual port bridge.

## What the extensions actually do

All observed behavior below is from `open-remote-wsl` source
(`src/wsl/wslManager.ts`, `src/serverSetup.ts`, `src/authResolver.ts`);
Microsoft's extension is materially identical per release notes and user
logs, modulo its proprietary server.

### Activation and resolution

- Activation event `onResolveRemoteAuthority:wsl`; `activate()` bails on
  non-Windows immediately.
- A remote window is opened with URI
  `vscode-remote://wsl+<distro><path>` -- the distro name is embedded in
  the authority, e.g. `wsl+mac-alpine`.
- The resolver returns `ResolvedAuthority('127.0.0.1', listeningOn,
  connectionToken)` -- i.e. the client opens TCP to **127.0.0.1 on the
  machine running VS Code**.

### The `wsl.exe` call surface

Spawned via
`cp.spawn('wsl.exe', args, { windowsVerbatimArguments: true })` --
needs a literal `wsl.exe` PE on `PATH`; no shell, no console:

| Invocation                             | Encoding | Purpose                                                             |
| -------------------------------------- | -------- | ------------------------------------------------------------------- |
| `wsl.exe --list --verbose`             | UTF-16LE | Distro list; regex `(\*\|\s)\s+NAME STATE VERSION` (VERSION = `\d`) |
| `wsl.exe --list --online`              | UTF-16LE | Installable distros (`NAME FRIENDLY NAME` table)                    |
| `wsl.exe --set-default <d>`            | UTF-16LE | Set default                                                         |
| `wsl.exe --unregister <d>`             | UTF-16LE | Delete distro (exit code checked)                                   |
| `wsl.exe --distribution <d> -- <cmd>…` | UTF-8    | All exec: env probing and the server bootstrap                      |

Notable details:

- **UTF-16LE for list commands** -- `wsl.exe`'s real output encoding on
  Windows. A shim emitting UTF-8 decodes as mojibake and the regex
  fails.
- The `--distribution <d> -- <cmd>` form needs byte-clean stdout/stderr
  passthrough with **no TTY** and correct exit codes -- the whole
  bootstrap is one giant `bash -c '<script>'` whose stdout is scraped
  for a result block (`<id>: start` … `key==value==` … `<id>: end`).
- Distro name regex is `[\w.-]+` -- `container machine`'s `[a-z0-9-]`
  names are a compatible subset.

### The guest-side bootstrap

One `wsl.exe -d <d> -- bash -c '<~350-line script>'` call that:

1. Probes `uname -s` (must be `Linux`), `uname -m` (arm64→`arm64` build
   -- fine on Apple silicon), `/etc/os-release`.
2. Downloads the server tarball (`vscodium-reh-*`, `vscode-server`, or
   the Windsurf equivalent) with `wget`/`curl` into
   `$HOME/<serverDataFolder>/bin/<commit>`, `tar -xf` it.
3. Writes a token file, starts
   `server --start-server --host=127.0.0.1 --port=0 --connection-token-file …`,
   scrapes the log for `Extension host agent listening on <port>`.
4. Echoes the result block, then stays resident in a `sleep 300` loop
   watching the server pid -- the spawn is long-lived.

**Guest image requirements**: `bash` (not just `sh`), `wget` or `curl`,
`tar`, `grep`, `sed`, and **procps** -- the script uses
`ps -o pid,args`, which busybox `ps` doesn't support. Stock `alpine`
fails this without `apk add bash procps curl`. glibc-based images
(ubuntu/debian) also avoid the musl-server problem -- the server binary
and most extensions are glibc-linked.

## The transport problem

The server listens on **guest** `127.0.0.1:<port>`; the editor connects
to **host** `127.0.0.1:<port>`. Real WSL2 closes that gap with
`localhostForwarding`; container machines have no equivalent -- the
guest is reachable only at its own `machine`-network IP, and a process
bound to guest loopback isn't reachable even there.

So every architecture below needs a bridge for that port. Options:

- **Bind wider instead.** If we control the bootstrap script (the fork
  option), start the server with `--host=0.0.0.0` and resolve
  `ResolvedAuthority(<machine-ip>, port)` -- the machine IP *is*
  reachable from macOS. Zero bridging.
- **In-guest relay.** `socat TCP-LISTEN:<p>,bind=0.0.0.0,fork
  TCP:127.0.0.1:<p>` (or busybox `nc`) inside the machine, then
  host-side `TcpListener → machine-ip:<p>` -- a `cm forward` subcommand
  per the [port-forwarding gap doc].
- **stdio bridge.** Spawn `cm -d <m> -i -- nc 127.0.0.1 <p>` and splice
  the socket ↔ the child's stdio -- the same trick Microsoft's
  `wslExeProxy` connection method uses. Works even on minimal images
  that ship busybox `nc`; no listening socket in the guest.
- **SSH.** If the guest runs `sshd`, Remote-SSH-style forwarding
  (`ssh -L`) handles this natively -- see architecture C.

VS Code's in-editor "forwarded ports" feature hits the same wall: the
resolver's `tunnelFactory` maps remote `localhost:P` to client
`localhost:P`, assuming WSL magic. For us each tunnel needs a real
spawned forwarder -- trivial to add in a forked resolver, awkward via a
fake `wsl.exe`.

## `cm`/`container` gaps found by testing

Checked against a running `alpine` machine (`container` 1.5.x):

| Requirement                        | Status today                                                                                                                                   |
| ---------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Non-TTY exec with `--` passthrough | works -- exit codes propagate, stderr clean                                                                                                    |
| stdin → guest                      | **needs `-i`**; without it guest stdin is closed -- see [exec-stdio]                                                                           |
| stdout reliability                 | **racy** -- first writes dropped on either stream; see [exec-stdio]                                                                            |
| argv fidelity                      | **not preserved** -- `machine run` shell-evals joined argv ([apple/container#1954]); single-quoted `bash -c '…'` survives the extra eval round |
| `-l -v` WSL-format output          | absent -- `-v` ignored today; see [verbose-list]                                                                                               |
| UTF-16LE list output               | absent -- only relevant on the Windows side anyway                                                                                             |
| `-d`, `-s`, `-t`, `--unregister`   | all present and compatible                                                                                                                     |
| `--list --online`                  | no equivalent (no distro store); can return a curated image list                                                                               |

Details in [gaps/exec-stdio] -- the headline for this use case: stdout
attaches late and the **first write on either stream is dropped**, so
quick probe commands (`uname`, `command -v`, `cat /etc/os-release`) are
exactly the traffic that loses output, while the bootstrap script's
result block (echoed at the end, after downloads and sleeps) survives.
The honest fix is upstream in `machine run`'s stdio wiring; `cm`
workarounds (`-i` passthrough, attach handshake) are in the gap doc.

Concrete `cm` changes for this use case:

- Pass `-i` on the exec path (or at least when stdin isn't a TTY) -- one
  flag; details in [exec-stdio].
- Per-arg quoting in the exec path to neutralize `machine run`'s
  shell-eval round ([apple/container#1954]) -- otherwise
  `cm -e cmd "a b"` silently becomes `cmd a b`.
- Emit WSL-compatible `-l -v` (a `*`, name, `Running`/`Stopped`, version
  `2`) -- overlaps the existing verbose-list gap.
- Optionally grow `cm forward` (localhost→machine port forwarder) and/or
  a `cm -d m --stdio-connect <port>` that execs an in-guest `nc` -- the
  two bridging primitives the transports above want.

## Architectures

### A -- fake `wsl.exe` on Windows (unmodified extensions)

The only way to reach the proprietary extensions
(`ms-vscode-remote.remote-wsl`, `codeium.windsurf-remote-wsl`): make a
`wsl.exe` they can't distinguish from the real one.

- A small PE on the Windows `PATH` ahead of `System32\wsl.exe` (~100
  lines of Rust/C# -- cross-compile from this repo or write fresh). It
  translates each call into `ssh <mac> cm <args>` and proxies stdio.
- **Distro routing**: pass through `-d <real-distro>` to the genuine
  `wsl.exe` so real WSL keeps working; intercept only names we own (e.g.
  a `mac-` prefix, or a config file).
- **Encoding**: emit UTF-16LE for
  `--list`/`--set-default`/`--unregister` output; UTF-8 for exec.
  Synthesize the `-l -v` table from
  `container machine list --format json`.
- **Port bridge**: when the server port is known, the shim listens on
  Windows `127.0.0.1:<port>` and splices each connection to
  `ssh <mac> cm -d <m> -i -- nc 127.0.0.1 <port>` -- or chains an
  `ssh -L` to the Mac plus an in-guest socat/nc relay. Either way it's
  ~100 lines in the shim, since `open-remote-wsl`-style resolvers can't
  be told to look anywhere but `127.0.0.1`.
- Cost multipliers: every `wsl.exe` call is a fresh SSH session (auth
  latency; use `ControlMaster`/`ControlPersist`), and Microsoft's
  extension probes more eagerly (env checks, `wsl.exe --status`-ish
  calls) so expect a couple of extra patterns to emulate.

Verdict: feasible and proven by community shims, but the most fragile
option -- you're emulating a closed protocol at arm's length through two
process hops.

### B -- fork `open-remote-wsl` into a `cm` resolver (macOS-native)

`open-remote-wsl` is MIT. A fork becomes "Remote -- container machine":

- Delete `isWindows` gate; `wslManager` becomes a thin `cm` spawner
  (`cm -l` JSON-ish, `cm -d <m> -- <cmd>`).
- Keep `serverSetup.ts` almost verbatim; change `--host=127.0.0.1` →
  `--host=0.0.0.0` (or keep it and spawn an in-guest relay) and return
  `ResolvedAuthority(<machine-ip>, port, token)`. The resolver can run a
  `cm -d <m> -- hostname -I`-equivalent probe for the IP.
- `tunnelFactory` spawns real forwarders (`cm -d <m> -i -- nc …` stdio
  splice, or the proposed `cm forward`) per tunnel -- solves the
  forwarded-ports feature properly.
- Server install dir lands in the guest's real home (`/home/<user>`) on
  the machine's persistent disk -- survives machine restarts, not
  `--unregister`.
- Bonus: the same resolver works for editors on *any* OS, including
  Windows → Mac (bridge only needs `ssh` reachability).

The blocker: where it can run. VSCodium/code-oss forks accept resolver
extensions via `enable-proposed-api` (open-remote-wsl already requires
this). For genuine VS Code, the proprietary `ms-vscode-remote` family is
the only sanctioned remote authority -- a custom `wsl`-scheme resolver
may need a custom authority (`cm+<machine>`), which is allowed but lives
outside the official remote UX. Windsurf/Cursor extension hosts are
their own forks -- sideloading a resolver extension is worth an
experiment but unverified.

Verdict: the clean engineering path; small diff, full control of both
ends, and it makes container machines a first-class remote target on
macOS rather than an emulated Windows feature.

### B2 -- same fork, but Apple `container`-native (drop the `cm` layer)

A variant worth separating out: if you're already forking the extension,
there's no reason to route it through `cm`'s WSL façade at all. Speak to
`container` directly:

- `wslManager` becomes a `container` spawner:
  `container machine list --format json` (structured output -- richer
  than anything WSL's table gives: status, IP, default flag, disk size),
  `container machine run -n <m> -i -- <cmd>` for exec,
  `container machine create` behind the "install distro" UX. No
  UTF-16LE, no WSL table synthesis, no `cm` on the path -- `container`
  itself is the only dependency.
- Authority becomes `container-machine+<name>` (or `cm+<name>`); the
  resolver reads the machine IP straight from `machine list` JSON rather
  than probing inside the guest.
- Everything else from option B applies unchanged -- the install script,
  the `--host=0.0.0.0` + machine-IP resolve trick, `tunnelFactory`
  spawning real forwarders.
- Two bonus directions this unlocks that a `cm`-flavored fork can't take
  without doubling back:
  - **L2 later**: swap CLI scraping for `MachineAPIClient` via a small
    spawned Swift helper -- typed calls, no JSON drift risk (see
    [lower-level-integration]).
  - **Wider scope**: plain `container exec` targets
    (Dev-Containers-style remote into a single container), not just
    machines -- the extension becomes "Remote -- Apple Container," not
    "fake WSL."
- Tradeoff vs option B: `cm`'s compat surface goes unexercised (it stays
  a human-facing CLI), and the arg plumbing is duplicated rather than
  shared. But `cm` adds no capability here -- only WSL syntax, which a
  purpose-built extension doesn't need.

Verdict: if a fork happens, prefer this flavor. `cm` is the right
abstraction for a *human* typing `wsl` commands; an extension is better
off being its own thin client -- it is the integration layer.

### C -- don't emulate WSL at all (works today)

- **Remote-SSH / open-remote-ssh**: install `openssh-server` in the
  guest image (`apk add openssh` / `apt install`), `ssh <machine-ip>`
  from the editor. SSH carries the server transport *and* port
  forwarding through the channel -- the entire transport problem
  evaporates. `SSH_AUTH_SOCK` is already forwarded into machines, and
  the machine IP is reachable from macOS. Requires sshd in the image;
  per-machine IP can drift across reboots (resolve via `cm -l`).
- **Tunnels**: run `code tunnel` (or the Windsurf CLI equivalent) inside
  the guest -- connects outbound, usable from desktop or web clients,
  nothing to bridge.
- **Dev Containers**: would need a Docker-compatible API on `container`
  -- a separate, larger project.

## Licensing note

Microsoft's remote extensions are licensed for use only with genuine
Visual Studio products -- driving `ms-vscode-remote.remote-wsl` at a
fake `wsl.exe` sits in a gray zone technically even if it's your own
machine. `open-remote-wsl` is MIT and is the unambiguous base for any
fork; VSCodium is the unambiguous client for it.

## Recommendation

1. **Today**: option C -- `sshd` in the image plus open-remote-ssh gives
   the full remote-dev experience with zero `cm` work; `code tunnel` in
   the guest is the zero-install alternative.
2. **If WSL-parity UX is wanted on macOS**: fork `open-remote-wsl` --
   prefer the container-native flavor (B2), since the extension gets
   structured `container` JSON for free and the `cm` layer buys it
   nothing. If you do go through `cm`, fold in its three fixes (`-i` on
   exec, WSL `-l -v` output, upstream bug report for the [stdout race])
   -- the fork needs them regardless via `container machine run`.
3. **Option A** only if the requirement is specifically "stock VS Code /
   Windsurf on Windows, no forked client" -- it's real but it's a shim
   emulating a shim.

## Open questions

- ~~Does `container machine run`'s stdout race reproduce upstream, and
  is it already tracked?~~ Answered 2026-10-01: reproduces on 1.5.0, no
  exact upstream issue -- closest is [apple/container#1148] (`run -i`
  output truncation). File a new one; see [exec-stdio].
- Does `machine run` allocate a TTY when the caller has none, and can
  `-t` behavior affect the exec path?
- Do `windsurf-remote-wsl` / Cursor's remote use the same call surface?
  Their authority URI is `wsl+<distro>`, strongly suggesting a fork of
  the same resolver -- verify empirically before relying on it.
- Can a third-party `RemoteAuthorityResolver` be sideloaded into
  Windsurf (their extension host) or does the remote stack reject
  unknown authorities?
- `cm forward` vs in-guest relay vs `--host=0.0.0.0`: which bridging
  primitive to standardize on depends on whether the server can be told
  to bind wider -- forked script (yes) vs. proprietary extension (no).

[#8]: https://github.com/daphnediane/container-distro/issues/8
[apple/container#1148]: https://github.com/apple/container/issues/1148
[exec-stdio]: gaps/exec-stdio.md
[gap doc]: gaps/port-forwarding.md
[gaps/exec-stdio]: gaps/exec-stdio.md
[lower-level-integration]: lower-level-integration.md#l2--swift-library-products-thin-c-shim-for-rust
[port-forwarding gap doc]: gaps/port-forwarding.md
[stdout race]: gaps/exec-stdio.md
[apple/container#1954]: https://github.com/apple/container/issues/1954
[verbose-list]: gaps/verbose-list.md
