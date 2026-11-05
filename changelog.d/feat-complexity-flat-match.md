### Added

- `[workspace.metadata.fallow.complexity] cyclomatic-policy = "flat-match"`
  counts a flat `match` as one decision for the `max-cyclomatic` limit. A
  flat `match` has no guard and no decision in an arm, for example a lookup
  table. A finding then has `standard_cyclomatic` with the standard count.
  The default stays `standard`, and the health score does not change.
