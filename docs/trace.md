# Trace

A trace shows why a check keeps or reports one item. Use it before you change
the item. A trace is not a report: it has no gate, and it exits 0 when the
item exists. An unknown item exits 2. A trace has only `--format human` and
`--format json`. It ignores the baseline, `--changed-since` and
`--fail-on-issues`. The `kinds` of the configuration table still limit the
verdict of `visibility --trace`.

```bash
cargo fallow visibility --trace my_crate::module::Type::method
cargo fallow deps --trace serde --format json --quiet
cargo fallow dupes --trace dup:1a2b3c4d
```

`visibility --trace <CRATE::PATH>` takes the crate name and the item path, as
the `crate` and `item` fields of a finding give them. The JSON document has
the `kind` `visibility-trace`. Each item in `items` has the fast-mode verdict
(`findings`, `undecided`, `reason`), `holds` and `uses`. A path can name more
than one item, for example a library and a binary with the same crate name.

Each entry in `uses` is a place in code that gives the item a credit. `credit`
tells how the code names the item:

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
syntax cannot decide the type). `--precise` mode decides only the candidates
that `untyped` credits keep. `holds` lists the facts other than a use that
keep the item or its visibility: `excluded-crate`, `glob-reexport`,
`module-with-reexports`, `macro-export`, `attribute-macro`, `allow-dead-code`,
`external-symbol`, `trait-impl`, `trait-item`, `proc-macro`, `test-function`,
`derive`, `main-function`, `generated-file` and `opaque-include`.

`deps --trace <DEPENDENCY>` takes the key in `Cargo.toml` or the package name.
The JSON document has the `kind` `deps-trace`. `declarations` lists each entry
that declares the dependency. `uses` lists each place in the code of a
declaring package, with its `context` (the names of `used_by`) and `via`:
`import`, `path`, `macro-tokens`, `attribute`, `doc-code`, `macro-crate` (the
output of a macro crate such as `serde_derive` uses the dependency) or
`include` (an `include!` call that the graph cannot read). `used` of each
declaration tells whether the code of its package uses it. `findings` holds
the `deps` findings of the dependency, and `skipped` the declaring packages
that the check does not judge.

`dupes --trace <FINGERPRINT>` takes the `fingerprint` of a clone group. Give
the same `--mode`, `--min-tokens` and `--min-lines` as the run that gave the
fingerprint. The JSON document has the `kind` `dupes-trace` and the group in
`clone_groups`.
