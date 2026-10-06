# Gap: no localhost port forwarding

- **Status:** partially closed -- `cm --forward HOST[:GUEST]` (L0,
  static, foreground, **experimental**) works for machines and distros;
  distros also publish ports natively (`container distro create -p`,
  `cm --install --publish`, `distro set --publish/--unpublish`, default
  host IP `127.0.0.1`). `--forward` is expected to be superseded by
  WSL-style automatic forwarding, which is still open
- **Cheapest fix level:** L0 (manual forwarder) · [L2.5] for WSL-like
  automatic forwarding · L3/L4 for full control
- **Tracking:** [#5]

## What WSL does

WSL2 defaults to `localhostForwarding=true`: a guest process listening
on `localhost:3000` is reachable at `localhost:3000` on Windows. There
is no forward list to edit -- a Windows-side service watches guest
listeners and proxies them dynamically as they come and go.

## What we have today

A container machine gets its own IP on the `machine` network (shown by
`cm -l`). Guest services are reachable _at that IP only_ -- nothing is
bridged to macOS localhost.

## Options

- **L0 -- userspace forwarder.** `cm` could grow a `forward` subcommand:
  `TcpListener` on `127.0.0.1:<p>` → `TcpStream` to `<machine-ip>:<p>`.
  ~100 lines of std-only Rust, no new deps. Static mapping only -- no
  auto-discovery of listening ports like WSL does.
- **L0 -- ssh-style forwarding.** If the guest runs sshd, `ssh -L` works
  today (SSH agent is already forwarded in). Heavy dependency on image
  contents; not all distros ship sshd.
- **L2.5 -- `network`-type plugin or forwarder daemon.** A custom
  `network` plugin is selected per-network via
  `container network create --plugin <name>` -- that controls IPAM, not
  forwarding. Better: a small `core`/auxiliary plugin running a
  host-side socket forwarder (`container` itself ships a
  `SocketForwarder` library product to copy from), optionally watching
  guest listeners for WSL-style dynamic mapping.
- **L2 -- `SocketForwarder`/`NetworkClient` library products.** Same
  forwarder without the plugin wrapper, driven by a Swift shim.
- **L3/L4 -- own the network.** `VZNATNetworkDeviceAttachment` or vmnet
  with our own forwarding rules; maximum control, maximum cost.

## Recommendation

The L0 forwarder shipped as `cm --forward` and covers the common "run
dev server in guest, open browser on Mac" case. It is **experimental**:
for distros, `--publish` (create-time or via `distro set`) is the
supported path since it uses real runtime publishing rather than a
userspace proxy. Machines have no publish path, so `--forward` stays for
them until listener-watching automatic forwarding (L2.5) lands -- at
which point `--forward` should be deprecated outright. Dynamic
WSL-parity forwarding is probably not worth it until requested.

[#5]: https://github.com/daphnediane/container-distro/issues/5
[L2.5]: ../lower-level-integration.md#the-plugin-route--l25
