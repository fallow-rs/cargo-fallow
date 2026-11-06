### Fixed

- The module walk now reads a `path` value that is a raw string, such as
  `#[path = r"plat/imp.rs"]`, and resolves the escapes of a `path` value like
  rustc. Before, `deps` reported a dependency that only such a file uses, and
  `fix` removed it.
- A `path` attribute on an inline module now sets the folder of the modules
  in it, like rustc: `#[path = "imp"] mod outer { mod inner; }` loads
  `imp/inner.rs`. Before, the walk read `outer/inner.rs`, and `files` skipped
  the package.
- The walk also reads `\` in a `path` value as a folder separator when the
  file with the `\` in its name does not exist. A dependency that only a
  Windows file uses then counts on each host.
- A `cfg_attr` condition that no build makes true, such as `any()`, now adds
  no module file.
