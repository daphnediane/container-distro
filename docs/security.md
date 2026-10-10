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

- **Your home directory**, read-write, at the same path. Guest code
  can write `~/.zshrc`, `~/.ssh/authorized_keys`, `~/.gitconfig`
  (`core.hooksPath`), `~/Library/LaunchAgents` -- host code execution
  on your next login -- plus access to everything in `$HOME`, including
  `container`'s own state and the image store other containers rely
  on. `--home-mount ro|none` scopes this down; extra `-v`/`--share`/
  `--automount` mounts extend it further.
- **Your SSH agent** (`SSH_AUTH_SOCK` forwarded in). Guest code can't
  steal private keys, but can authenticate *as you* -- push to your
  repos, ssh to your hosts -- for as long as the guest runs.
  `--no-ssh` disables the forward.
- **Instant guest root.** The provisioned user has passwordless
  sudo/doas and the container runs with `--cap-add ALL` -- guest user →
  guest root in one command. That's the WSL model too; the VM boundary,
  not capabilities, is what contains the guest. `--no-sudo` skips the
  grant (see the caveats below).
- **Published ports** bind `127.0.0.1` by default -- keep it that way
  unless you want LAN-reachable services; a `0.0.0.0` bind changes
  exposure for *other* machines too.
- **Imported rootfses get all of the above.** `container distro
  import`, `cm --install --from-file`, and catalog `.wsl` installs
  provision your identity into an arbitrary rootfs -- importing an
  image is trusting it. Catalog `.wsl` downloads are SHA-256-verified
  before import, but that proves the file is the catalog's, not that
  its contents are safe.

## Scoping down

| Option                         | Effect                                                                                    |
| ------------------------------ | ----------------------------------------------------------------------------------------- |
| `--home-mount ro` or `none`    | Shrink or remove the host share (machines: `container machine set home-mount=` + restart) |
| `--no-ssh`                     | No SSH-agent forwarding                                                                   |
| `--no-sudo`                    | Skip the sudo/doas grant -- future grants only; can't remove files already in a rootfs    |
| `--network none`               | No interface, DNS, or outbound (loopback only)                                            |
| `--restricted` (`--untrusted`) | All four at once -- see below                                                             |

## Restricted distros

`create --restricted` (alias `--untrusted`, also on `import` and
`cm --install`) flips the create-time defaults: `--home-mount none`,
`--network none` (loopback only -- no interface, DNS, or outbound),
`--no-ssh`, and `--no-sudo` (the assets dir mounted at `/sbin.distro`
lacks `grant-admin.sh`, so no privilege-granting code exists in the
guest). It exists for semi-trusted images -- generated code, random
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

The full security analysis -- each accepted trade-off (T1–T8) with its
rationale and re-evaluation triggers, the review findings list
(C1–C12), and the risks that planned features would add -- is in
[design/security.md](design/security.md). Reporting: see
[CONTRIBUTING.md](../CONTRIBUTING.md#security-issues).
