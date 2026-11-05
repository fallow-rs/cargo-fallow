# cargo-fallow documentation

The [README](../README.md) is the overview. The pages below are the reference
for each command.

## Commands

| Page | Contents |
| --- | --- |
| [The check command](check.md) | Flags of `check`, and the health score |
| [Adoption](adoption.md) | Changed files, packages, rule severity, baselines, `init` and suppressions |
| [Unused dependencies](dependencies.md) | Rules of `deps`, and the cases where it counts a dependency as used |
| [Fix](fix.md) | What `cargo fallow fix` changes, and how the command checks the result |
| [Unused files](unused-files.md) | How `files` finds the files that no target compiles |
| [Visibility](visibility.md) | Kinds of `visibility`, and precise mode |
| [Structure checks](structure.md) | `boundaries`, `size`, `complexity` and `dupes` |
| [API changes](api.md) | `api --base <REF>` |
| [Trace](trace.md) | `--trace` of `visibility`, `deps` and `dupes`, to see why a check reports one item or does not |
| [Mutation testing](mutation-testing.md) | `mutants`: tiers, status, test selection and safety |
| [Output formats](output.md) | Each format, the JSON Schema, `explain` and the exit codes |
| [Hooks](hooks.md) | The agent gate of Claude Code and Codex, and the git pre-commit hook |
| [GitHub Action](../action/README.md) | Inputs, outputs and install methods of the action |
| [GitLab CI](gitlab.md) | The template: code quality report and merge request note |

## Reference files

- [`output-schema.json`](output-schema.json) is the JSON Schema of each JSON
  document. `cargo fallow schema` prints the same schema.
- [`health-score.md`](health-score.md) records the corpus data behind the
  weights of the health score.

## Maintainers

- [`release-procedure.md`](release-procedure.md) describes a release.
