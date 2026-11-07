### Fixed

- `cargo fallow fix` no longer removes an unused dependency entry when a
  target of that table uses a one-name attribute macro, or calls a one-name
  macro in the place of an item, that an import brings from a crate outside
  the workspace. The rule also applies to a rename of the crate and to
  `#[macro_use] extern crate`. For example, `use wasm_bindgen::prelude::*;` and
  `#[wasm_bindgen] pub async fn` write `wasm_bindgen_futures::` paths that the
  command cannot see. `deps` still reports the entry, and the action tells
  why `fix` keeps it.
