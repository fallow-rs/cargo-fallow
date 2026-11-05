### Added

- `cargo fallow fix --only <CHECK>` and `--skip <CHECK>` limit the edits to
  some checks. The values are `deps` and `visibility`. Each flag takes a list
  with commas and can repeat. A `--skip` of both checks exits 2.
