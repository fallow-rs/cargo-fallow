# The SARIF log of a `cargo fallow mutants` JSON report.
#
# The `mutants` command runs the tests of each mutant, so the action runs it
# once, with `--format json`. This filter writes the SARIF log that
# `--format sarif` writes for the same report. src/render/mutants.rs holds the
# original. action/tests/run.sh compares the two forms of one real run, so a
# change of the renderer fails the test. A survivor has the error severity, so
# its level is always `error`. Only a gate notification follows the gate.

def level_of($outcome):
  if $outcome.status == "fail" and $outcome.enforced then "error"
  elif $outcome.status == "fail" or $outcome.status == "warn" then "warning"
  else null
  end;

def notification($name; $outcome; $level):
  {
    level: $level,
    message: {
      text: "The `\($name)` gate \(if $outcome.status == "warn" then "warned" else "failed" end) (\(if $outcome.enforced then "enforced" else "not enforced" end))."
    },
    descriptor: { id: $name }
  };

def survivor_message:
  "`\(.original)` became `\(.replacement)` (\(.operator))" as $change
  | if .reason then "Each test passed one at a time when \($change): \(.reason)."
    else "Each test passed when \($change)."
    end;

. as $report
| "surviving-mutants" as $rule
| [($report.gate_outcomes // {}) | to_entries[]
    | level_of(.value) as $level
    | select($level != null)
    | { name: .key, outcome: .value, level: $level }] as $flagged
| [$report.mutants[]? | select(.status == "survived") | {
    ruleId: $rule,
    level: "error",
    message: { text: survivor_message },
    locations: [{ physicalLocation: { artifactLocation: { uri: .file }, region: { startLine: .line } } }],
    partialFingerprints: { mutantId: .id }
  }] as $results
| {
    "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
    version: "2.1.0",
    runs: [{
      tool: {
        driver: {
          name: "cargo-fallow",
          version: $report.version,
          informationUri: "https://github.com/fallow-rs/cargo-fallow",
          rules: (if $results == [] then [] else [{
            id: $rule,
            shortDescription: { text: "A mutant survived: each test passed with the change." }
          }] end)
        }
      },
      invocations: [
        { executionSuccessful: true }
        + (if $flagged == [] then {} else {
            toolExecutionNotifications: [$flagged[] | notification(.name; .outcome; .level)]
          } end)
      ],
      results: $results
    }]
  }
