import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  REQUIRED_WORKFLOWS,
  errorLines,
  evaluateRuns,
  parseArguments,
  verifyReleaseCi,
} from "./verify-release-ci.mjs";

const SHA = "a".repeat(40);
const OTHER_SHA = "b".repeat(40);
const REPO = "fallow-rs/cargo-fallow";

let nextId = 1;
const run = (file, { status = "completed", conclusion = "success", event = "push", id } = {}) => ({
  id: id ?? nextId++,
  name: file,
  path: `.github/workflows/${file}`,
  status,
  conclusion: status === "completed" ? conclusion : null,
  event,
  html_url: `https://github.com/${REPO}/actions/runs/${id ?? nextId}`,
});

// A workflow with no push trigger gets its run from a dispatch.
const allPassed = () =>
  REQUIRED_WORKFLOWS.map((workflow) =>
    run(workflow.file, { event: workflow.onPush === false ? "workflow_dispatch" : "push" }),
  );

const options = (extra = {}) => ({
  sha: SHA,
  repo: REPO,
  timeoutMinutes: 10,
  pollSeconds: 1,
  missingGraceMinutes: 0,
  dispatchMissing: false,
  ...extra,
});

// A fake clock: each wait moves the time forward.
const clock = () => {
  let time = 0;
  return { now: () => time, wait: async (ms) => void (time += ms) };
};

