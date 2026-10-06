# Gap: no GUI applications (X11 / Wayland -- WSLg)

- **Status:** open -- no display plumbing at all today
- **Cheapest fix level:** L0 throughout. X11 is a host X server plus env
  plumbing; Wayland rides a third-party macOS compositor with the same
  socket forwarding. Nothing here needs lower-than-CLI access.
- **Tracking:** [#7]

## What WSL does

[WSLg] runs a per-user-distro "system distro" container on the same VM
containing:

- **Weston** -- a Wayland compositor with an RDP backend
- **XWayland** -- X11 clients enter through the Wayland compositor
- **PulseAudio** -- with a sink/source plugin riding the RDP channel

The system distro's sockets (`/tmp/.X11-unix/X0`, `wayland-0`, pulse)
are shared into the user distro and `DISPLAY`, `WAYLAND_DISPLAY`,
`PULSE_SERVER` are preconfigured, so GUI apps work out of the box. On
the Windows side, mstsc's RAIL mode remotes *individual windows* -- that
per-window integration is the actual product; the Linux side is just
plumbing.

## What we have today

Nothing. No `DISPLAY`, no sockets, and no GPU: `container` exposes no
virtio-gpu device to guests, so rendering would be llvmpipe/softpipe
either way.

Verified plumbing facts (this Mac, `container` 1.5.0):

- Guests NAT through `192.168.64.1` -- the default route *and* the DNS
  resolver -- so the host is reachable there with no forwarding needed.
- `$HOME` is shared at the same path, so `~/.Xauthority` written on the
  host is visible inside a distro for free.
- Host already has an X server: MacPorts `xorg-server` (`X11.app`,
  XQuartz-derived). Requiring XQuartz-or-equivalent is reasonable.

## Options

### A. X11 over TCP to a host X server -- the direct analog

Enable TCP listening on the X server (`defaults write org.xquartz.X11
nolisten_tcp -bool false`, or the MacPorts `org.x.X11` equivalent), then
`DISPLAY=<gateway>:0` in the guest. Rootless mode gives per-window Aqua
windows and pasteboard↔X-selection sync for free -- the closest macOS
equivalent of the WSLg experience.

- Guest env: `DISPLAY=<gw>:0`, with `<gw>` computed from the container
  network subnet at create time. Optionally bridge a guest
  `/tmp/.X11-unix/X0` unix socket → `<gw>:6000` (socat or a small static
  helper in `/sbin.distro`) so `DISPLAY=:0` also works.
- Auth: `xhost +<guest-ip>` is the easy path; the proper path is an MIT
  cookie in the shared `~/.Xauthority` (`xauth` generate/merge on the
  host at setup time).
- Changes here: a `gui` label on `DistroSpec` → `--env DISPLAY=…` in
  `create_args`; `--gui` on `create`/`import` plus `set --gui on|off`; a
  host-side `cm` helper that launches the X server and fixes xauth.
  Machines can't take env at create, but `cm`'s exec paths already pass
  `--env`, so per-session injection works there.

### B. Wayland via a host compositor + waypipe -- per-window

macOS has no built-in Wayland compositor, but third-party ones exist --
one is aimed directly at this use case:

- **cocoa-way** -- a Rust/smithay compositor with a Metal renderer,
  rootless per-window mode, and an *Apple Container transport* built on
  `container`'s `--publish-socket`. Needs `waypipe-darwin` on the host
  and a waypipe server in the guest.
  (<https://github.com/J-x-Z/cocoa-way>)
- **Wawona** -- native compositor (Rust core, AppKit chrome per
  toplevel), waypipe remoting, ships nested Weston/Niri.
  (<https://github.com/Wawona/Wawona>)
- **wayoa** -- Cocoa/Metal compositor, one NSWindow per toplevel.
  (<https://github.com/ericcurtin/wayoa>)

`waypipe` is the transport: it proxies Wayland protocol over a single
socket, ssh `-X`-style. The guest needs `waypipe` (or a unix-socket
bridge to the host compositor's socket) plus `WAYLAND_DISPLAY`. X11-only
apps under this model go through in-guest XWayland → Wayland → waypipe,
or keep using option A in parallel.

Caveat: all three compositors are pre-1.0 personal projects. `cm`'s
share is small -- env + socket plumbing + maybe launching the host
app -- so integrating is cheap whenever one matures; the risk is upstream
stability, not ours.

### C. In-guest compositor → streaming -- desktop-in-a-window

- `weston --backend=rdp` + an RDP client on the Mac (Windows App /
  FreeRDP): works today, but no macOS RDP client does RAIL per-window
  remoting, so it's a remote desktop in one window.
- `weston --backend=vnc` / wayvnc → Screen Sharing or any VNC client.
- `weston --backend=x11` nested inside XQuartz -- same end result.

All packageable from distro repos with no new host deps, but the UX is a
worse version of WSLg, not WSLg.

### D. Audio (optional add-on)

PulseAudio via MacPorts/Homebrew on the host with
`module-native-protocol-tcp`, `PULSE_SERVER=tcp:<gw>:4713` in the
guest -- WSLg-equivalent in shape. (cocoa-way advertises CoreAudio
forwarding if option B lands.)

### What doesn't transfer

The literal WSLg port doesn't: no RDP client on macOS does RAIL
RemoteApp-style per-window remoting against an arbitrary server (Windows
App doesn't expose it; FreeRDP's Mac client is unmaintained). The
*shape* transfers -- guest-side servers, sockets shared across the
boundary, preset env -- but the renderer on the host side has to be X11
or a native Wayland compositor instead of RDP.

## Recommendation

Phase 1 is option A: X11 over TCP to the user's X server. It's all L0 in
this repo -- a `gui` label, env injection in `create_args`, and a host
helper that launches the X server and sorts out xauth -- and it covers
everything from x11-apps to GTK with no new guest-side processes. Phase
2 is option B, scoped to "env + socket plumbing + launch the host
compositor"; revisit when cocoa-way or Wawona is stable enough to
recommend. Skip the desktop-streaming modes and the literal RDP port.

[#7]: https://github.com/daphnediane/container-distro/issues/7
[WSLg]: https://github.com/microsoft/wslg
