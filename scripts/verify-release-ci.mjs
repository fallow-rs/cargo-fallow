#!/usr/bin/env node

// Release gate: a release does not start until each workflow passed on the
// release commit. A pull request runs only part of CI, and the push to main
// runs the rest. This script reads the workflow runs of the release SHA. It
// waits until each required workflow has a successful run, and it fails when
// a required run is missing or when any run on that SHA failed.
//
// Usage: node scripts/verify-release-ci.mjs --sha <sha> [--repo owner/name]
//        [--timeout-minutes 90] [--poll-seconds 30]
//        [--missing-grace-minutes 5] [--dispatch-missing]
//
// The script calls `gh`. In GitHub Actions, set GH_TOKEN with `actions: read`.
// --dispatch-missing starts each required workflow that has no run, one time,
// with `gh workflow run <file> --ref main`. It starts a run only when main on
// GitHub is the release SHA, because `--ref main` runs the tip of main.

import { spawnSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

/**
 * The workflows that must have a successful run on the release SHA. Each one
 * runs on push to main. `fuzz-smoke.yml` has a path filter, so a release
 * commit can have no run of it. `dispatchable: false` marks a workflow
 * without a `workflow_dispatch` trigger.
 *
 * Update this list when a workflow gets or loses the push trigger on main.
 * `scripts/verify-release-ci.test.mjs` compares it with the workflow files.
 */
export const REQUIRED_WORKFLOWS = Object.freeze([
  { name: "CI", file: "ci.yml" },
  { name: "Commitlint", file: "commitlint.yml", dispatchable: false },
  { name: "Fuzz Smoke", file: "fuzz-smoke.yml" },
  { name: "GitHub Action", file: "github-action.yml" },
  { name: "GitLab template", file: "gitlab-template.yml", dispatchable: false },
]);

const WORKFLOW_DIR = ".github/workflows/";
// A pull request run tests a merge ref, not the release commit. A weekly run
// is an audit, not a release gate: a weekly run that is slow or failed must
// not stop a release.
const IGNORED_EVENTS = new Set([
  "pull_request",
  "pull_request_target",
  "merge_group",
  "dynamic",
  "schedule",
]);
// An earlier failed release attempt must not block a new attempt.
const IGNORED_FILES = new Set(["release.yml", "release-published.yml"]);
const PASSING_CONCLUSIONS = new Set(["success", "skipped", "neutral"]);
const MAX_API_ERRORS = 5;
const MAIN = "main";
const SHORT_SHA = 12;

const DEFAULTS = Object.freeze({
  "timeout-minutes": { key: "timeoutMinutes", value: 90 },
  "poll-seconds": { key: "pollSeconds", value: 30 },
  "missing-grace-minutes": { key: "missingGraceMinutes", value: 5 },
});

const shortSha = (sha) => String(sha).slice(0, SHORT_SHA);

/** The workflow file name of a run, for example `ci.yml`. */
const workflowFile = (run) => {
  const path = String(run.path ?? "").split("@")[0];
  return path.startsWith(WORKFLOW_DIR) ? path.slice(WORKFLOW_DIR.length) : path;
};

// The newest run of each workflow file. A re-run keeps its run ID, and the
// API gives the state of its last attempt, so a re-run counts.
const latestRunPerWorkflow = (runs) => {
  const latest = new Map();
  for (const run of runs) {
    const file = workflowFile(run);
    if (IGNORED_EVENTS.has(run.event) || IGNORED_FILES.has(file)) continue;
    const current = latest.get(file);
    if (current === undefined || run.id > current.id) latest.set(file, run);
  }
  return latest;
};

const classifyRun = (run, required) => {
  if (run.status !== "completed") return "pending";
  if (required) return run.conclusion === "success" ? "pass" : "fail";
  return PASSING_CONCLUSIONS.has(run.conclusion) ? "pass" : "fail";
};

/**
 * The gate state of the runs on the release SHA: `pass`, `fail` or `wait`.
 * `allowMissing` is true while a push run can still appear.
 */
export const evaluateRuns = (
  runs,
  { required = REQUIRED_WORKFLOWS, allowMissing = false } = {},
) => {
  const latest = latestRunPerWorkflow(runs);
  const requiredFiles = new Set(required.map((workflow) => workflow.file));
  const result = {
    passed: [],
    pending: [],
    failed: [],
    missing: required.filter((workflow) => !latest.has(workflow.file)),
  };
  for (const [file, run] of latest) {
    const state = classifyRun(run, requiredFiles.has(file));
    const list = { pass: result.passed, pending: result.pending, fail: result.failed }[state];
    list.push({ file, name: run.name, run });
  }
  let state = "pass";
  if (result.failed.length > 0) state = "fail";
  else if (result.pending.length > 0) state = "wait";
  else if (result.missing.length > 0) state = allowMissing ? "wait" : "fail";
  return { state, ...result };
};

const missingRunHint = (workflow) =>
  workflow.dispatchable === false
    ? "It has no workflow_dispatch trigger. Push a new commit to main and prepare the release from that commit."
    : `Start it with: gh workflow run ${workflow.file} --ref ${MAIN}, or run this gate with --dispatch-missing.`;

/** The `::error::` lines of a failed gate. */
export const errorLines = (evaluation, sha) => [
  ...evaluation.missing.map(
    (workflow) =>
      `::error title=Missing CI run::${workflow.name} (${workflow.file}) has no run for ${sha}. ${missingRunHint(workflow)}`,
  ),
  ...evaluation.failed.map(
    (entry) =>
      `::error title=Failed CI run::${entry.name} (${entry.file}) ended with '${entry.run.conclusion ?? entry.run.status}' on ${sha}: ` +
      `${entry.run.html_url}. Fix main, or run it again with: gh run rerun ${entry.run.id} --failed`,
  ),
];

const summaryTable = (evaluation, sha) => {
  const rows = [
    ...evaluation.failed.map((entry) => [entry.name, entry.run.conclusion, "fail"]),
    ...evaluation.missing.map((workflow) => [workflow.name, "no run", "missing"]),
    ...evaluation.pending.map((entry) => [entry.name, entry.run.status, "pending"]),
    ...evaluation.passed.map((entry) => [entry.name, entry.run.conclusion, "pass"]),
  ];
  return [
    `### Release CI gate for \`${sha}\`: ${evaluation.state}`,
    "",
    "| Workflow | Result | Gate |",
    "| --- | --- | --- |",
    ...rows.map((row) => `| ${row.join(" | ")} |`),
    "",
  ].join("\n");
};

const describe = (evaluation) => {
  const names = (list) => list.map((entry) => entry.name).join(", ") || "none";
  return (
    `pass ${evaluation.passed.length}, pending ${evaluation.pending.length} (${names(evaluation.pending)}), ` +
    `missing ${evaluation.missing.length} (${names(evaluation.missing)})`
  );
};

const nonNegativeNumber = (name, value) => {
  const number = Number(value);
  if (value === "" || !Number.isFinite(number) || number < 0) {
    throw new Error(`--${name} must be a number >= 0`);
  }
  return number;
};

/** Reads the command line. `repo` is the default of `--repo`. */
export const parseArguments = (argv, { repo = process.env.GITHUB_REPOSITORY } = {}) => {
  const numeric = Object.fromEntries(
    Object.entries(DEFAULTS).map(([name, { value }]) => [
      name,
      { type: "string", default: String(value) },
    ]),
  );
  const { values } = parseArgs({
    args: argv,
    options: {
      sha: { type: "string" },
      repo: { type: "string", default: repo },
      "dispatch-missing": { type: "boolean", default: false },
      ...numeric,
    },
    strict: true,
  });
  if (!/^[0-9a-f]{40}$/u.test(values.sha ?? "")) {
    throw new Error("--sha must be a full 40-character commit SHA");
  }
  if (!/^[\w.-]+\/[\w.-]+$/u.test(values.repo ?? "")) {
    throw new Error("--repo or GITHUB_REPOSITORY must be owner/name");
  }
  const options = {
    sha: values.sha,
    repo: values.repo,
    dispatchMissing: values["dispatch-missing"],
  };
  for (const [name, { key }] of Object.entries(DEFAULTS)) {
    options[key] = nonNegativeNumber(name, values[name]);
  }
  return options;
};

const gh = (args) => {
  const result = spawnSync("gh", args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(result.stderr.trim() || `gh ${args[0]} exited with ${result.status}`);
  }
  return result.stdout;
};

const ghRuns = (repo, sha) =>
  gh([
    "api",
    "--paginate",
    "--jq",
    ".workflow_runs[]",
    `repos/${repo}/actions/runs?head_sha=${sha}&per_page=100`,
  ])
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));

