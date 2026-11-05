### Added

- A `boundaries.sources` rule takes `scope = "production"`. The rule then
  skips test targets, test modules and items with a test attribute, for
  example `#[cfg(test)]` or `#[test]`.
- A `boundaries.sources` rule takes `allow = [{ path, max }]`. A listed file
  passes up to `max` matches of the rule. Each match over `max` is a finding.
