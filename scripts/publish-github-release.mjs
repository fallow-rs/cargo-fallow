#!/usr/bin/env node

// The last step of a release. The owner runs it after the `Release ready for
// signed tag` job of the release workflow passed:
//
//   node scripts/publish-github-release.mjs --run <run ID> --version X.Y.Z \
//     --title "vX.Y.Z: <summary>" --notes <notes file> [--check-only]
//
// The script checks the title, the notes, the run, the release commit, the
// absent tag and the `release-assets` artifact of the run. Then it creates the
// signed tag on the release commit, pushes it, and creates the GitHub release
// with the assets. `--check-only` runs each check and changes nothing.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readdirSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs as parseOptions } from "node:util";

import { runCliMain } from "./cli-main.mjs";
import { assertChangelogRelease } from "./verify-release-metadata.mjs";

const REPOSITORY = "fallow-rs/cargo-fallow";
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const VERIFY_SIGNATURE = join(ROOT, "action/scripts/verify-signature.mjs");
const EM_DASH = String.fromCodePoint(0x2014);
const SUMS = "SHA256SUMS";
const READY_JOB = "Release ready for signed tag";
// `git ls-remote --exit-code` exits with 2 when no ref matches.
const NO_MATCHING_REF = 2;
const SUM_LINE = /^(?<hash>[0-9a-f]{64}) [ *](?<name>\S+)$/u;

/** The targets of the `build` job in release.yml, with the archive format. */
const TARGETS = [
  ["x86_64-unknown-linux-musl", "tar.gz"],
  ["aarch64-unknown-linux-musl", "tar.gz"],
  ["x86_64-apple-darwin", "tar.gz"],
  ["aarch64-apple-darwin", "tar.gz"],
  ["x86_64-pc-windows-msvc", "zip"],
];

const USAGE = `Usage: node scripts/publish-github-release.mjs --run <run ID> --version X.Y.Z --title "vX.Y.Z: <summary>" --notes <file> [--check-only]`;

const archiveNames = (version) =>
  TARGETS.map(([target, format]) => `cargo-fallow-${version}-${target}.${format}`);

/** Returns the complete asset set of a release, in a fixed order. */
export const expectedAssetNames = (version) => [
  ...archiveNames(version).flatMap((archive) => [archive, `${archive}.sha256`, `${archive}.sig`]),
  SUMS,
];

const sha256Of = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");

const sumLines = (text) =>
  text
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter((line) => line !== "");

const checksumProblems = (dir, version, present) => {
  if (!present.has(SUMS)) {
    return [];
  }
  const lines = sumLines(readFileSync(join(dir, SUMS), "utf8"));
  const hashes = new Map(
    lines
      .map((line) => SUM_LINE.exec(line)?.groups)
      .filter(Boolean)
      .map((g) => [g.name, g.hash]),
  );
  const archives = archiveNames(version).filter((name) => present.has(name));
  const problems = archives
    .filter((name) => hashes.get(name) !== sha256Of(join(dir, name)))
    .map((name) => `${name} does not match its line in ${SUMS}`);
  const listed = archives
    .filter((name) => present.has(`${name}.sha256`))
    .flatMap((name) => sumLines(readFileSync(join(dir, `${name}.sha256`), "utf8")));
  if (listed.toSorted().join("\n") !== lines.toSorted().join("\n")) {
    problems.push(`${SUMS} is not the list of the .sha256 files of the archives`);
  }
  return problems;
};

/** Returns the problems of the downloaded `release-assets` directory. */
export const assetProblems = (dir, version) => {
  const expected = expectedAssetNames(version);
  const present = new Set(readdirSync(dir));
  return [
    ...expected.filter((name) => !present.has(name)).map((name) => `release-assets has no ${name}`),
    ...[...present]
      .filter((name) => !expected.includes(name))
      .toSorted()
      .map((name) => `release-assets has the unexpected file ${name}`),
    ...expected
      .filter((name) => present.has(name) && statSync(join(dir, name)).size === 0)
      .map((name) => `${name} is empty`),
    ...checksumProblems(dir, version, present),
  ];
};

