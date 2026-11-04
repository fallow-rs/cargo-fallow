---
name: review
description: Review a cargo-fallow branch, pull request or diff before the merge. Use after implement, or when asked to review a change.
---
<!-- Generated from .agents/skills. Do not edit. -->

# Review

1. Read `AGENTS.md` again. Read the complete diff against `origin/main`.
2. Run the gates of step 9 of `implement`.
3. Get one independent reviewer that sees the diff and the code, not your
   conclusions. Give it the lenses that the diff touches:
   - Rust correctness, the parse layer and the `mutants` safety rules in
     `CONTRIBUTING.md`;
   - the CLI contract of `AGENTS.md`: exit codes, the JSON error document,
     sorted output and the SARIF and CodeClimate formats;
   - the GitHub Action under `action/` and the GitLab template under `ci/`;
   - the release workflow and `docs/release-procedure.md`.
4. Reproduce each finding before you act on it:
   - trace the call site before you accept a bug claim;
   - drop style preferences and "a different design also works";
   - read the diff or the output that the reviewer cites.
5. Mark each finding `APPROVE`, `CONCERN` or `BLOCK`. Fix each block and
   each confirmed defect, with a regression test.
6. When the analysis changed, run the corpus check of
   `tests/corpus/README.md`. Explain each new or lost finding.
7. Run the gates again after the fixes.

Do not trust a green build, one narrow test or a summary. Read the live
output.
