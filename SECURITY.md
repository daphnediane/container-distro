# Security Policy

> **Draft.** `container-distro` is pre-1.0 with a single maintainer;
> this policy is adopted ahead of need. Anything marked `[TODO]` is a
> placeholder for response expectations and disclosure details.

## Supported Versions

The project is pre-1.0: only the latest release receives fixes, and
there are no backports to older versions.

| Version        | Supported |
| -------------- | --------- |
| latest release | yes       |
| older releases | no        |

## Reporting a Vulnerability

**Please do not report suspected vulnerabilities through public GitHub
issues or pull requests.**

Report privately via GitHub's private vulnerability reporting: open
the repository's **Security** tab and choose **Report a
vulnerability**.

If that isn't available to you, or you get no response within
[TODO: N] days, open a public issue that says only "I have a security
report to share privately" -- no details -- and the maintainer will
reach out.

Including as much of the following as you can helps triage:

- `cm`/`container-distro` version (`cm --version`), `container`
  version, and macOS release
- the command or configuration involved
- steps to reproduce, or a proof of concept
- the impact -- what an attacker gains that they shouldn't have

Response expectations: **[TODO: acknowledgement / fix timelines --
currently best-effort]**.

## Scope

Not every scary-looking behavior is a bug here. Machines and distros
are **not sandboxes** -- a guest is a second login session for the
same user, and several apparent holes (rw home mount, SSH-agent
forwarding, passwordless guest sudo, `--cap-add ALL`) are documented,
deliberate trade-offs. Please read
[docs/security.md](docs/security.md) and
[docs/design/security.md](docs/design/security.md) before reporting;
reports that restate a documented trade-off will be closed with a
pointer to the design doc.

Suspected vulnerabilities in upstream projects -- Apple `container`
and `containerization`, base images, Rust crates, catalog `.wsl`
rootfses -- belong to those projects; report them per the upstream
project's own policy. If it isn't clear where the fault lies, the
private channel above is fine.

## Disclosure

[TODO: coordinated-disclosure policy -- e.g. fix released before any
public advisory, expected embargo window, whether reporters are
credited]

## Attribution

Adapted from GitHub's default `SECURITY.md` template, with scope and
disclosure sections informed by the OpenSSF
[oss-vulnerability-guide](https://github.com/ossf/oss-vulnerability-guide)
(CC BY 3.0).
