### Performance

- The Linux release binaries use the mimalloc allocator. The allocator of
  musl was slow when the checks ran in parallel threads. On a 2-CPU Linux
  machine, `cargo fallow check` on the fallow workspace now takes 22 s
  instead of 4.5 minutes. The output does not change. (#147)

### Added

- Each phase of `cargo fallow check` writes its time to stderr when it ends,
  for example `Finding clones: 10.2 s`. The phases run in parallel, so a CI
  log now shows which phase takes the time. The `files`, `boundaries` and
  `dupes` commands write the time of their main phase. `--quiet` hides these
  lines. (#147)
