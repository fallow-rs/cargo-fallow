### Fixed

- The GitHub Action fails the step when `base: auto` cannot read the base
  commit and the command needs the base for a gate (`check`, `size` and
  `mutants`). Before, the action wrote a notice and ran with no base, so the
  growth gate passed without a check. A run with no pull request base and no
  push stays a notice.

### Added

- The GitHub Action passes the `base` input to `check` as `--base`, so the
  `file-growth` gate runs in a `check` step.