const ghMainSha = (repo) =>
  gh(["api", "--jq", ".object.sha", `repos/${repo}/git/ref/heads/${MAIN}`]).trim();

const ghDispatch = (repo) => (workflow) => {
  try {
    gh(["workflow", "run", workflow.file, "--ref", MAIN, "--repo", repo]);
    return { ok: true };
  } catch (error) {
    return { ok: false, error: error.message };
  }
};

// Starts each missing required workflow one time. It starts nothing when a
// missing workflow has no workflow_dispatch trigger, because the gate then
// fails anyway. Returns `{ ok, started }`.
const dispatchMissingRuns = async (evaluation, { sha, dispatched, readMainSha, dispatch, log }) => {
  if (evaluation.missing.some((workflow) => workflow.dispatchable === false)) {
    return { ok: true, started: 0 };
  }
  const missing = evaluation.missing.filter((workflow) => !dispatched.has(workflow.file));
  if (missing.length === 0) return { ok: true, started: 0 };
  const mainSha = await readMainSha();
  if (mainSha !== sha) {
    log(
      `::error::--dispatch-missing starts runs only while ${MAIN} is the release commit ${shortSha(sha)}, ` +
        `but ${MAIN} is at ${shortSha(mainSha)}. Prepare the release again from the new commit.`,
    );
    return { ok: false, started: 0 };
  }
  for (const workflow of missing) {
    const result = dispatch(workflow);
    if (!result.ok) {
      log(`::error::Could not start ${workflow.name} (${workflow.file}): ${result.error}`);
      return { ok: false, started: 0 };
    }
    dispatched.add(workflow.file);
    log(
      `Started ${workflow.name}, which had no run for ${sha}: gh workflow run ${workflow.file} --ref ${MAIN}`,
    );
  }
  return { ok: true, started: missing.length };
};

