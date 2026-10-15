# Security model

`cm` and `container-distro` are local, single-host developer tools.
Everything runs as the invoking user -- no setuid, no daemon of ours
(`container`'s services are upstream's), and nothing listens on the
network unless you publish a port. Outbound fetches do happen on your
behalf: OCI image pulls, catalog `.wsl` downloads, and the optional
`container` installer download -- hash/signature-verified where a pin
exists (catalog SHA-256s, the pinned installer hash), plain TLS and
registry trust otherwise.
Guests are lightweight VMs via Apple Containerization, so the VM
boundary does real work -- but virtiofs shares, published ports, and
the forwarded SSH agent are deliberate holes punched through it.

**A machine or distro is not a sandbox for untrusted code -- it is a
second login session for the same user.** Treat code running in a
guest as roughly equivalent to code run on the host as you.

## What's exposed by default

- **Your home directory** — but only when the install implies trust.
  Distros installed through a **catalog entry** (`cm --install
  <Distro>`, `container distro catalog install`) keep the WSL-style
  rw share at the same path; a catalog is a curation decision, and
  resolving through one is treated as an act of trust (see the
  `--catalog` caveat in [design/security.md](design/security.md#t4-import-fully-provisions-an-untrusted-rootfs)).
  Every other creation path — `container distro create` or
  `cm --install` with a bare image ref, `import`, `--from-file`,
  `--from-image` — defaults to `--home-mount none`, no host share at
  all. The effective `--automount` mode implies you want host paths
  shared, so home follows it (a warning is printed when it does).
  Whatever the default, an rw home lets guest code write
  `~/.zshrc`, `~/.ssh/authorized_keys`, `~/.gitconfig`
  (`core.hooksPath`), `~/Library/LaunchAgents` -- host code execution
  on your next login -- plus access to everything in `$HOME`, including
  `container`'s own state and the image store other containers rely
  on. `--home-mount ro|none` scopes this down; extra `-v`/`--share`/
  `--automount` mounts extend it further.
- **Your SSH agent** (`SSH_AUTH_SOCK` forwarded in) — but only at the
  same trust tier as an rw home: the default is on when the effective
  home-mount is `rw` (catalog installs, explicit `--home-mount rw`),
  off otherwise — under `--restricted` it's off regardless (every hole
  needs its own flag there). Guest code can't steal private keys, but can
  authenticate *as you* -- push to your repos, ssh to your hosts --
  for as long as the guest runs. `--ssh`/`--no-ssh` override.
  `container machine` is the exception: upstream forwards the agent
  at every boot with no opt-out (an upstream limitation), so
  `--machine` itself is the opt-in — `cm --install --machine` and
  `migrate --out` warn when the resolved agent setting is off, and
  only refuse an explicit `--no-ssh` pin the machine can't honor.
- **Instant guest root.** The provisioned user has passwordless
  sudo/doas and the container runs with `--cap-add ALL` -- guest user →
  guest root in one command. That's the WSL model too; the VM boundary,
  not capabilities, is what contains the guest. `--no-sudo` skips the
  grant (see the caveats below).
- **A file-access relay** (`container distro fs`): each distro
  publishes a socket the host can use to read and write its whole
  root filesystem through a file server started on demand as guest
  root. The relay is host-initiated — a guest can't reach the host
  through it — and every connection is checked against Apple's
  runtime signature plus a per-session secret. It adds nothing the
  host user can't already do with `container distro run --root`.
  Off under `--restricted`; `--no-files` turns it off.
- **Published ports** bind `127.0.0.1` by default -- keep it that way
  unless you want LAN-reachable services; a `0.0.0.0` bind changes
  exposure for *other* machines too.
- **Imported rootfses get all of the above** — except the home share,
  which defaults to `none`. `container distro
  import`, `cm --install --from-file`, and catalog `.wsl` installs
  provision your identity into an arbitrary rootfs -- importing an
  image is trusting it. Catalog `.wsl` downloads are SHA-256-verified
  before import, but that proves the file is the catalog's, not that
  its contents are safe.

## Scoping down

| Option                          | Effect                                                                                                                                                                          |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--home-mount ro` or `none`     | Shrink or remove the host share (machines: `container machine set home-mount=` + restart)                                                                                       |
| `-v SRC:DST:ro,nosuid,nodev...` | Mount options on any share -- `ro` is host-enforced; `nosuid`/`nodev`/`noexec`/`noatime` etc. are guest-side mount flags (defense in depth -- guest root can remount them away) |
| `--no-ssh`                      | No SSH-agent forwarding                                                                                                                                                         |
| `--no-sudo`                     | Skip the sudo/doas grant -- future grants only; can't remove files already in a rootfs                                                                                          |
| `--network none`                | No interface, DNS, or outbound (loopback only)                                                                                                                                  |
| `--no-files`                    | No host file-access relay (`container distro fs` refuses the distro)                                                                                                            |
| `--restricted` (`--untrusted`)  | All of the above at once -- see below                                                                                                                                           |
| `--trusted`                     | The mirror preset: rw home, rw `/Volumes` automount, agent on                                                                                                                   |

`--automount` shares get `nosuid,nodev` automatically — `/Volumes` is
data, so guest-created setuid binaries and device nodes there never
function.

The other direction: `--trusted` is the mirror preset — rw home,
`/Volumes` automounted rw, SSH agent forwarded — for images you'd
give your own login session. A machine forwards the agent
unconditionally, so `--machine` operations just warn when the
resolved setting is off; only a distro with agent forwarding
*pinned* off (`set --no-ssh`) is refused a `migrate --out` until the
pin is dropped (`set --ssh` or `set --reset ssh`).

Presets are a recorded tier, not a one-time default flip: `--preset
restricted|default|trusted` on `create`/`import`/`cm --install`
(`--restricted`/`--trusted` are aliases) is journaled on the distro,
and `container distro set --preset TIER` re-derives it later. Every
option you ever set with an explicit flag is pinned and survives a
tier change; everything else follows the tier — including implied
chains like home-mount inheriting the effective automount mode. `set
--reset OPTION` un-pins one option (back to preset/source-derived),
`--reset mounts`/`--reset publish` clear those lists, and `--reset
restricted|default|trusted` clears *every* pin and sets the tier.
Catalog installs record the entry's curated home request as `source`
intent: it fills options nothing else sets and caps what a tier
would raise, but an explicit flag always wins over it.

## Restricted distros

`create --restricted` (alias `--untrusted`, also on `import` and
`cm --install`) flips the create-time defaults: `--home-mount none`,
`--network none` (loopback only -- no interface, DNS, or outbound),
`--no-ssh`, and `--no-sudo` (the assets dir mounted at `/sbin.distro`
lacks `grant-admin.sh`, so no privilege-granting code exists in the
guest), plus `--no-files` (no host file-access relay — `--files`
opts back in). It exists for semi-trusted images -- generated code, random
images, imported rootfses.

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

## Deeper reading

The full security analysis -- each accepted trade-off (T1–T13) with its
rationale and re-evaluation triggers, the review findings list
(C1–C12), and the risks that planned features would add -- is in
[design/security.md](design/security.md). Reporting: see
[CONTRIBUTING.md](../CONTRIBUTING.md#security-issues).
