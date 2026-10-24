# Security model and analysis

<!-- cspell:ignore UPBK2H6LZM -->

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
`.wsl` downloads, self-update release downloads (`cm --update`), and
the optional `container` installer.
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
- **Partially re-evaluated (0.6.0, #19):** the default is now
  *source-dependent* rather than uniform `rw`. Installs resolved
  through a catalog entry keep `rw` — a catalog is already an act of
  trust (T4), so the curated path keeps the WSL ergonomics — while
  every ad-hoc path (`create`/`cm --install` with a bare image ref,
  `import`, `--from-file`, `--from-image`) defaults to `none`. A
  catalog entry can also pin a different per-entry default via the
  schema's optional `HomeMount` field. Resolution order (`DistroSpec::resolve`,
  the single implementation every path shares): explicit flag, then
  `min(preset → inherited, source)` — the catalog entry's request
  caps what a preset or the automount-inherit would raise and fills
  the slot when neither produced a value — then the built-in
  default. Home inherits the *effective* automount mode (`rw`
  volumes are `rw`-level host exposure anyway), and the preset is
  journaled on the distro so `set --preset` re-derives every option
  the user hasn't pinned — `explicit` intent always beats tier and
  source. `--trusted` is the mirror preset: rw home, rw automount,
  agent on — the deliberate "I trust this image" escape hatch.
  Effective values bind at create into the spec, so `set`
  recreations never silently change an existing distro's posture.
- **Re-evaluate if:** the catalog-trust heuristic stops matching how
  distros actually get installed, or scoped subdir shares (home `ro` +
  a narrow rw overlay — expressible today) deserve a first-class flag.
  Remaining hole: catalog `Image` refs are pulled **by tag**, which
  is mutable — see T4's "known hole" note.

### T2. `SSH_AUTH_SOCK` is forwarded into every machine and distro

`create_args` passes `container`'s `--ssh` unconditionally; machines get
it from the machine plugin. Guest code cannot extract private keys, but
can authenticate *as you* -- `git push` to your repos, ssh to your
hosts -- for as long as the guest runs.

- **Accepted because:** it matches `container machine` and is the
  feature that makes `git`/`ssh` "just work" in-guest, a core WSL-ism.
- **Mitigations that exist:** `--no-ssh` at create or `set --no-ssh`;
  `--restricted` turns it off along with everything else.
