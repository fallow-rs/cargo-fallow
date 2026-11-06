### Performance

- The boundary check builds the module graph at most one time for the item,
  re-export and cfg rules. When `cargo fallow check` also runs the
  dependency or the visibility check, these rules read the graph of that
  check, so the run builds no extra graph. The source rules read and parse
  their files on parallel threads. On the fallow workspace with 2 CPUs,
  `cargo fallow check` takes about 9 s instead of about 12 s, and
  `cargo fallow boundaries` on a laptop takes 2.4 s instead of 5.4 s. The
  findings do not change. (#155)

### Added

- `cargo fallow check` and `cargo fallow boundaries` write the time of the
  item, re-export and cfg rules as a separate phase,
  `Checking the boundary rules on the module graph`. (#155)