const writeSummary = (evaluation, sha) => {
  if (process.env.GITHUB_STEP_SUMMARY) {
    appendFileSync(process.env.GITHUB_STEP_SUMMARY, summaryTable(evaluation, sha));
  }
};

const reportTimeout = (options, evaluation, log) => {
  log(
    `::error::Timed out after ${options.timeoutMinutes} min while the CI runs on ${options.sha} did not finish.`,
  );
  if (evaluation === undefined) return;
  writeSummary(evaluation, options.sha);
  log(describe(evaluation));
  for (const entry of evaluation.pending) {
    log(
      `::error title=Unfinished CI run::${entry.name} is still ${entry.run.status}: ${entry.run.html_url}`,
    );
  }
  for (const line of errorLines(evaluation, options.sha)) log(line);
};

const reportResult = (options, evaluation, log) => {
  writeSummary(evaluation, options.sha);
  if (evaluation.state === "fail") {
    for (const line of errorLines(evaluation, options.sha)) log(line);
    log("A release does not start until each check passed on the release commit.");
    return 1;
  }
  log(
    `Every CI run on ${options.sha} passed: ${evaluation.passed.map((entry) => entry.name).join(", ")}.`,
  );
  return 0;
};

/** Polls until the gate passes, fails or times out. Returns the exit code. */
export const verifyReleaseCi = async (
  options,
  {
    required = REQUIRED_WORKFLOWS,
    now = Date.now,
    wait = sleep,
    log = console.log,
    readRuns = () => ghRuns(options.repo, options.sha),
    readMainSha = () => ghMainSha(options.repo),
    dispatch = ghDispatch(options.repo),
  } = {},
) => {
  const deadline = now() + options.timeoutMinutes * 60_000;
  const dispatched = new Set();
  // A started run needs some time to appear, so a start begins a new grace period.
  let graceStart = now();
  let apiErrors = 0;
  let evaluation;

  for (;;) {
    try {
      const runs = await readRuns();
      apiErrors = 0;
      const allowMissing = now() - graceStart < options.missingGraceMinutes * 60_000;
      evaluation = evaluateRuns(runs, { required, allowMissing });
      if (
        options.dispatchMissing &&
        evaluation.state === "fail" &&
        evaluation.failed.length === 0
      ) {
        const result = await dispatchMissingRuns(evaluation, {
          sha: options.sha,
          dispatched,
          readMainSha,
          dispatch,
          log,
        });
        if (!result.ok) return 1;
        if (result.started > 0) {
          graceStart = now();
          evaluation = { ...evaluation, state: "wait" };
        }
      }
    } catch (error) {
      apiErrors += 1;
      log(
        `::warning::Could not read the workflow runs (${apiErrors}/${MAX_API_ERRORS}): ${error.message}`,
      );
      if (apiErrors >= MAX_API_ERRORS) {
        log(
          "::error::The GitHub API failed too often. The CI state of the release is unknown, so the gate fails.",
        );
        return 1;
      }
    }
    if (evaluation !== undefined && evaluation.state !== "wait") break;
    if (now() + options.pollSeconds * 1000 > deadline) {
      reportTimeout(options, evaluation, log);
      return 1;
    }
    if (evaluation !== undefined) log(`Waiting for CI on ${options.sha}: ${describe(evaluation)}`);
    await wait(options.pollSeconds * 1000);
  }
  return reportResult(options, evaluation, log);
};

const localRepo = () => {
  const result = spawnSync("git", ["remote", "get-url", "origin"], { encoding: "utf8" });
  const url = result.status === 0 ? result.stdout.trim() : "";
  return /github\.com[:/]([\w.-]+\/[\w.-]+?)(?:\.git)?\/?$/u.exec(url)?.[1];
};

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const repo = process.env.GITHUB_REPOSITORY || localRepo();
    process.exitCode = await verifyReleaseCi(parseArguments(process.argv.slice(2), { repo }));
  } catch (error) {
    console.log(`::error::${error.message}`);
    process.exitCode = 1;
  }
}