// The `on:` block of a workflow, without comment lines.
const triggers = (file) => {
  const text = readFileSync(new URL(`../.github/workflows/${file}`, import.meta.url), "utf8");
  const lines = text.split("\n");
  const start = lines.indexOf("on:");
  assert.notEqual(start, -1, `${file}: the on: block is a mapping`);
  const end = lines.findIndex((line, index) => index > start && /^\S/u.test(line));
  return lines
    .slice(start + 1, end)
    .filter((line) => !/^\s*#/u.test(line))
    .join("\n");
};

// True when the workflow runs on a push to main. A `push` without a branch
// filter also runs on main.
const runsOnPushToMain = (file) => {
  const push = /^ {2}push:(?<rest>.*)\n(?<body>(?: {4}.*\n?)*)/mu.exec(triggers(file));
  if (push === null) return false;
  const branches = /^ {4}branches: *\[(?<list>[^\]]*)\]/mu.exec(push.groups.body);
  return branches === null || /(?:^|[\s,'"])main(?:$|[\s,'"])/u.test(branches.groups.list);
};

test("each required workflow runs on a push to main, or the gate dispatches it", () => {
  for (const workflow of REQUIRED_WORKFLOWS) {
    assert.equal(runsOnPushToMain(workflow.file), workflow.onPush !== false, workflow.file);
    assert.equal(
      /^ {2}workflow_dispatch:/mu.test(triggers(workflow.file)),
      workflow.dispatchable !== false,
      `${workflow.file}: dispatchable`,
    );
  }
});

test("each workflow that runs on a push to main is required", () => {
  const dir = new URL("../.github/workflows/", import.meta.url);
  const pushed = readdirSync(dir)
    .filter((file) => file.endsWith(".yml") && runsOnPushToMain(file))
    .toSorted();
  const required = REQUIRED_WORKFLOWS.filter((workflow) => workflow.onPush !== false);
  assert.deepEqual(required.map((workflow) => workflow.file).toSorted(), pushed);
});

test("a required workflow with no push trigger can be dispatched", () => {
  for (const workflow of REQUIRED_WORKFLOWS.filter((entry) => entry.onPush === false)) {
    assert.notEqual(workflow.dispatchable, false, workflow.file);
  }
});

test("a weekly corpus run leaves the corpus run of the release missing", () => {
  const runs = [
    ...allPassed().filter((entry) => !entry.path.endsWith("corpus.yml")),
    run("corpus.yml", { event: "schedule" }),
  ];
  const evaluation = evaluateRuns(runs);
  assert.equal(evaluation.state, "fail");
  assert.deepEqual(
    evaluation.missing.map((workflow) => workflow.file),
    ["corpus.yml"],
  );
});

test("--dispatch-missing starts the corpus run at the first poll", async () => {
  const { now, wait } = clock();
  const started = [];
  const others = allPassed().filter((entry) => !entry.path.endsWith("corpus.yml"));
  const pending = others.map((entry) =>
    entry.path.endsWith("ci.yml") ? { ...entry, status: "in_progress", conclusion: null } : entry,
  );
  let polls = 0;
  const code = await verifyReleaseCi(options({ dispatchMissing: true, missingGraceMinutes: 5 }), {
    now,
    wait,
    log: () => {},
    readMainSha: () => SHA,
    dispatch: (workflow) => {
      started.push({ file: workflow.file, poll: polls });
      return { ok: true };
    },
    readRuns: () => {
      polls += 1;
      if (started.length === 0) return pending;
      return [...others, run("corpus.yml", { id: 60_000, event: "workflow_dispatch" })];
    },
  });
  assert.equal(code, 0);
  assert.deepEqual(started, [{ file: "corpus.yml", poll: 1 }]);
});

test("a weekly run on the release commit does not count", () => {
  const runs = [
    ...allPassed(),
    run("corpus.yml", { event: "schedule", status: "in_progress" }),
    run("hawk.yml", { event: "schedule", conclusion: "failure" }),
    run("ci.yml", { id: 60_000, event: "schedule", conclusion: "failure" }),
  ];
  assert.equal(evaluateRuns(runs).state, "pass");
});

test("a timeout writes the step summary", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "release-ci-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const summary = join(dir, "summary.md");
  const saved = process.env.GITHUB_STEP_SUMMARY;
  process.env.GITHUB_STEP_SUMMARY = summary;
  t.after(() => {
    if (saved === undefined) delete process.env.GITHUB_STEP_SUMMARY;
    else process.env.GITHUB_STEP_SUMMARY = saved;
  });
  const { now, wait } = clock();
  const code = await verifyReleaseCi(options({ timeoutMinutes: 0 }), {
    now,
    wait,
    log: () => {},
    readRuns: () => [...allPassed(), run("ci.yml", { id: 70_000, status: "queued" })],
  });
  assert.equal(code, 1);
  assert.match(readFileSync(summary, "utf8"), /Release CI gate for `a{40}`: wait/u);
});

test("the gate passes when each required workflow passed", () => {
  assert.equal(evaluateRuns(allPassed()).state, "pass");
});

test("pull request runs and release runs do not count", () => {
  const runs = [
    ...allPassed(),
    run("ci.yml", { event: "pull_request", conclusion: "failure" }),
    run("release.yml", { event: "workflow_dispatch", conclusion: "failure" }),
  ];
  assert.equal(evaluateRuns(runs).state, "pass");
});

test("a missing required run waits during the grace period and fails after it", () => {
  const runs = allPassed().filter((entry) => entry.path !== ".github/workflows/fuzz-smoke.yml");
  assert.equal(evaluateRuns(runs, { allowMissing: true }).state, "wait");
  const evaluation = evaluateRuns(runs);
  assert.equal(evaluation.state, "fail");
  assert.match(errorLines(evaluation, SHA)[0], /Fuzz Smoke \(fuzz-smoke\.yml\) has no run/u);
  assert.match(errorLines(evaluation, SHA)[0], /gh workflow run fuzz-smoke\.yml --ref main/u);
});

test("a missing run of a workflow without workflow_dispatch tells the owner to push again", () => {
  const runs = allPassed().filter((entry) => entry.path !== ".github/workflows/commitlint.yml");
  assert.match(errorLines(evaluateRuns(runs), SHA)[0], /no workflow_dispatch trigger/u);
});

test("a required run passes only on success, another run also on skipped", () => {
  const skipped = allPassed().map((entry) =>
    entry.path.endsWith("ci.yml") ? { ...entry, conclusion: "skipped" } : entry,
  );
  assert.equal(evaluateRuns(skipped).state, "fail");
  const extra = [
    ...allPassed(),
    run("hawk.yml", { event: "workflow_dispatch", conclusion: "skipped" }),
  ];
  assert.equal(evaluateRuns(extra).state, "pass");
});

test("a failed run that is not required fails the gate", () => {
  const runs = [
    ...allPassed(),
    run("hawk.yml", { event: "workflow_dispatch", conclusion: "failure" }),
  ];
  const evaluation = evaluateRuns(runs);
  assert.equal(evaluation.state, "fail");
  assert.match(errorLines(evaluation, SHA)[0], /ended with 'failure'.*gh run rerun/u);
});

test("the newest run of a workflow decides", () => {
  const runs = [
    ...allPassed().filter((entry) => !entry.path.endsWith("fuzz-smoke.yml")),
    run("fuzz-smoke.yml", { id: 10_000, conclusion: "cancelled" }),
    run("fuzz-smoke.yml", { id: 10_001, event: "workflow_dispatch" }),
  ];
  assert.equal(evaluateRuns(runs).state, "pass");
});

test("a pending run makes the gate wait", () => {
  const runs = [...allPassed(), run("ci.yml", { id: 20_000, status: "in_progress" })];
  assert.equal(evaluateRuns(runs).state, "wait");
});

test("parseArguments needs a full SHA and an owner/name repository", () => {
  assert.throws(() => parseArguments(["--sha", "abc"], { repo: REPO }), /full 40-character/u);
  assert.throws(() => parseArguments(["--sha", SHA], { repo: "x" }), /owner\/name/u);
  assert.throws(
    () => parseArguments(["--sha", SHA, "--poll-seconds=-1"], { repo: REPO }),
    /--poll-seconds must be a number >= 0/u,
  );
  assert.deepEqual(parseArguments(["--sha", SHA, "--dispatch-missing"], { repo: REPO }), {
    sha: SHA,
    repo: REPO,
    dispatchMissing: true,
    timeoutMinutes: 90,
    pollSeconds: 30,
    missingGraceMinutes: 5,
  });
});

test("verifyReleaseCi waits for a pending run and then passes", async () => {
  const { now, wait } = clock();
  const answers = [
    [...allPassed(), run("ci.yml", { id: 30_000, status: "in_progress" })],
    allPassed(),
  ];
  const lines = [];
  const code = await verifyReleaseCi(options(), {
    now,
    wait,
    log: (line) => lines.push(line),
    readRuns: () => answers.shift() ?? allPassed(),
  });
  assert.equal(code, 0);
  assert.match(lines.at(-1), /Every CI run on a{40} passed/u);
});

test("verifyReleaseCi times out on a run that does not finish", async () => {
  const { now, wait } = clock();
  const lines = [];
  const code = await verifyReleaseCi(options({ timeoutMinutes: 1, pollSeconds: 30 }), {
    now,
    wait,
    log: (line) => lines.push(line),
    readRuns: () => [...allPassed(), run("ci.yml", { id: 40_000, status: "queued" })],
  });
  assert.equal(code, 1);
  assert.ok(lines.some((line) => /Timed out after 1 min/u.test(line)));
});

test("verifyReleaseCi fails after repeated API errors", async () => {
  const { now, wait } = clock();
  const lines = [];
  const code = await verifyReleaseCi(options(), {
    now,
    wait,
    log: (line) => lines.push(line),
    readRuns: () => {
      throw new Error("HTTP 502");
    },
  });
  assert.equal(code, 1);
  assert.ok(lines.some((line) => /The GitHub API failed too often/u.test(line)));
});

test("--dispatch-missing starts a skipped workflow once and waits for it", async () => {
  const { now, wait } = clock();
  const started = [];
  const withoutFuzz = allPassed().filter((entry) => !entry.path.endsWith("fuzz-smoke.yml"));
  const code = await verifyReleaseCi(options({ dispatchMissing: true }), {
    now,
    wait,
    log: () => {},
    readMainSha: () => SHA,
    dispatch: (workflow) => {
      started.push(workflow.file);
      return { ok: true };
    },
    readRuns: () =>
      started.length === 0
        ? withoutFuzz
        : [...withoutFuzz, run("fuzz-smoke.yml", { id: 50_000, event: "workflow_dispatch" })],
  });
  assert.equal(code, 0);
  assert.deepEqual(started, ["fuzz-smoke.yml"]);
});

test("--dispatch-missing starts nothing when main is not the release commit", async () => {
  const { now, wait } = clock();
  const started = [];
  const lines = [];
  const code = await verifyReleaseCi(options({ dispatchMissing: true }), {
    now,
    wait,
    log: (line) => lines.push(line),
    readMainSha: () => OTHER_SHA,
    dispatch: (workflow) => {
      started.push(workflow.file);
      return { ok: true };
    },
    readRuns: () => allPassed().filter((entry) => !entry.path.endsWith("fuzz-smoke.yml")),
  });
  assert.equal(code, 1);
  assert.deepEqual(started, []);
  assert.ok(lines.some((line) => /main is at b{12}/u.test(line)));
});

test("--dispatch-missing starts nothing when a missing workflow has no workflow_dispatch", async () => {
  const { now, wait } = clock();
  const started = [];
  const code = await verifyReleaseCi(options({ dispatchMissing: true }), {
    now,
    wait,
    log: () => {},
    readMainSha: () => SHA,
    dispatch: (workflow) => {
      started.push(workflow.file);
      return { ok: true };
    },
    readRuns: () =>
      allPassed().filter(
        (entry) => !entry.path.endsWith("fuzz-smoke.yml") && !entry.path.endsWith("commitlint.yml"),
      ),
  });
  assert.equal(code, 1);
  assert.deepEqual(started, []);
});
