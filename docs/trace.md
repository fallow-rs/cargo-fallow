# Trace

A trace shows why a check reports one item, or why it does not. Run a trace
before you change or delete the item.

A trace has no gate. The command exits 0 when the item exists, and exits 2
for an unknown item. Only `--format human` and `--format json` are available.
The trace ignores the baseline, `--changed-since` and `--fail-on-issues`. For
`visibility --trace`, the `kinds` list of the configuration table still
limits the finding kinds in the result.

```bash
cargo fallow visibility --trace my_crate::module::Type::method
cargo fallow deps --trace serde --format json --quiet
cargo fallow dupes --trace dup:1a2b3c4d
```

For `visibility --trace <CRATE::PATH>`, give the crate name and the item path
from the `crate` and `item` fields of a finding. The JSON document has the
`kind` `visibility-trace`. Each entry in `items` has the fast-mode result
(`findings`, `undecided` and `reason`), `holds` and `uses`. One path can name
more than one item, for example when a library and a binary have the same
crate name.

Each entry in `uses` is a place in the code that counts as a use of the item.
The `credit` field tells how the code names the item:

| `credit` | Meaning |
| --- | --- |
| `import` | A `use` item or an `extern crate` item. |
| `glob-import` | A `use path::*` item. |
| `path` | A path that resolves to the item. |
| `name-match` | A path that does not resolve, so each item with its last name gets credit. |
| `method-name` | A method call. The type of the receiver is unknown. |
| `field-name` | A field access. The type of the receiver is unknown. |
| `member-name` | A name in tokens that can be a field or a method. |
| `record-field` | A field in a struct expression or a struct pattern. |
| `macro-call` | A call of a `macro_rules!` macro. |
| `macro-tokens` | A name in the tokens of a macro call. The line is the line where the call starts. |
| `attribute` | A name in an attribute. The line is the line of the attribute. |
| `doc-code` | A name in a code block of the documentation. The line is the first code line of the block. |

`class` is `exact`, `fallback` (the path did not resolve) or `untyped` (the
type is not known from the syntax). `--precise` mode checks only the
candidates that have no use other than an `untyped` one. `holds` lists the
reasons other than a use that the check does not report the item:
`excluded-crate`, `published-crate`, `glob-reexport`,
`module-with-reexports`, `macro-export`, `attribute-macro`, `allow-dead-code`,
`external-symbol`, `trait-impl`, `trait-item`, `proc-macro`, `test-function`,
`derive`, `main-function`, `generated-file` and `opaque-include`.

For `deps --trace <DEPENDENCY>`, give the key in `Cargo.toml` or the package
name. The JSON document has the `kind` `deps-trace`. `declarations` lists each
`Cargo.toml` entry of the dependency. `uses` lists each place in the code of a
declaring package that uses the dependency. An entry has `context` (the names
of `used_by`) and `via`, which is one of these values:
`import`, `path`, `macro-tokens`, `attribute`, `doc-code`, `macro-crate` (the
output of a macro crate such as `serde_derive` uses the dependency) or
`include` (an `include!` call that cargo-fallow cannot read). The `used`
field of a declaration is `true` when the code of its package uses the
dependency. `findings` lists the `deps` findings of the dependency.
`skipped` lists the declaring packages that the check skipped.

For `dupes --trace <FINGERPRINT>`, give the `fingerprint` of a clone group.
Use the same `--mode`, `--min-tokens` and `--min-lines` as the run that
reported the fingerprint. The JSON document has the `kind` `dupes-trace` and the group in
`clone_groups`.
