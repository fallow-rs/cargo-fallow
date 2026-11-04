### Fixed

- `cargo fallow visibility` reads more forms of intra-doc links: a `Self::`
  path, a turbofish such as `Gen::<u8>::new`, an indented paragraph, and an
  included file with a path from `env!("CARGO_MANIFEST_DIR")`. A label with a
  URL definition, such as `[tokio]: https://tokio.rs`, is no link.
- `cargo fallow visibility` no longer reports a type that a `macro_rules!`
  body names when the body writes a `pub` item.

### Performance

- The scan of intra-doc links is linear in the length of the text. A text
  with many open brackets no longer takes seconds.
