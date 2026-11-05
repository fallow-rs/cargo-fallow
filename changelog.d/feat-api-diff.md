### Added

- `cargo fallow api --base <REF>` reports the public API changes of each
  published library between the merge base of the ref and HEAD: `added`,
  `removed` and `changed` items, and `crate-added`, `crate-removed` and
  `feature-removed`. The `api-removals` gate counts a removal when the
  breaking part of the version did not change. The GitHub Action runs it with
  `command: api` and the `base` input.