/** Returns the problems of the release title. */
export const titleProblems = (title, tag) => {
  const prefix = `${tag}: `;
  return [
    title.startsWith(prefix) && title.slice(prefix.length).trim() !== ""
      ? null
      : `The title must start with "${prefix}" and give a summary`,
    title.includes(EM_DASH) ? "The title contains an em dash" : null,
  ].filter((problem) => problem !== null);
};

/** Returns the problems of the release notes text. */
export const notesProblems = (text, compareUrl) =>
  [
    text.trim() === "" ? "The notes file is empty" : null,
    text.includes(EM_DASH) ? "The notes contain an em dash" : null,
    text.trimEnd().endsWith(compareUrl) ? null : `The notes must end with ${compareUrl}`,
  ].filter((problem) => problem !== null);

/** Returns the problems of a `gh run view --json` document of the release run. */
export const runProblems = (run) => {
  const ready = run.jobs?.find((job) => job.name === READY_JOB);
  return [
    run.workflowName === "Release"
      ? null
      : `The run is of the workflow ${run.workflowName}, not Release`,
    run.event === "workflow_dispatch"
      ? null
      : `The run started from ${run.event}, not workflow_dispatch`,
    run.headBranch === "main" ? null : `The run is on ${run.headBranch}, not main`,
    run.status === "completed" && run.conclusion === "success"
      ? null
      : `The run is ${run.status} with the conclusion ${run.conclusion || "none"}, not success`,
    ready === undefined ? `The run has no job "${READY_JOB}"` : null,
    ready !== undefined && ready.conclusion !== "success"
      ? `The job "${READY_JOB}" has the conclusion ${ready.conclusion || "none"}, not success`
      : null,
  ].filter((problem) => problem !== null);
};

const commandRunner = (command, args) => {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "inherit"],
  });
  return { status: result.status, stdout: result.stdout ?? "" };
};

/** A step that failed. The CLI prints the problems and exits with 1. */
class Stop extends Error {
  constructor(problems) {
    super(problems.join("\n"));
    this.problems = problems;
  }
}

const stopOn = (problems) => {
  if (problems.length > 0) {
    throw new Stop(problems);
  }
};

