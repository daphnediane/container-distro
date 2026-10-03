# Gap: no localhost port forwarding

**Status:** partially closed — `cm --forward HOST[:GUEST]` (L0, static,
foreground) works for machines and distros; distros also publish ports
natively (`container distro create -p`, `cm --install --publish`,
default host IP `127.0.0.1`). WSL-style automatic forwarding is still
open
**Cheapest fix level:** L0 (manual forwarder) · [L2.5](../lower-level-integration.md#the-plugin-route--l25)
for WSL-like automatic forwarding · L3/L4 for full control

## What WSL does

WSL2 defaults to `localhostForwarding=true`: a guest process listening
on `localhost:3000` is reachable at `localhost:3000` on Windows,
proxied automatically.

## What we have today

A container machine gets its own IP on the `machine` network (shown by
`cm -l`). Guest services are reachable _at that IP only_ — nothing is
bridged to macOS localhost.

## Options

- **L0 — userspace forwarder.** `cm` could grow a `forward` subcommand:
  `TcpListener` on `127.0.0.1:<p>` → `TcpStream` to `<machine-ip>:<p>`.
  ~100 lines of std-only Rust, no new deps. Static mapping only —
  no auto-discovery of listening ports like WSL does.
- **L0 — ssh-style forwarding.** If the guest runs sshd, `ssh -L`
  works today (SSH agent is already forwarded in). Heavy dependency on
  image contents; not all distros ship sshd.
- **L2.5 — `network`-type plugin or forwarder daemon.** A custom
  `network` plugin is selected per-network via `container network create
--plugin <name>` — that controls IPAM, not forwarding. Better: a small
  `core`/auxiliary plugin running a host-side socket forwarder
  (`container` itself ships a `SocketForwarder` library product to copy
  from), optionally watching guest listeners for WSL-style dynamic
  mapping.
- **L2 — `SocketForwarder`/`NetworkClient` library products.** Same
  forwarder without the plugin wrapper, driven by a Swift shim.
- **L3/L4 — own the network.** `VZNATNetworkDeviceAttachment` or vmnet
  with our own forwarding rules; maximum control, maximum cost.

## Recommendation

Start with the L0 forwarder subcommand if anyone actually needs this —
it covers the common "run dev server in guest, open browser on Mac"
case. Dynamic WSL-parity forwarding is an L2.5 project and probably not
worth it until requested.
