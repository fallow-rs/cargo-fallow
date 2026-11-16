### Fixed

- The description of the `error-severity-findings` gate now says that it
  counts the `error` findings of `deps` only. The `error` findings of the
  other checks count for the gate of their own check, for example
  `duplication-findings`.
- The `--help` text of `--quiet` now says that the flag hides only the
  progress lines on stderr. The report on stdout does not change.
