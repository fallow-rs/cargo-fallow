### Fixed

- `cargo fallow visibility` reads more forms of intra-doc links: a `Self::`
  path (also in the documentation of a field or a variant), a turbofish such
  as `Gen::<u8>::new`, an indented paragraph, and an included file with a
  path from `env!("CARGO_MANIFEST_DIR")` or from more than one `concat!`
  literal.
- `cargo fallow visibility` no longer reports a type that macro tokens name
  when the macro can write a `pub` item: the tokens hold `pub` or a `vis`
  fragment.

### Performance

- The bracket scan of intra-doc links is linear in the length of the text. A
  text with many open brackets no longer takes seconds.
