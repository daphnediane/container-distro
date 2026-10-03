# Gap: `machine run` exec fidelity — stdin needs `-i`, first writes dropped, argv re-evaled

**Status:** partially closed — `cm` now always passes `-i` (stdin works).
`cm -e` runs machine commands with `container exec` in the backing
container (argv-exact, no shell re-eval), falling back to
`machine run` with single-quoted arguments; distros always use
`container exec`. The first-write drop is still open upstream
(not reproduced in later 1.5.0 runs on 2026-10-03)
**Fix level:** upstream fix in `container machine run` attach sequencing;
L0 workarounds (`-i` passthrough, attach handshake) cover most cases

## What WSL does

`wsl.exe -d <d> -e <cmd>` gives byte-exact pipe semantics: stdin, stdout,
and stderr are reliable streams, exit codes propagate, and nothing is
lost regardless of how fast the guest process writes. Tooling (editors'
remote extensions, scripts, prompts) depends on this.

## What we have today

`container machine run -- <cmd>` (what `cm -e`/`cm --` execs) has two
reliable-reproduction stdio problems, observed on `container` 1.5.0:

### stdin is closed unless `-i` is passed

```bash
$ echo hello | container machine run -n alpine -- cat
$ echo hello | container machine run -n alpine -i -- cat
hello
```

Without `-i` the guest process sees immediate EOF on stdin. `cm` doesn't
pass `-i` today, so `cm -d m -- cmd` ignores piped input entirely.

### The first guest→host write is dropped — on *either* stream

```bash
$ container machine run -n alpine -i -- sh -c 'echo A; echo B; echo C'

B          # "A" vanished, its newline partially survived
C

$ container machine run -n alpine -i -- sh -c 'echo S1 >&2; echo O1; echo S2 >&2; echo O2' 2>&1

S2         # S1 — first write overall — dropped even though it's stderr
O1
O2
```

Observed characteristics (12/12 repro on multi-write `sh -c`):

- The **first `write()` across both streams** is lost; everything after it
  arrives intact. A fragment survives as a leading blank line — looks like
  partial first-datagram delivery rather than a clean drop.
- stderr is affected identically to stdout — it's not a TTY-mode stdout
  bug; the streams attach late as a pair.
- Once attached, streaming is reliable — output after the first write
  (including output ~50 ms in) arrives completely, so long-running or
  verbose commands are mostly unaffected.
- Not fully deterministic: bare executables sometimes survive
  (`echo hi` 6/6, `seq 1 5` 6/6) and sometimes truncate oddly
  (`printf 'a b c\n'` delivered only `a` — presumably split writes losing
  all but the first). Treat *any* early output as unreliable.
- Exit codes and late stderr are unaffected.

### argv is not preserved — the guest shell-evaluates the joined command

