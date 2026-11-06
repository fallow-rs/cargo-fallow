import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { compare, judgedFindings, reportProblems, run, validate } from "./corpus-check.mjs";

const COMMIT = "0123456789abcdef0123456789abcdef01234567";

const finding = (id, check = "visibility", line = 3) => ({
  id,
  check,
  rule: "dead-public",
  severity: "warn",
  message: `finding ${id}`,
  location: { path: "src/lib.rs", line },
});

const report = (findings, checks = ["parse", "deps", "visibility"]) => ({
  schema_version: 1,
  checks,
  findings,
});

const verdictFile = (findings) => ({
  checks: ["deps", "visibility"],
  workspaces: {
    demo: { repository: "https://example.com/demo", commit: COMMIT, findings },
  },
});

// Writes the verdict file and one result file, and runs the check.
const runCase = (verdicts, reported, write = false) => {
  const dir = mkdtempSync(join(tmpdir(), "corpus-check-"));
  try {
    const verdictsPath = join(dir, "verdicts.json");
    writeFileSync(verdictsPath, JSON.stringify(verdicts));
    writeFileSync(join(dir, "demo.json"), JSON.stringify(report(reported)));
    const lines = [];
    const code = run({ verdictsPath, resultsDir: dir, write, log: (line) => lines.push(line) });
    return { code, lines, saved: JSON.parse(readFileSync(verdictsPath, "utf8")) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

const baseline = {
  check: "visibility",
  rule: "dead-public",
  path: "src/lib.rs",
  line: 3,
  verdict: "baseline",
};

test("an equal finding passes", () => {
  const { code, lines } = runCase(verdictFile({ a: baseline }), [finding("a")]);
  assert.equal(code, 0, lines.join("\n"));
});

test("a new finding without a verdict fails", () => {
  const { code, lines } = runCase(verdictFile({ a: baseline }), [finding("a"), finding("b")]);
  assert.equal(code, 1);
  assert.ok(
    lines.some((line) => line.includes("new finding b visibility/dead-public at src/lib.rs:3")),
  );
});

test("a removed finding gives a note and passes", () => {
  const { code, lines } = runCase(verdictFile({ a: baseline, b: baseline }), [finding("a")]);
  assert.equal(code, 0);
  assert.ok(lines.some((line) => line.startsWith("note: demo: b ") && line.endsWith("is gone")));
});

test("a finding of a check that the file does not judge is left out", () => {
  const { code } = runCase(verdictFile({}), [finding("d", "dupes")]);
  assert.equal(code, 0);
});

test("a file that did not parse fails the run", () => {
  const parse = { ...finding("p", "parse"), rule: "parse-error" };
  const { code, lines } = runCase(verdictFile({ a: baseline }), [parse]);
  assert.equal(code, 2);
  assert.ok(
    lines.some((line) => line.includes("src/lib.rs did not parse")),
    lines.join("\n"),
  );
});

test("a judged check that did not run fails the run", () => {
  assert.deepEqual(reportProblems(report([], ["visibility"]), ["deps", "visibility"]), [
    "the check `deps` did not run",
  ]);
  assert.equal(reportProblems({ findings: [] }, ["deps"]).length, 1);
  assert.equal(reportProblems({ ...report([]), schema_version: 2 }, ["deps"]).length, 1);
});

test("a result that is not JSON fails without a stack trace", () => {
  const dir = mkdtempSync(join(tmpdir(), "corpus-check-"));
  try {
    const verdictsPath = join(dir, "verdicts.json");
    writeFileSync(verdictsPath, JSON.stringify(verdictFile({})));
    writeFileSync(join(dir, "demo.json"), "{");
    const lines = [];
    const code = run({
      verdictsPath,
      resultsDir: dir,
      write: true,
      log: (line) => lines.push(line),
    });
    assert.equal(code, 2);
    assert.ok(
      lines.some((line) => line.includes("not JSON")),
      lines.join("\n"),
    );
    assert.deepEqual(JSON.parse(readFileSync(verdictsPath, "utf8")), verdictFile({}));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a missing result file fails", () => {
  const dir = mkdtempSync(join(tmpdir(), "corpus-check-"));
  try {
    const verdictsPath = join(dir, "verdicts.json");
    writeFileSync(verdictsPath, JSON.stringify(verdictFile({})));
    const code = run({ verdictsPath, resultsDir: dir, write: false, log: () => {} });
    assert.equal(code, 2);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("--write adds new findings as baseline and removes the gone ones", () => {
  const kept = { ...baseline, verdict: "true-positive" };
  const { code, saved } = runCase(
    verdictFile({ a: kept, gone: baseline }),
    [finding("a"), finding("b")],
    true,
  );
  assert.equal(code, 0);
  assert.deepEqual(Object.keys(saved.workspaces.demo.findings), ["a", "b"]);
  assert.equal(saved.workspaces.demo.findings.a.verdict, "true-positive");
  assert.equal(saved.workspaces.demo.findings.b.verdict, "baseline");
});

test("a known false positive gives a note in the output", () => {
  const { code, lines } = runCase(verdictFile({ a: { ...baseline, verdict: "false-positive" } }), [
    finding("a"),
  ]);
  assert.equal(code, 0);
  assert.ok(lines.some((line) => line.startsWith("note: demo: known false positive a")));
});

test("compare marks a known false positive", () => {
  const found = judgedFindings({ findings: [finding("a")] }, ["visibility"]);
  const result = compare(found, { a: { ...baseline, verdict: "false-positive" } });
  assert.equal(result.knownFalse.length, 1);
  assert.equal(result.added.length, 0);
});

test("validate rejects an unknown verdict and a short commit", () => {
  const file = verdictFile({ a: { ...baseline, verdict: "maybe" } });
  file.workspaces.demo.commit = "abc123";
  const problems = validate(file);
  assert.equal(problems.length, 2, problems.join("\n"));
  const { code } = runCase(file, []);
  assert.equal(code, 2);
});

// Writes the verdict file, the default result and the internal result, and
// runs the check.
const runInternalCase = (verdicts, reported, internal, write = false) => {
  const dir = mkdtempSync(join(tmpdir(), "corpus-check-"));
  try {
    const verdictsPath = join(dir, "verdicts.json");
    writeFileSync(verdictsPath, JSON.stringify(verdicts));
    writeFileSync(join(dir, "demo.json"), JSON.stringify(report(reported)));
    if (internal !== null) {
      writeFileSync(join(dir, "demo.internal.json"), JSON.stringify(report(internal)));
    }
    const lines = [];
    const code = run({ verdictsPath, resultsDir: dir, write, log: (line) => lines.push(line) });
    return { code, lines, saved: JSON.parse(readFileSync(verdictsPath, "utf8")) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

const withInternal = (findings, internal) => {
  const file = verdictFile(findings);
  file.workspaces.demo.internal = internal;
  return file;
};

test("the internal run is judged against its own verdicts", () => {
  const passed = runInternalCase(
    withInternal({ a: baseline }, { b: baseline }),
    [finding("a")],
    [finding("b")],
  );
  assert.equal(passed.code, 0, passed.lines.join("\n"));
  const failed = runInternalCase(withInternal({ a: baseline }, {}), [finding("a")], [finding("b")]);
  assert.equal(failed.code, 1);
  assert.ok(
    failed.lines.some((line) => line.startsWith("error: demo (internal): new finding b")),
    failed.lines.join("\n"),
  );
});

test("a missing internal result fails when the file has internal verdicts", () => {
  const { code } = runInternalCase(withInternal({ a: baseline }, {}), [finding("a")], null);
  assert.equal(code, 2);
});

test("--write records the internal run in its own section", () => {
  const { code, saved } = runInternalCase(
    withInternal({ a: baseline }, { gone: baseline }),
    [finding("a")],
    [finding("b")],
    true,
  );
  assert.equal(code, 0);
  assert.deepEqual(Object.keys(saved.workspaces.demo.findings), ["a"]);
  assert.deepEqual(Object.keys(saved.workspaces.demo.internal), ["b"]);
});