const releaseSteps = (options, { run, log, tempDir }) => {
  const mustRun = (command, args, what) => {
    const result = run(command, args);
    if (result?.status !== 0) {
      throw new Error(`${what} failed: ${command} ${args.join(" ")} (exit ${result?.status})`);
    }
    return result.stdout;
  };
  const tag = `v${options.version}`;
  const tagRef = `refs/tags/${tag}`;
  const assertTagAbsent = () => {
    const remote = run("git", ["ls-remote", "--exit-code", "--tags", "origin", tagRef]);
    if (remote?.status === 0) {
      stopOn([`The tag ${tag} already exists on the remote`]);
    }
    if (remote?.status !== NO_MATCHING_REF) {
      throw new Error(`could not read the tag ${tag} from origin (git exit ${remote?.status})`);
    }
    stopOn(
      run("git", ["rev-parse", "--verify", "--quiet", tagRef])?.status === 0
        ? [`The tag ${tag} already exists in this clone. Delete the local tag first.`]
        : [],
    );
  };

  stopOn(titleProblems(options.title, tag));
  const notes = readFileSync(options.notes, "utf8");

  const runJson = JSON.parse(
    mustRun(
      "gh",
      [
        "run",
        "view",
        options.runId,
        "--repo",
        REPOSITORY,
        "--json",
        "headSha,headBranch,event,status,conclusion,workflowName,jobs",
      ],
      "Reading the release run",
    ),
  );
  stopOn(runProblems(runJson));
  const sha = runJson.headSha;

  mustRun("git", ["fetch", "--quiet", "origin", "main"], "Fetching origin/main");
  const main = mustRun(
    "git",
    ["ls-remote", "--exit-code", "origin", "refs/heads/main"],
    "Reading origin/main",
  );
  const mainSha = main.trim().split(/\s/u)[0];
  const subject = mustRun(
    "git",
    ["log", "-1", "--format=%s", sha],
    "Reading the release commit",
  ).trim();
  stopOn(
    [
      mainSha === sha ? null : `origin/main is ${mainSha}, not the release commit ${sha}`,
      subject === `chore: release ${tag}`
        ? null
        : `The release commit subject is '${subject}', expected 'chore: release ${tag}'`,
    ].filter((problem) => problem !== null),
  );
  assertTagAbsent();

  const changelog = mustRun("git", ["show", `${sha}:CHANGELOG.md`], "Reading the CHANGELOG");
  const { compareUrl } = assertChangelogRelease(changelog, tag);
  stopOn(notesProblems(notes, compareUrl));

  const dir = tempDir();
  mustRun(
    "gh",
    [
      "run",
      "download",
      options.runId,
      "--repo",
      REPOSITORY,
      "--name",
      "release-assets",
      "--dir",
      dir,
    ],
    "Downloading release-assets",
  );
  stopOn(assetProblems(dir, options.version));
  for (const archive of archiveNames(options.version)) {
    mustRun(
      process.execPath,
      [VERIFY_SIGNATURE, join(dir, archive), join(dir, `${archive}.sig`)],
      `Checking the signature of ${archive}`,
    );
  }
  log(`Checked the run ${options.runId}, the release commit ${sha}, the notes and the assets.`);

  if (options.checkOnly) {
    log("--check-only: no tag and no release created.");
    return;
  }
  assertTagAbsent();
  mustRun(
    "git",
    ["tag", "-s", tag, sha, "-m", `cargo-fallow ${options.version}`],
    "Creating the tag",
  );
  mustRun("git", ["verify-tag", tag], "Checking the tag signature");
  mustRun("git", ["push", "origin", tagRef], "Pushing the tag");
  mustRun(
    "gh",
    [
      "release",
      "create",
      tag,
      "--repo",
      REPOSITORY,
      "--verify-tag",
      "--title",
      options.title,
      "--notes-file",
      options.notes,
      ...expectedAssetNames(options.version).map((name) => join(dir, name)),
    ],
    "Creating the GitHub release",
  );
  log(`Created the signed tag ${tag} and the GitHub release.`);
};

/** Runs the release step. Returns 1 when a check fails, else 0. */
export const publishGithubRelease = (
  options,
  {
    run = commandRunner,
    log = console.log,
    tempDir = () => mkdtempSync(join(tmpdir(), "cargo-fallow-release-")),
  } = {},
) => {
  try {
    releaseSteps(options, { run, log, tempDir });
    return 0;
  } catch (error) {
    if (!(error instanceof Stop)) {
      throw error;
    }
    for (const problem of error.problems) {
      log(`error: ${problem}`);
    }
    log("Nothing was tagged or published.");
    return 1;
  }
};

export const parseArguments = (argv) => {
  const { values } = parseOptions({
    args: argv,
    options: {
      run: { type: "string" },
      version: { type: "string" },
      title: { type: "string" },
      notes: { type: "string" },
      "check-only": { type: "boolean" },
    },
    strict: true,
  });
  if ([values.run, values.version, values.title, values.notes].includes(undefined)) {
    throw new Error(USAGE);
  }
  if (!/^\d+$/u.test(values.run)) {
    throw new Error(`--run must be a workflow run ID, found "${values.run}"`);
  }
  if (!/^\d+\.\d+\.\d+$/u.test(values.version)) {
    throw new Error(`--version must be MAJOR.MINOR.PATCH, found "${values.version}"`);
  }
  return {
    runId: values.run,
    version: values.version,
    title: values.title,
    notes: values.notes,
    checkOnly: values["check-only"] === true,
  };
};

const invokedPath =
  process.argv[1] === undefined ? null : pathToFileURL(resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  runCliMain(() => publishGithubRelease(parseArguments(process.argv.slice(2))));
}
