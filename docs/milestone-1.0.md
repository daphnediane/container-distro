# 1.0 finalization checklist

What remains before tagging `v1.0.0`. The policy work is done:
`SECURITY.md`, `CODE_OF_CONDUCT.md`, and `CONTRIBUTING.md` are
de-drafted and concrete, and the pre-1.0 checklist in
[design/security.md](design/security.md) is fully closed. The
standard release gates (release-gate, legacy-adoption check, milestone
sweep, changelog audit) live in [release.md](design/release.md) step 1
and aren't repeated here -- only the 1.0-specific deltas are.

## Before tagging

- [ ] **Final re-evaluation pass over
  [design/security.md](design/security.md)** -- its preamble frames the
  register as "what to re-evaluate before calling anything stable";
  one sweep against the T1-T13 triggers is the last review before
  calling it stable.
- [ ] **Set the 1.0 version** -- `cargo release release` tags whatever
  the dev version points at, so `0.10.0-dev` would tag `0.10.0`. When
  the next release is decided to be 1.0, run
  `cargo release version 1.0.0-dev -x --no-confirm` first so the tag
  lands as `v1.0.0`.
- [ ] **Release per [release.md](design/release.md)** -- gate,
  `check-legacy-adoption.sh`, milestone sweep (`gh issue list
  --milestone 1.0` -- currently 0 open), CHANGELOG rename to
  `## 1.0.0 -- YYYY-MM-DD`, tag + `release/1.0` branch, signed
  artifacts, publish, `cargo publish`, close shipped issues.
