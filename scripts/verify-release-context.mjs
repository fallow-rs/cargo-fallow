#!/usr/bin/env node

// Checks that a release run starts from the correct commit, before any job
// builds or publishes:
//
//   node scripts/verify-release-context.mjs --version <X.Y.Z or empty> --dry-run <true|false>
//
// A release refuses to start when the ref is not main, when the HEAD subject
// is not `chore: release vX.Y.Z`, when origin/main is not HEAD, when the
// version is not the Cargo.toml version, or when the tag is on the remote.
// A dry run reports the same problems as warnings and continues.

import { spawnSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { runCliMain } from "./cli-main.mjs";

const USAGE =
  "Usage: node scripts/verify-release-context.mjs --version <X.Y.Z or empty> --dry-run <true|false>";
const SEMVER = /^\d+\.\d+\.\d+$/u;
const MAIN_REF = "refs/heads/main";
// `git ls-remote --exit-code` exits with 2 when no ref matches.
const NO_MATCHING_REF = 2;

/** Returns the `version` of the `[package]` table of a Cargo.toml text. */
export const manifestVersionOf = (manifest) => {
  const table = /^\[package\]\s*$(?<body>[\s\S]*?)(?=^\[|(?![\s\S]))/mu.exec(manifest)?.groups.body;
  const version =
    table === undefined ? undefined : /^version = "(?<v>[^"]+)"$/mu.exec(table)?.groups.v;
  if (version === undefined) {
    throw new Error("Cargo.toml has no [package] version");
  }
  return version;
};

/** Returns the version of the run. Only a dry run can omit the input. */
export const resolveVersion = (input, manifestVersion, dryRun) => {
  if (input === "") {
    if (!dryRun) {
      throw new Error("a release needs the version input, for example 0.2.0");
    }
    return manifestVersion;
  }
  if (!SEMVER.test(input)) {
    throw new Error(`the version input "${input}" must be MAJOR.MINOR.PATCH, for example 0.2.0`);
  }
  return input;
};

/** Returns one problem, with the fix, for each fact that blocks a release. */
export const releaseContextProblems = (facts) => {
  const tag = `v${facts.version}`;
  const subject = `chore: release ${tag}`;
  return [
    facts.ref === MAIN_REF
      ? null
      : `The release must run on main, not on ${facts.ref}. Run: gh workflow run release.yml --ref main`,
    facts.subject === subject
      ? null
      : `The HEAD commit subject is '${facts.subject}', expected '${subject}'. Merge the release pull request with --subject "${subject}".`,
    facts.originMain === facts.head
      ? null
      : `origin/main is ${facts.originMain}, not the release commit ${facts.head}. Merge nothing during a release.`,
    facts.manifestVersion === facts.version
      ? null
      : `The version ${facts.version} is not the Cargo.toml version ${facts.manifestVersion}. Set the version in the release commit.`,
    facts.tagExists
      ? `The tag ${tag} already exists on the remote. The owner creates the tag after the run, so prepare a new version.`
      : null,
  ].filter((problem) => problem !== null);
};

const gitRunner = (args) => {
  const result = spawnSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
  return { status: result.status, stdout: result.stdout ?? "" };
};

const required = (result, what) => {
  if (result?.status !== 0) {
    throw new Error(`could not read ${what} (git exit ${result?.status})`);
  }
  return result.stdout.trim();
};

/** Reads the facts of the run from git and the manifest text. */
export const gatherFacts = ({ input, dryRun, ref, manifest, run = gitRunner }) => {
  const manifestVersion = manifestVersionOf(manifest);
  const version = resolveVersion(input, manifestVersion, dryRun);
  const tagRef = `refs/tags/v${version}`;
  const tag = run(["ls-remote", "--exit-code", "--tags", "origin", tagRef]);
  if (tag?.status !== 0 && tag?.status !== NO_MATCHING_REF) {
    throw new Error(`could not read the tag v${version} from origin (git exit ${tag?.status})`);
  }
  return {
    version,
    ref,
    subject: required(run(["log", "-1", "--format=%s"]), "the HEAD subject"),
    head: required(run(["rev-parse", "HEAD"]), "HEAD"),
    originMain: required(
      run(["ls-remote", "--exit-code", "origin", MAIN_REF]),
      "origin/main",
    ).split(/\s/u)[0],
    manifestVersion,
    tagExists: tag.status === 0,
  };
};

/** Prints the result. Returns 1 when a release has a problem, else 0. */
export const verifyReleaseContext = ({ facts, dryRun, log = console.log }) => {
  const problems = releaseContextProblems(facts);
  const level = dryRun ? "warning" : "error";
  for (const problem of problems) {
    log(`::${level}::${problem}`);
  }
  if (problems.length === 0) {
    log(`The release context of v${facts.version} is correct.`);
  }
  return problems.length > 0 && !dryRun ? 1 : 0;
};

export const parseArguments = (argv) => {
  const options = { input: undefined, dryRun: undefined };
  for (let index = 0; index < argv.length; index += 2) {
    const [flag, value] = [argv[index], argv[index + 1]];
    if (flag === "--version") {
      options.input = value ?? "";
    } else if (flag === "--dry-run") {
      options.dryRun = value === "true" ? true : value === "false" ? false : undefined;
    } else {
      throw new Error(`unknown argument: ${flag}\n${USAGE}`);
    }
  }
  if (options.dryRun === undefined) {
    throw new Error(`--dry-run must be true or false\n${USAGE}`);
  }
  return { input: options.input ?? "", dryRun: options.dryRun };
};

const main = () => {
  const { input, dryRun } = parseArguments(process.argv.slice(2));
  const facts = gatherFacts({
    input,
    dryRun,
    ref: process.env.GITHUB_REF ?? "",
    manifest: readFileSync("Cargo.toml", "utf8"),
  });
  if (process.env.GITHUB_OUTPUT !== undefined) {
    appendFileSync(process.env.GITHUB_OUTPUT, `version=${facts.version}\n`);
  }
  return verifyReleaseContext({ facts, dryRun });
};

const invokedPath =
  process.argv[1] === undefined ? null : pathToFileURL(resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  runCliMain(main);
}
