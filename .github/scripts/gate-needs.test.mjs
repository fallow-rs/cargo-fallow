// Policy tests for the gate jobs that branch protection requires. A gate job
// passes only when each job in its `needs` list passes, so a job that the
// list leaves out cannot block a merge.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const ROOT = new URL("../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

/** The required gate job of each workflow that has one. */
const GATES = [
  { workflow: ".github/workflows/ci.yml", gate: "ci-ok" },
  { workflow: ".github/workflows/github-action.yml", gate: "action-success" },
];

/** Returns the lines of the `jobs:` block. */
const jobsBlock = (source) => {
  const lines = source.split(/\r?\n/u);
  const start = lines.indexOf("jobs:");
  assert.notEqual(start, -1, "missing jobs block");
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => /^\S/u.test(line) && !line.startsWith("#"));
  return end === -1 ? rest : rest.slice(0, end);
};

/**
 * Returns the job names under `jobs:`. A line at two spaces that is not a
 * comment and not a plain job key fails the test, so a job in another form
 * cannot leave the gate check.
 */
const jobNames = (source) =>
  jobsBlock(source).flatMap((line) => {
    if (!/^ {2}\S/u.test(line) || line.trimStart().startsWith("#")) {
      return [];
    }
    const match = /^ {2}([A-Za-z_][\w-]*):\s*(?:#.*)?$/u.exec(line);
    assert.ok(match, `unknown job key form: ${line}`);
    return [match[1]];
  });

/** Returns the inline `needs: [...]` list of one job. */
const needsOf = (source, job) => {
  const lines = jobsBlock(source);
  const start = lines.indexOf(`  ${job}:`);
  assert.notEqual(start, -1, `missing job ${job}`);
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => /^ {2}[a-z]/u.test(line));
  const body = end === -1 ? rest : rest.slice(0, end);
  const needs = body.map((line) => /^ {4}needs: \[(.*)\]$/u.exec(line)).find(Boolean);
  assert.ok(needs, `${job} declares its needs as an inline list`);
  return needs[1].split(",").map((name) => name.trim());
};

/** Returns the jobs that the gate job does not need, in file order. */
const missingNeeds = (source, gate) => {
  const needs = new Set(needsOf(source, gate));
  return jobNames(source).filter((job) => job !== gate && !needs.has(job));
};

/** Builds a workflow with the given lines under `jobs:`. */
const fixture = (...jobLines) => ["on: push", "jobs:", ...jobLines, ""].join("\n");

test("missingNeeds reports a job that the gate leaves out", () => {
  const source = fixture(
    "  build:",
    "    runs-on: ubuntu-latest",
    "  extra: # a trailing comment",
    "    runs-on: ubuntu-latest",
    "  gate:",
    "    needs: [build]",
    "    runs-on: ubuntu-latest",
  );
  assert.deepEqual(missingNeeds(source, "gate"), ["extra"]);
});

test("missingNeeds returns no job when the gate needs each job", () => {
  const source = fixture("  build:", "    runs-on: ubuntu-latest", "  gate:", "    needs: [build]");
  assert.deepEqual(missingNeeds(source, "gate"), []);
});

test("missingNeeds fails on a needs list over more lines", () => {
  const source = fixture(
    "  build:",
    "    runs-on: ubuntu-latest",
    "  gate:",
    "    needs:",
    "      - build",
  );
  assert.throws(() => missingNeeds(source, "gate"), /inline list/u);
});

test("missingNeeds fails on a quoted job key", () => {
  const source = fixture(
    '  "build":',
    "    runs-on: ubuntu-latest",
    "  gate:",
    "    needs: [build]",
  );
  assert.throws(() => missingNeeds(source, "gate"), /unknown job key form/u);
});

for (const { workflow, gate } of GATES) {
  test(`${gate} in ${workflow} needs every other job`, () => {
    assert.deepEqual(missingNeeds(read(workflow), gate), []);
  });
}
