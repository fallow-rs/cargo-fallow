# Corpus regression check

`verdicts.json` holds the verdict of each finding of `cargo fallow check` on
seven public workspaces at pinned commits. The weekly `Corpus` workflow runs
the check on each workspace and compares the finding ids with this file. A
finding without a verdict fails the job, so a false positive cannot come back
unseen.

The file judges the checks in `checks`: `deps`, `files`, `visibility` and
`boundaries`. Their findings are judgments about the code. The `size`,
`complexity` and `dupes` findings are measurements, so the file does not
judge them.

## Two runs for each workspace

Each corpus workspace publishes its libraries. In a default run, the API of
a library that a registry can publish is an entry point, so the
`unnecessary-public` and `dead-public` rules report almost nothing in it.
`scripts/corpus-run.sh` therefore checks each workspace two times:

| Run | Configuration | Result file | Verdicts |
| --- | --- | --- | --- |
| Default | The configuration of the workspace | `<workspace>.json` | `findings` |
| Internal | `public-crates = []`, so each library is internal | `<workspace>.internal.json` | `internal` |

For the internal run, the script adds the key in a new
`[workspace.metadata.fallow.visibility]` table at the end of the root
manifest, and restores the manifest after the run. It stops for a root
manifest with no `[workspace]` table or with a `[workspace.metadata]` table.

## Verdicts

| Verdict | Meaning |
| --- | --- |
| `baseline` | The finding was in the first recorded run. Nobody checked this finding alone. Earlier corpus reviews of the visibility and dependency checks against hawk and cargo-shear found no false positive. |
| `true-positive` | A person checked the finding and found it correct. |
| `false-positive` | A person checked the finding and found it wrong. The job gives a note while the tool still reports it. |

## When the job fails

1. Download the `corpus-results` artifact, or run the check locally:

   ```bash
   cargo build --release --locked
   bash scripts/corpus-run.sh tests/corpus/verdicts.json ~/.cache/corpus target/release/cargo-fallow
   node scripts/corpus-check.mjs --verdicts tests/corpus/verdicts.json --results ~/.cache/corpus/results
   ```

2. Check each new finding by hand in the source of the workspace.
3. Fix a false positive in the tool. Add a correct finding with
   `--write`, then change its verdict from `baseline` to `true-positive`.

A finding that is gone gives a note and does not fail the job. `--write`
removes it from the file.
