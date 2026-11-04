#!/usr/bin/env node

// Compare the findings of `cargo fallow check` on the corpus workspaces with
// the verdict file. A finding with no verdict fails the check, so a new false
// positive cannot come back unseen. See tests/corpus/README.md.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** The verdicts that a finding can have. */
export const VERDICTS = new Set(["true-positive", "false-positive", "baseline"]);

const USAGE = `Usage: node scripts/corpus-check.mjs --verdicts <file> --results <dir> [--write]

  --verdicts <file>  The verdict file, tests/corpus/verdicts.json.
  --results <dir>    A folder with <workspace>.json, the output of
                     \`cargo fallow check --format json\` in each workspace.
  --write            Add each new finding with the verdict "baseline", and
                     remove each finding that the run no longer reports.`;

/** The findings of a check report that the verdict file judges, keyed by id. */
export const judgedFindings = (report, checks) => {
  const found = new Map();
  for (const finding of report.findings ?? []) {
    if (checks.includes(finding.check)) {
      found.set(finding.id, {
        check: finding.check,
        rule: finding.rule,
        path: finding.location?.path ?? "",
        line: finding.location?.line ?? null,
        message: finding.message ?? "",
      });
    }
  }
  return found;
};

/**
 * Compares the findings of one workspace with its verdicts. Returns the new
 * findings (no verdict), the removed verdicts (no finding) and the known
 * false positives that the run still reports.
 */
export const compare = (found, verdicts) => {
  const added = [];
  const knownFalse = [];
  for (const [id, finding] of found) {
    const verdict = verdicts[id];
    if (verdict === undefined) {
      added.push({ id, ...finding });
    } else if (verdict.verdict === "false-positive") {
      knownFalse.push({ id, ...finding });
    }
  }
  const removed = Object.keys(verdicts)
    .filter((id) => !found.has(id))
    .map((id) => ({ id, ...verdicts[id] }));
  return { added, removed, knownFalse };
};

/**
 * The problems of a check report that make its findings untrustworthy: a
 * file that did not parse, a judged check that did not run, or an unknown
 * shape. A missing finding then looks like a fixed one.
 */
export const reportProblems = (report, checks) => {
  if (!Array.isArray(report?.findings) || !Array.isArray(report?.checks)) {
    return ["the result is not a `cargo fallow check` report"];
  }
  if (report.schema_version !== 1) {
    return [`the report has the unknown schema_version ${report.schema_version}`];
  }
  const problems = checks
    .filter((check) => !report.checks.includes(check))
    .map((check) => `the check \`${check}\` did not run`);
  for (const finding of report.findings) {
    if (finding.check === "parse") {
      problems.push(`${finding.location?.path ?? "a file"} did not parse`);
    }
  }
  return problems;
};

/** The problems of the shape of a verdict file, as messages. */
export const validate = (file) => {
  const problems = [];
  if (typeof file.workspaces !== "object" || file.workspaces === null) {
    problems.push("`workspaces` must be an object");
  }
  if (!Array.isArray(file.checks) || file.checks.length === 0) {
    problems.push("`checks` must be a list of check names");
  }
  for (const [name, workspace] of Object.entries(file.workspaces ?? {})) {
    if (!/^[0-9a-f]{40}$/u.test(workspace.commit ?? "")) {
      problems.push(`${name}: \`commit\` must be a full commit hash`);
    }
    if (typeof workspace.repository !== "string") {
      problems.push(`${name}: \`repository\` must be a URL`);
    }
    for (const [id, entry] of Object.entries(workspace.findings ?? {})) {
      if (!VERDICTS.has(entry.verdict)) {
        problems.push(`${name}: ${id} has the unknown verdict \`${entry.verdict}\``);
      }
    }
  }
  return problems;
};

const place = (finding) =>
  `${finding.path}${finding.line === null || finding.line === undefined ? "" : `:${finding.line}`}`;

/** The verdict entry of a new finding for --write. */
const baselineEntry = (finding) => ({
  check: finding.check,
  rule: finding.rule,
  path: finding.path,
  line: finding.line,
  verdict: "baseline",
});

/** Runs the check and returns the exit code. */
export const run = ({ verdictsPath, resultsDir, write, log = console.log }) => {
  const file = readJson(verdictsPath, log);
  if (file === null) {
    return 2;
  }
  const problems = validate(file);
  if (problems.length > 0) {
    for (const problem of problems) {
      log(`error: ${problem}`);
    }
    return 2;
  }
  let failed = false;
  let broken = false;
  for (const [name, workspace] of Object.entries(file.workspaces)) {
    const resultPath = join(resultsDir, `${name}.json`);
    if (!existsSync(resultPath)) {
      log(`error: ${name}: no result file ${resultPath}`);
      broken = true;
      continue;
    }
    const report = readJson(resultPath, log);
    const reportIssues =
      report === null ? ["the result is not JSON"] : reportProblems(report, file.checks);
    if (reportIssues.length > 0) {
      for (const problem of reportIssues) {
        log(`error: ${name}: ${problem}`);
      }
      broken = true;
      continue;
    }
    const found = judgedFindings(report, file.checks);
    const verdicts = workspace.findings ?? {};
    const { added, removed, knownFalse } = compare(found, verdicts);
    for (const finding of added) {
      log(
        `error: ${name}: new finding ${finding.id} ${finding.check}/${finding.rule} at ${place(finding)}: ${finding.message}`,
      );
    }
    for (const finding of removed) {
      log(
        `note: ${name}: ${finding.id} ${finding.check}/${finding.rule} at ${place(finding)} is gone`,
      );
    }
    for (const finding of knownFalse) {
      log(`note: ${name}: known false positive ${finding.id} at ${place(finding)}`);
    }
    log(
      `${name}: ${found.size} findings, ${added.length} new, ${removed.length} gone, ${knownFalse.length} known false positives`,
    );
    if (write) {
      for (const finding of added) {
        verdicts[finding.id] = baselineEntry(finding);
      }
      for (const finding of removed) {
        delete verdicts[finding.id];
      }
      workspace.findings = Object.fromEntries(
        Object.entries(verdicts).toSorted(([a], [b]) => a.localeCompare(b)),
      );
    } else if (added.length > 0) {
      failed = true;
    }
  }
  if (broken) {
    log("The run cannot judge each workspace, so the verdict file stays as it is.");
    return 2;
  }
  if (write) {
    writeFileSync(verdictsPath, `${JSON.stringify(file, null, 2)}\n`);
    log(`wrote ${verdictsPath}`);
    return 0;
  }
  if (failed) {
    log(
      "Each new finding needs a verdict. Check it by hand, then add it to tests/corpus/verdicts.json.",
    );
  }
  return failed ? 1 : 0;
};

/** The JSON of a file, or null with an error line when it does not parse. */
const readJson = (path, log) => {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    log(`error: cannot read ${path}: ${error.message}`);
    return null;
  }
};

const parseArgs = (argv) => {
  const options = { write: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--verdicts") {
      options.verdictsPath = argv[(index += 1)];
    } else if (arg === "--results") {
      options.resultsDir = argv[(index += 1)];
    } else if (arg === "--write") {
      options.write = true;
    } else {
      return null;
    }
  }
  return options.verdictsPath && options.resultsDir ? options : null;
};

if (import.meta.url === pathToFileURL(resolve(process.argv[1] ?? "")).href) {
  const options = parseArgs(process.argv.slice(2));
  if (options === null) {
    console.error(USAGE);
    process.exit(2);
  }
  process.exit(run(options));
}
