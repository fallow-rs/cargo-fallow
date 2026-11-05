# API changes

`cargo fallow api --base <REF>` reports the public API changes of the
published libraries of the workspace. A published library is a library whose
`publish` is not `false`. The command compares HEAD with the merge base of HEAD
and the ref, the same as `size --base`. `cargo fallow check` does not run it.

```bash
cargo fallow api --base origin/main
cargo fallow api --base origin/main -p app-core --format json
cargo fallow api --base origin/main --fail-on-issues
```

## How the command compares

The command reads the full tree of the merge base with `git archive`, runs
`cargo metadata --no-deps` on it, and builds the module graph of both
versions. git only reads: the command changes no index, no ref and no work
tree.

The key of an item is each path that another crate can write:

- the path through `pub` modules;
- each `pub use` path, and each path through a `pub use <module>::*`;
- the `pub` fields and the variants of a public type, and the `pub` items of
  its inherent `impl` blocks. An `impl` block counts for a type only when its
  self path resolves to that type;
- the members of a public trait;
- each `#[macro_export]` macro, at the crate root.

A `#[doc(hidden)]` item and the items below it are not API. One key can hold
more than one item, for example one item for each `cfg` variant. The command
reports `removed` only when no item with the key is left.

| `change` | Meaning |
| --- | --- |
| `added` | HEAD has a key that the base does not have. |
| `removed` | The base has a key that HEAD does not have. |
| `changed` | The header of an item at the key changed. |
| `crate-added` | HEAD publishes a library that the base does not publish. |
| `crate-removed` | The base publishes a library that HEAD does not publish. |
| `feature-removed` | The base has a feature that HEAD does not have. |

The header of an item is its signature with the `cfg` and `derive`
attributes, without the body, the doc comments and the layout. A body change
is no API change.

A member below a key that is a type alias at HEAD gives no `removed`
finding. The members of the alias target can still exist, and the command
does not follow the alias.

## The gate

The `api-removals` gate counts each `removed`, `crate-removed` and
`feature-removed` finding of a library when the breaking part of its version
did not change. The breaking part is the major version from 1.0, and the
minor version before 1.0. Thus a removal in `0.3.1 -> 0.3.2` counts, and a
removal in `0.3.1 -> 0.4.0` does not count. Each such finding has
`breaking: true`. Only `--fail-on-issues` enforces the gate. A `changed`
finding does not count.

## Output

Each finding has `crate`, `path`, `item_kind`, `change`, `before`, `after`,
`file`, `line`, `version_base`, `version_head`, `breaking` and `actions`. The
summary has `base`, `merge_base`, `crates`, `added`, `removed` and `changed`.

- `before` and `after` are the headers, or `null`. More than one header, for
  example one for each `cfg` variant, are joined with ` | `.
- For a removal, `file` and `line` point to the nearest item around the path
  at HEAD, or to the root file of the library. When the library is gone, they
  point to its manifest.
- SARIF, CodeClimate and the annotations show the removals and the changes.
  An addition is only in the JSON report and in the counts.
- The rules are `api-removal`, `api-change` and `api-addition`. See
  `cargo fallow explain api-removal`.

## Errors

- A ref that git does not know exits 2.
- A shallow clone without the merge base exits 2. The help names the fetch:
  in GitHub Actions, set `fetch-depth: 0` in actions/checkout.

## Limits

The command does not judge a type change behind a re-export, an added or a
removed trait implementation, an auto trait, the meaning of a bound,
`#[non_exhaustive]` or the MSRV. It is not a full semver check.
