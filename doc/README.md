# wsl-compat docs

Design notes and research documents for `cm`. These are working documents —
they describe options and tradeoffs, not commitments.

| Document                                              | Contents                                                                                                                                                                            |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [lower-level-integration](lower-level-integration.md) | Can `cm` bypass the `container` CLI — the XPC/Swift-library/Containerization/Virtualization.framework layers, Rust↔Swift interop options, and whether a Swift rewrite is worthwhile |

Upstream references:

- [apple/container](https://github.com/apple/container) — the `container` CLI
  and its `container-apiserver` XPC services (`container machine` lives here)
- [apple/containerization](https://github.com/apple/containerization) — the
  lower-level Swift package for OCI images, lightweight VMs, and `vminitd`
