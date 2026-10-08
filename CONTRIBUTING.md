# Contributing

**Draft.** `container-distro` is a personal project with, so far, a
single maintainer -- this file exists ahead of need. Issues and pull
requests are still welcome; anything marked `[TODO]` is a placeholder
that will be filled in if a real community forms.

## Code of conduct

Participation is covered by the [Code of Conduct](CODE_OF_CONDUCT.md)
(Contributor Covenant 3.0 -- also a draft). Short version: be kind,
assume good faith, don't be a jerk.

## Ways to help

- **Bug reports** -- open an [issue] with the `cm`/`container-distro`
  version (`cm --version`), your `container` version, macOS release,
  the command you ran, and what happened vs. what you expected.
- **Feature ideas** -- check doc/TODO.md first; a fair amount of the
  roadmap is already tracked there. For anything bigger than a small
  fix, open an issue to discuss before writing code.
- **Pull requests** -- small, focused PRs are far easier to review
  than sweeping ones.

## Development setup

Full prerequisites and install detail live in doc/install.md. Short
version:

- macOS with Apple's [`container`] installed and working
  (`container system start`); development targets `container` 1.5.x
- a current stable Rust toolchain (edition 2024)

```bash
cargo build --workspace        # binaries land in target/debug/
```

The workspace has three crates:

| Crate                     | Contents                                                       |
| ------------------------- | -------------------------------------------------------------- |
| `crates/cm`               | the `cm` binary (WSL-compatible CLI)                           |
| `crates/container-distro` | the `container-distro` binary + `container_distro` library     |
| `crates/cm-core`          | shared `container` CLI plumbing (naming, OCI, forwarder, etc.) |

## Before opening a PR

Run the CI-equivalent checks:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If you add or bump a dependency it must also pass the supply-chain
gate (`cargo deny check` -- permissive licenses only; the policy lives
in deny.toml). `THIRD-PARTY-NOTICES.yaml` is regenerated at release
time, so you don't need to touch it.

Man pages and shell completions are generated at runtime from the clap
definitions -- never edit generated output; change the definitions
instead. If your change alters the CLI surface, update the matching
reference doc (doc/cm.md or doc/container-distro.md).

## Commit and PR conventions

- Commit subjects use conventional-commit tags (`feat`, `fix`, `docs`,
  `refactor`, `test`, `chore`, ...), imperative mood, ~50 characters.
- Describe the why, not just the what.
- AI-assisted contributions are welcome -- most of this codebase was
  AI-generated and human-reviewed (see the AI coding declaration in
  the README). If an AI agent meaningfully authored the change, credit
  it with a `Co-Authored-By:` trailer in the commit message. Either
  way, only submit code you have actually read and understood.

## Security issues

Machines and distros are *not* sandboxes -- read doc/security.md
before reporting what looks like a hole; several apparent ones are
documented, deliberate trade-offs.

For genuine vulnerabilities, report privately to
**[TODO: security contact]** rather than opening a public issue.

## License

Contributions are licensed under the project's
[BSD-2-Clause License](LICENSE). By submitting a pull request you
agree your contribution is provided under those terms.

[issue]: https://github.com/daphnediane/container-distro/issues
[`container`]: https://github.com/apple/container
