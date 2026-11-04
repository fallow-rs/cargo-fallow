### Changed

- The SARIF level of a finding now follows its severity: `error` gives
  `error`, and `warn` gives `warning`. Before, an `error` finding was a
  SARIF `warning` when its gate was not enforced. A gate flag no longer
  changes the level. This is the same rule that fallow uses.
- A `deps` warning, for example `unused-optional-dependency`, is now a SARIF
  `warning`. Before, it was a SARIF `note`.
- The `github-annotations` and `github-summary` formats and the GitHub Action
  use the same level. A surviving mutant is now always an `error`.
