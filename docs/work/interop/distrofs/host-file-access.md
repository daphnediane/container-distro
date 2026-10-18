# Host access to distro filesystems (`\\wsl$`)

- **Issue:** [#27]
- **Milestone:** `distrofs`

Guest `distrofs` server over `--publish-socket`, FSKit `Distro
Files.app` front-end; phased plan in [host-file-access].

- [ ] Phase 1: protocol, guest server, host client, `files` spec
  field, `container distro fs` CLI, T13
- [ ] Phase 2: FSKit spike, app + module + agent, signing
- [ ] Phase 3: inotify events -> `DataCacheHandler` invalidation
- [ ] Phase 4: `.pkg` payload (build `distrofs` in release.yml /
  `build-pkg.sh`), `cm --mount`, `files` in `list -v`/`info`
- [ ] Later: machines via exec stdio; `/mnt/wsl`-style
  distro-to-distro mounts

[#27]: https://github.com/daphnediane/container-distro/issues/27
[host-file-access]: ../../../design/host-file-access.md
