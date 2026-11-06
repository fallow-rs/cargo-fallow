### Fixed

- `cargo fallow visibility` gives the keyword change as the step of a
  `#[cfg(test)] pub` item that only its own module uses, and `cargo fallow fix`
  applies it. The step no longer tells you to put `#[cfg(test)]` on an item
  that already has it, and `test_only` is `false` for such an item.