- **Narrowed (0.6.0, #19):** the forward now defaults on only when the
  effective home-mount is `rw` — the agent is "authenticate as you",
  the same exposure tier as write access to your home, so the two
  share one trust knob. Ad-hoc sources (default `none`) and
  `ro`-pinned catalog entries lose the forward unless `--ssh` opts
  back in; `--home-mount rw` on any source opts both in at once.
  `--restricted` beats the tier: every hole needs its own explicit
  flag there, so `restricted + --home-mount rw` still defaults the
  agent off.
  **Machines are the exception:** `container machine` forwards
  `SSH_AUTH_SOCK` from the caller's env at *every* boot with no flag
  to disable it (upstream limitation), so `--machine` itself is the
  opt-in: `cm --install --machine` and `migrate --out` resolve `ssh`
  through the same chain a distro would, **warn** when it lands off,
  and **refuse** only an explicit opt-out — `--no-ssh` at install,
  or a distro whose `explicit.ssh` is pinned off on migrate. An
  unpinned ssh-off distro migrates with a warning rather than being
  refused, since asking for a machine already accepts the forward.
  (`machine set home-mount=` on an existing machine has the same
  asymmetry and can't be policed — upstream's model.)
- **Re-evaluate if:** upstream grows a per-machine ssh opt-out, or
  agent confirmation / per-key scoping becomes desirable. macOS's
  `ssh-agent` supports per-use confirmation which would blunt this
  entirely.

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
image, then applies provisioning: your uid/gid, passwordless sudo, ssh
agent, all caps. Since 0.6.0 the home share defaults to `none` on this
path (T1) — but importing a hostile rootfs still hands it your identity.

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
  trust. The same act-of-trust framing is why catalog-resolved
  installs keep the `rw` home default (T1): a user-supplied catalog
  gets the same relaxed default for whatever it lists, which is an
  accepted consequence of documenting `--catalog` as "I trust this
  manifest" rather than adding a separate trust bit.
- **Known hole — tag mutability.** Unlike `.wsl` entries, catalog
  `Image` refs carry no integrity pin: pulls resolve the tag at pull
  time, so whatever the tag points to *then* is what lands. The
  accepted exposure is a compromised or misused registry namespace
  (Docker Hub controls who can push to `library/*`; other registries
  have equivalent namespacing), which is a different — and arguably
  weaker — guarantee than the `.wsl` SHA-256s. Digest pinning was
  considered and punted for two reasons: upstream `container`
  resolves `name:tag@sha256:…` **by tag** (`Reference::normalize`
  prefers `_tag`), so an embedded pin only enforces if we construct
  tagless `name@digest` refs or verify post-pull ourselves — plumbing
  for marginal benefit over "the tag was probably fine"; and pins go
  stale on every upstream republish (weekly-to-monthly for most
  catalog entries), which is a maintenance treadmill for integrity
  that registries partially provide by namespace control anyway.
  Signature verification (cosign/notation via OCI referrers — a
  referrers client already exists in ContainerizationOCI) is the
  stronger long-term answer and is tracked as a separate item. If
  pinning is revisited, store `Digest` as a separate catalog field
  and pull `name@digest` — never `name:tag@digest`.
- **Strategic direction — implemented.** `.wsl` rootfs packages are
  the preferred catalog source where they exist: they have a vendor
  release cadence, a mandatory SHA-256 in the schema, and a
  content-addressed download cache — the verified-bits property
  `Image` entries lack. The built-in catalog is now Microsoft's own
  `DistributionInfo.json` (vendored verbatim in `vendor/microsoft/`
  under its MIT license) merged with a small overlay; `Entry::source`
  prefers the arch's `.wsl` and falls back to `Image`, so `Image`
  survives only for entries/archs with no `.wsl`. The overlay can
  never override an upstream field — a stale overlay can't downgrade
  a verified download — and `scripts/check-catalog-freshness.sh`
  gates releases on the vendored copy matching upstream. A custom
  `--catalog FILE` still replaces the merge outright.

### T5. `container` is resolved from `PATH` (or `CONTAINER_CLI`)

`container`, and spawned helpers, are looked up in the environment. A
hijacked `PATH` or `CONTAINER_CLI` runs an arbitrary binary as the user.

- **Accepted because:** it is the standard Unix tool resolution model
  and `CONTAINER_CLI` is a documented feature for testing.
- **Mitigations that exist:** the installed location
  (`/usr/local/bin/container`) is preferred over PATH when present;
  `CONTAINER_CLI` redirects are honored only under `--dev` on that
  invocation — a value planted in a shell profile is ignored with a
  warning — except values equal to the default resolution (`container`
  or the installed path), which can't redirect anything anyway; and
  `id`/`sysctl` are no longer spawned -- `getpwuid_r`,
  `getuid`/`getgid`, and `sysctlbyname` do the lookups in-process (C2).
  If we later need richer system introspection (CPU/memory/processes,
  e.g. watching guest listeners for auto port-forwarding), the `sysinfo`
  crate is the documented switch point -- see `default_resources` in
  `src/distro/host.rs`.
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
- The terminal-profile launchers get the same treatment — they run on
  the *host* under the user's session, so a guest that could rewrite
  them through a shared home would own the next profile launch.
  `uchg` covers `terminal-profiles/*.terminal`,
  `terminal-cmds/*.sh`, the iTerm2 dynamic-profile file, and
  the `~/.container-distro` alias symlink (via `chflags -h` —
  `chflags(2)` follows links, so flagging the link naively locks the
  state dir instead). The silent import no longer rewrites the
  launcher: it arms a `profile-import` sentinel inside the locked
  journal dir, which the import window's `run --profile-launch`
  consumes (unlink — exactly one launch wins) before exiting. A
  sentinel is only honored before its deadline (one minute), and only
  by a launch with no command, so the worst a planted one could do is
  end one session at once — and the locked journal dir keeps a guest
  from planting it.
- What a launcher *runs* is the binary that wrote it, canonicalized
  (`layout::self_invocation`) — `<container-distro> run -n NAME`, or
  the plugin copy's own path. Earlier
  launchers baked a sibling `cm` or else the first `cm` on `PATH`,
  which let whatever shadowed `cm` on `PATH` at generation time become
  a locked, host-run launcher target. There is no lookup now.
- **Alias scripts** (`install-alias`, `cm --install-alias`) are the
  same shape — `exec '<canonical binary>' --invoked-as "$0" [cli]
  "$@"`, written `O_NOFOLLOW` (C13) — but are *not* `uchg`-locked:
  they live in bin directories next to the binaries they run, which
  a rw-shared home exposes equally (`~/.cargo/bin`; T1), and locking
  them would break `cargo install`/uninstall. Removal only deletes a
  file carrying our marker line or a legacy symlink to this binary.
  The hidden leading `--invoked-as PATH` is honored only as
  `argv[1]`, so a guest command line can't carry it, and it only
  picks the directory a *bare-name* alias is written to (user
  privilege, the alias's own directory). `install-all` replaces a
  pre-alias `cm` *binary* only when it is a Mach-O file embedding our
  label prefix — read, never executed — and `--stage` bakes the
  `--prefix` path into aliases, never the staging copy.

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

### T9. The release .pkg installs root-owned files without prompts

`scripts/build-pkg.sh` produces a payload-only `.pkg`: `cm`,
`container-distro`, the `container distro` plugin registration, man
pages, and completions land under `/usr/local` owned by root, on the
admin authentication Installer.app collects.

- **Accepted because:** it is the same layout `cargo install` +
  `install-plugin` produces -- the payload is literally staged by those
  commands, and the plugin copy keeps the copy-not-symlink property
  (C1). No install scripts ship, so the package adds no code-execution
  surface beyond the binaries themselves, and the shipped uninstaller
  is receipt-driven — it removes only what `pkgutil --files` reports
  and only empty dirs. Installing our files into
  `libexec/container-plugins/` before `container` exists is inert --
  nothing scans that dir until Apple's pkg lands.
- **Residuals:** unsigned until a Developer ID exists, so installing
  means bypassing Gatekeeper (`sudo installer -pkg` or right-click →
  Open) -- a user step, like trusting the repo. `cargo binstall`
  tarballs skip quarantine entirely (CLI downloads carry no quarantine
  xattr) -- same trust level as `cargo install` compiling the source it
  was pointed at. Once signed: binaries get hardened runtime, the pkg a
  Developer ID Installer signature, and notarization + stapling gate
  both ([packaging]).

### T10. Self-update fetches and installs a root-requiring pkg

`cm --update` / `container distro update` (the `wsl --update` analog)
download a release `.pkg` from GitHub and hand it to Installer.app.
Unlike the T8 offer there is no pinned hash — "latest" isn't knowable
at build time — so this is the largest unpinned trust step we take.

- **Accepted because:** self-update is the expected `wsl --update`
  surface, the pkg flow is the documented install path anyway, and the
  request is always explicit — the command exists only because the
  user typed it.
- **Mitigations that exist:** the downloaded bytes must match the
  SHA-256 the GitHub API reports for the asset (so a swapped CDN file
  fails before anything runs); the signed asset must pass `pkgutil
  --check-signature` with a Developer ID leaf; the leaf's team ID is
  compared against the installed binary's `codesign` `TeamIdentifier`
  — a validly-signed pkg from a *different* developer team prompts
  instead of installing silently; the unsigned CI build, downgrades,
  and same-version reinstalls each need an interactive yes (`--force`
  skips, non-interactive sessions get an error instead of an EOF
  prompt); running as root is refused; and the handoff is an `exec`
  into `open -a /System/Library/CoreServices/Installer.app` — the
  process is replaced, so nothing of ours runs while the privileged
  install proceeds, and Installer.app re-verifies notarization and
  collects admin credentials itself. We never invoke `sudo
  installer` — `installer`(8) can't self-elevate — so a failed `exec`
  just prints the verified path and the manual command.
- **Residuals:** the chain of custody rests on api.github.com + GitHub
  Releases returning the right bytes/digest — a maintainer-account or
  release compromise is upstream of every check (the team check only
  helps if the attacker can't sign as the same team). A cargo/dev
  install has no receipt baseline, so the same-team check is skipped
  for it (it warns it's installing a pkg next to the cargo copy and
  asks first). `CONTAINER_DISTRO_RELEASES_URL` redirects release
  lookups for local testing (DEV.md), honored only behind an explicit
  `--dev` on that invocation — a bare env var could be planted in a
  shell profile to steer a later `cm --update` at an untrusted
  server, so the override must never fire silently.

### T11. Trusted-tier distros can run image-supplied boot config

Honoring `/etc/wsl.conf` lets a rootfs pick its own init, hostname,
fstab mounts — and under the `command` feature, run `[boot] command`
as root at every boot. Guest config is guest content: the file that
configures a distro is itself attacker-controlled whenever the rootfs
is.

- **Accepted because:** a trusted/catalog image *already* runs its own
  code as guest root (its init, its login shells) — `[boot] command`
  changes nothing an image couldn't do by shipping a different init.
  The honored set is guest-local by construction (hostname, `mount -a`,
  init selection): nothing the file can say reaches host mounts,
  host networking, or published ports.
- **Mitigations that exist:** honoring is a journaled mode expression
  (exact set / all-except / tier-relative adjustments), not a bare
  switch — and `command` is only a functional toggle inside it, not a
  separate trust gate, because the image's init already runs as root.
  Tier resolution puts `restricted`'s empty set under every
  source ask, so no source can reopen a restricted distro; ad-hoc
  creates honor nothing unless `--use-wsl-conf` opts in. The one
  host-visible key (`automount.enabled=false`) can only *lower*
  sharing and loses to every explicit flag — the T6 rule. Snapshots
  are runtime-mediated `container cp` (no guest binary executes to
  produce them), size-capped, `uchg`-locked, parse-only, and never
  consumed for mount/network/publish decisions. `oobe.command` is
  never executed — an imported rootfs doesn't get to run code before
  you ask.
- **Residuals:** the gate is `CONTAINER_WSL_CONF` env on the init
  exec, not an unmounted asset — the claim is "init never opens the
  file", not "the guest can't read it" (guest root can read its own
  filesystem regardless). A symlinked `/etc/wsl.conf` copies whatever
  it points at into the journal — bounded by the 64 KiB cap and the
  parse-only consumer. `[boot] command` is image code running as
  root, accepted only at the same trust tier that already runs the
  image's own init.

### T12. Packaged conf gates writes into terminal-app preferences

Creating/importing a distro writes a Terminal.app settings set into
`com.apple.Terminal`'s `Window Settings` and, when iTerm2 exists, a
dynamic profile under `~/Library/Application Support/iTerm2` — host
preference stores a package shouldn't normally reach.

- **Accepted because:** the writes are user-level only (cfprefsd/
  plist file + iTerm2's documented drop-in dir — no privilege, no
  daemon), and they *are* the `windowsterminal` feature — WSL does
  the equivalent on install. A `.wsl` is already trusted to run its
  own guest code (T4); shaping a host profile named for the distro
  is far below that.
- **Mitigations that exist:** the package supplies exactly one value
  — the `windowsterminal.enabled` boolean. The profile name, title,
  and `run -n NAME` command are host-generated (distro names are already
  validated), and `ProfileTemplate` styling is not read at all
  ([#34]). The
  `ContainerDistroDistro` marker plus the " (cm)" fallback mean a
  generated profile can never overwrite or delete a user's own
  same-named one. iTerm2's drop-in is our own file — an unparsable
  one is left alone rather than rewritten. Removal edits the plist
  directly (and AppleScript only against an already-running
  Terminal), so cleanup can't be tricked into launching a window.
- **Consent:** opening a window (`terminal`, `cm --terminal`) sends
  Apple events to Terminal, so macOS's Automation (TCC) consent for
  the *calling* app applies — we never request or bypass it, and a
  denial fails the open with the `osascript` reason. Removal's
  live-model `delete` needs the same consent (a running Terminal
  flushes its cache over a plist-only edit); a denial is reported as
  a warning that the profile may reappear.
- **Residuals:** Terminal's live settings model loads `Window
  Settings` lazily, so a profile deleted while Terminal runs could
  linger in its in-memory view until a pref reload — cosmetic, and
  the plist (the persistent copy) is still clean. `enabled=false`
  is package opt-out, not a boundary: the conf is honored even for
  ad-hoc imports because it only *suppresses* host writes.

### T13. Host file access runs a root file server in the guest ([#27])

`container distro fs` (and, later, the FSKit volume) reads and
writes a distro's whole root filesystem through `distrofs`, a server
started on demand as **guest root** and reached through a
`--publish-socket` relay that every distro with `files` on carries
([host-file-access]). As designed, `fs get` hands the host user any guest
file, `/etc/shadow` included — the same reach as `container distro
run --root` or `container cp`, which that user already has.

- **Accepted because:** it's the `\\wsl$` feature, and it adds no
  capability the host user lacks — anything running as the user can
  already `container exec -u 0` into the distro. The relay is
  host-initiated: a guest can't open anything through it.
- **Host surface and connection hygiene:** the relay's host end is a
  socket bound by Apple's runtime helper in
  `<Darwin user temp>/container-distro/s/` — outside `$HOME`, so no
  virtiofs share can plant a symlink there (T6's shape). macOS has no
  public `connectat`, so `connect(2)` follows symlinks; the defense is
  layered instead, and lives in the standalone `distrofs` client: the
  socket's directory chain and leaf are verified before connect, and —
  what makes the check-then-connect race irrelevant — after
  connecting, the peer's **audit token** must satisfy the
  runtime-helper code requirement container-distro passes as
  `--peer-requirement` (`identifier
  com.apple.container.container-runtime-linux`, Apple anchor, team
  `UPBK2H6LZM`). Which *container* answered is proven by a **mutual
  HMAC-SHA256 handshake** over a fresh per-launch 32-byte token: the
  server proves it was launched by us in that container; the client
  proves it to the server.
- **The distrofs client binary itself is a trust boundary:** it is
  handed the session token and the socket path, so before exec
  container-distro verifies it — a signed copy must satisfy
  `identifier "io.github.daphnediane.distrofs" and anchor apple
  generic`; an unsigned (cargo/dev) copy must be owned by us or root
  on a path writable only by trusted accounts, checked up to `/`. A
  `DISTROFS` env override exists but is `--dev`-gated — same rule as
  `CONTAINER_CLI` — so a planted variable can't swap the binary that
  receives the token.
- **Guest-internal:** a root server is a privilege target for guest
  users. Its socket sits in a `0700` root directory, the client must
  prove the token before any request is served, and the server exits
  30 s after its last client (no listener between sessions). The token
  reaches it through `container exec --env NAME` inheritance — never
  argv — and is removed from its environment at start.
- **Guest → host input:** replies are untrusted. Frames are capped
  (16 MiB) before allocation, calls time out, in-flight requests are
  bounded, directory entry names must be single components (no `/`,
  NUL, `.`/`..`), and symlink targets are confined with chroot
  semantics before anything host-side resolves them — the macOS VFS
  follows links client-side, so a raw `/Users/me/.ssh` or `../../..`
  would otherwise point into the host. The server only resolves single
  components against `O_PATH` fds (`openat(…O_NOFOLLOW)`), and
  presents virtiofs shares and pseudo filesystems as empty read-only
  directories.
- **Restricted distros:** `files` defaults off under `--restricted`
  (`--files` opts in): the relay is inert, but mounting puts untrusted
  content in front of Finder, Quick Look, and Spotlight once the FSKit
  front-end lands.
- **Residuals:** the requirement pins Apple's signing of the runtime
  helper — a self-built `container` fails closed (no override seam
  yet; one would need the `--dev` gate). The relay is added at
  materialize, so a distro running from before an upgrade needs a
  stop/start before `fs` works.
- **Re-evaluate if:** the FSKit volume ships (Spotlight/Quick Look
  exposure, `nosuid`/`nodev`, mount ownership presentation), or
  distro-to-distro mounts add a guest-initiated path.

### T14. `--env` values cross argv; the guest environ is the weaker channel

`cm --env K=V` forwards to `container exec --env K=V` / `machine run
-e K=V`, so the literal value rides `cm`'s *and* the inner `container`
process's argv — visible to other host users via `ps` for the
process's lifetime (macOS shows every user's *arguments*; a process's
*environment* is shown only to its own uid and root, so `ps -E`
revealing an inherited `FOO=…` on the `container` process is the
shell's environment leaking no further than it already does). The
bare `--env K` inherit form and `--env-file FILE` are materially
safer: only a name or a path crosses argv, and the `container` CLI
resolves both itself (`Parser.allEnv` — host-side, in the CLI
process, shipped to the API server over XPC; `--env-file` is read via
`FileHandle`, so FIFOs and `<(…)` process substitution work). In the
guest, values land in the exec'd process's environment — readable via
`/proc/<pid>/environ` by the process's uid and by root, and inherited
by every child. `/proc/cmdline` (the kernel command line) is
unaffected, but `/proc/<pid>/cmdline` exposes command *arguments* —
secrets should never ride `-e`/positional args either.

- **Accepted because:** env passing is the standard exec mechanism and
  the exposure is the same one `docker exec -e` users already accept;
  nothing `cm` does is worse than upstream's flags it wraps.
- **Mitigations that exist:** the inherit form and `--env-file`
  (surfaced on `cm` and `container distro run` — [#44]) keep values
  off every argv, though they still land in the guest environ. A
  literal `--env K=V` whose name or value looks like a credential
  (`container::looks_secret`: `PASSWORD`/`TOKEN`/`SECRET`/`KEY`-style
  name tokens minus reference suffixes like `_ID`/`_FILE`/`_PATH`,
  PEM blocks, well-known token prefixes) draws a one-line stderr
  warning naming the variable — never the value — and the two
  alternatives. The T13 design uses the safer pattern: the distrofs
  token reaches the guest via inherited env, never argv, and is
  scrubbed from the server's environment at start.
- **Residuals:** env is a weak secret channel by nature — in-guest it
  is uid-/root-readable for the process's whole lifetime and beyond
  via children. The warning is a heuristic nudge, not a guard: a
  secret under an innocuous name passes silently. Prefer credential
  *forwarding* (the T2 ssh-agent pattern — the secret never enters
  the guest) or file delivery into a `0600` guest path; neither has a
  `cm` affordance yet beyond `--env-file <(…)` for the delivery half.

## Restricted distros

The user-facing description of `--restricted` -- what it sets and what
it does *not* do -- lives in [the security guide](../security.md#restricted-distros).
Design-side, the trade-off it manages is T4 (untrusted rootfs import),
and T3 covers why "no sudo" is a convenience boundary rather than a
wall.

## Findings and resolutions

Numbered C1–C12 from the 2026-10-03 review; later findings continue
the sequence. Status reflects the fix
commits that follow this document.

| #   | Severity   | Issue                                                                                                                                                  | Status      |
| --- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------- |
| C1  | Medium     | `install-plugin` symlinks `bin/distro` -> a user-writable binary; other users would exec it with their privileges                                      | **Fixed**   |
| C2  | Medium     | `id`/`sysctl` spawned via `PATH` -- hijack -> code exec as user                                                                                        | **Fixed**   |
| C3  | Medium     | `CONTAINER_USER`/`UID`/`GID`/`HOME` interpolated unvalidated into root-run shell code (`create-user.sh` sudoers path traversal, passwd-line injection) | **Fixed**   |
| C4  | Medium     | `-d`/`-s`/`-t`/`--unregister` values passed unvalidated -> flag smuggling into the inner `container` CLI (`cm -t=-f` -> `machine stop -f`)             | **Fixed**   |
| C5  | Medium     | `distro export -o <dir>` hits upstream [apple/container#2325] -- `export` deletes an existing directory                                                | **Fixed**   |
| C6  | Low        | Distro silently shadows a machine of the same name on `-d` (warning only in `cm -l`)                                                                   | **Fixed**   |
| C7  | Low        | `init -u` re-provisions on every boot: sudoers re-added, owner can't lock down their distro                                                            | **Fixed**   |
| C8  | Low        | Idle-PID1 loop doesn't reap zombies                                                                                                                    | **Fixed**   |
| C9  | Low        | Forwarder: unbounded thread per connection, no timeouts                                                                                                | **Fixed**   |
| C10 | Low        | `--automount` mounts every `/Volumes/*` rw (DMGs, USB, network shares); lowercase/`->`-name collisions duplicate targets                               | **Partial** |
| C11 | Info       | `uninstall` check-then-delete TOCTOU; snapshot images cleaned by name prefix not label                                                                 | **Fixed**   |
| C12 | Info (bug) | `resolve_shell` probe breaks under `machine run` re-eval -> falls back to `/bin/sh`                                                                    | **Fixed**   |
| C13 | Medium     | Lock/write helpers followed symlinks: `chflags` unlocked a planted link's target; launcher/iTerm2 writes went through planted links                    | **Fixed**   |

Resolutions:

- **C1:** binary is copied, not linked.
- **C2:** libc `getpwuid_r`/`getuid`/`getgid`/`sysctlbyname`;
  `container` prefers `/usr/local/bin/container`.
- **C3:** charset checks in `create-user.sh` and `host_user()`.
- **C4:** `validate_name`/`validate_user` on all passthru args.
- **C5:** existing dirs rejected in `distro::export`.
- **C6:** resolution-time warning, both backends annotated in
  listings/info; same-name pairs refused on `create`/`import`/
  `catalog install`/`migrate`/`rename` and `cm
  --install`/`--import`/`--rename`, with `--force` as the deliberate
  opt-in.
- **C7:** per-user `/etc/.distro.user.*` sentinel; admin edits
  preserved.
- **C8:** bare `wait` reaps orphans reparented to PID 1.
- **C9:** 64-conn semaphore cap; backlog queues excess (no idle
  timeout by design).
- **C10:** case preserved, target collisions deduped + warned,
  `--automount ro` available; rw-all-`/Volumes` remains an accepted
  opt-in.
- **C11:** uninstall removes only files it installed; imported
  images carry a `distro` label cleanup matches.
- **C12:** probe passed as one pre-joined string.
- **C13:** `lchflags`; launcher, iTerm2 and asset writes/chmods go
  through `O_NOFOLLOW`; tests target temp files, not `/etc/passwd`.

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
- **Host file access, FSKit phase ([host-file-access], [#27]).** The
  CLI half is T13. The Finder volume adds Spotlight/Quick Look
  parsing of guest content (mount `nobrowse` until decided), must
  mount `nosuid,nodev`, and moves the token-holding connect into a
  launch agent that passes the authenticated fd to a sandboxed
  module — the agent's XPC surface becomes host attack surface.
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
[#27]: https://github.com/daphnediane/container-distro/issues/27
[#34]: https://github.com/daphnediane/container-distro/issues/34
[#44]: https://github.com/daphnediane/container-distro/issues/44
[apple/container#2325]: https://github.com/apple/container/issues/2325
[configuration-files]: notes/configuration-files.md
[gui-apps]: notes/gui-apps.md
[host-file-access]: host-file-access.md
[packaging]: packaging.md
[remote-wsl-interop]: remote-wsl-interop.md
[Restricted distros]: ../security.md#restricted-distros
