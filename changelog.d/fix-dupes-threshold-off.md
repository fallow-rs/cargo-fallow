### Changed

- With `code-duplication = "off"`, the `duplication-threshold` gate still
  judges the percentage, and the output has no clone group. A workspace can
  now gate the percentage without a warning for each clone group. Before, the
  rule at `off` removed the gate too. A workspace that used `off` to drop the
  gate must now remove `threshold` from `[workspace.metadata.fallow.dupes]`.