Separate but adjacent fidelity problem, already tracked upstream as
[apple/container#1954](https://github.com/apple/container/issues/1954):
`machine run`'s guest init does `exec "$SHELL" -c "$*"` — the argv vector
is concatenated and re-evaluated as shell code rather than executed
directly:

```bash
$ container machine run -n alpine -i -- printf ':%s:' 'one two three'
:one::two::three:          # 'one two three' re-split into 3 args

$ container machine run -n alpine -i -- sh -c 'for i in 1 2 3; do echo i$i; done'
/bin/sh: syntax error: unexpected "do"   # quoted script re-evaled away
```

Consequences:

- `cm -e`/`cm --` do **not** deliver wsl.exe-style argv-exact exec —
  a command like `cm -e grep 'foo bar' file` silently becomes
  `grep foo bar file` (three patterns).
- Quoting survives one extra eval round, which is why `sh -c 'a; b; c'`
  works at all — and why the remote extensions' `bash -c '<single-quoted
  script>'` call style mostly still lands correctly. Fragile but
  functional.
- This also confounds repro of the write-loss bug: in `sh -c 'echo A;
  echo B; echo C'`, `B`/`C` may be printed by the *outer* eval rather
  than the inner `sh`.

`#1954` asks for an argv-preserving mode; until it lands, `cm` could
mitigate by quoting each arg before handing off (single-quote + `'\''`
escaping), so the extra eval round is a no-op.

### Impact

- **Tooling that parses `cm` exec output is broken on the margin.** The
  remote-extension bootstrap ([remote-wsl-interop](../remote-wsl-interop.md))
  survives because its result block is echoed at the end of a long
  script — but the quick probe commands editors sprinkle around it
  (`uname`, `command -v …`, `cat /etc/os-release`) are exactly the
  first-write-fast commands that lose output.
- **Piped stdin silently does nothing** without `-i` — `git` helpers,
  `ssh` ProxyCommand-style uses, `tar` streams, all affected.
- **Argv quoting is leaky** — scripts doing `cm -e cmd "arg with space"`
  get silently re-split args.

## Related upstream issues

Searched [apple/container/issues](https://github.com/apple/container/issues)
(2026-10-01). **No exact match** for the first-write drop — a new issue is
warranted. The neighborhood is well-populated though:

| Issue                                                                                          | Status | Relation                                                      |
| ---------------------------------------------------------------------------------------------- | ------ | ------------------------------------------------------------- |
| [#1148](https://github.com/apple/container/issues/1148) — `run -i` truncates piped stdout      | open   | Closest — output bytes lost on the interactive/exec path      |
| [#1954](https://github.com/apple/container/issues/1954) — `machine run` shell-evals argv       | open   | The argv section above; asks for an argv-preserving mode      |
| [#2299](https://github.com/apple/container/issues/2299) — `exec -it` leaves host tty broken    | open   | Same attach/teardown machinery                                |
| [#2009](https://github.com/apple/container/issues/2009) — log MultiWriter dies on client EPIPE | open   | Same output-fanout plumbing, different symptom                |
| [#949](https://github.com/apple/container/issues/949) — `-i`/`-t` didn't pipe stdout           | closed | Historical precedent: this plumbing has shipped broken before |
| [#2057](https://github.com/apple/container/issues/2057) — completed exec state retained        | open   | Exec lifecycle in container-runtime-linux                     |
| [#2210](https://github.com/apple/container/issues/2210) — exec exit 255 ambiguous              | open   | Exit-code fidelity — the "codes propagate" claim has a caveat |

## Options

- **Upstream fix (the real answer).** File `apple/container`: `machine
  run` starts the guest process before the vsock stdio channels are
  confirmed attached; first writes land before the reader exists. If the
  race is in the CLI's sequencing (spawn-then-attach), an L2 client could
  sequence it correctly itself; if it's in `vminitd`'s process-start
  ordering, no client-side level escapes it. Repro above is a good issue
  body.
- **L0 — `cm` passes `-i` on exec.** One flag; fixes stdin. Harmless for
  interactive use (`-i` is implied by `-t` anyway). Should land
  regardless of the race.
- **L0 — attach handshake for reliable scripted output.** Since stdin
  works with `-i`, `cm` could wrap exec commands so the guest blocks on a
  byte before running: `sh -c 'IFS= read -r _; exec "$@"' cm-attach <cmd>…`
  with `cm` writing the byte once attached. Only sound if stdin-reaching-
  the-guest implies stdout attach — worth verifying before adopting.
  Simpler but flakier: a fixed delay prefix (note busybox `sleep` on
  alpine doesn't accept fractional args — `sleep 0.05` fails, `sleep 1`
  works).
- **L2/L2.5** — doesn't inherently help; same `createProcess`/vminitd
  plumbing. Only relevant if the bug turns out to be in the *CLI's*
  attach ordering, in which case a client that sequences correctly wins.

## Recommendation

Do all of: pass `-i` in `cm`'s exec path now; file a fresh upstream issue
with the first-write repro (cite #1148 as adjacent); add per-arg quoting
to neutralize the shell-eval round until #1954 lands; document the
handshake workaround in the README's differences section if it verifies.
This is the one gap that makes `cm` currently untrustworthy for scripted
(non-interactive) use — it should be tracked as a correctness bug, not a
parity nicety.

## Related

- [remote-wsl-interop](../remote-wsl-interop.md) — the motivating use
  case; its probe/bootstrap traffic is exactly what this race corrupts
- [subprocess-overhead](subprocess-overhead.md) — same exec path;
  scraping `--format json` output shares this surface
