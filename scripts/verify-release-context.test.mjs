import assert from "node:assert/strict";
import test from "node:test";

import {
  gatherFacts,
  manifestVersionOf,
  parseArguments,
  releaseContextProblems,
  resolveVersion,
  verifyReleaseContext,
} from "./verify-release-context.mjs";

const HEAD = "a".repeat(40);
const OTHER = "b".repeat(40);

const facts = (overrides = {}) => ({
  version: "0.2.0",
  ref: "refs/heads/main",
  subject: "chore: release v0.2.0",
  head: HEAD,
  originMain: HEAD,
  manifestVersion: "0.2.0",
  tagExists: false,
  ...overrides,
});

const MANIFEST = [
  "[package]",
  'name = "cargo-fallow"',
  'version = "0.2.0"',
  "",
  "[dependencies]",
  'serde = { version = "1.0.0" }',
  "",
].join("\n");

test("the manifest version comes from the [package] table", () => {
  assert.equal(manifestVersionOf(MANIFEST), "0.2.0");
  assert.throws(
    () => manifestVersionOf("[workspace]\n"),
    /Cargo\.toml has no \[package\] version/u,
  );
});

test("a release needs an explicit version, a dry run falls back to the manifest", () => {
  assert.equal(resolveVersion("0.2.0", "0.1.0", false), "0.2.0");
  assert.equal(resolveVersion("", "0.1.0", true), "0.1.0");
  assert.throws(() => resolveVersion("", "0.1.0", false), /a release needs the version input/u);
  for (const input of ["v0.2.0", "0.2", "0.2.0; rm -rf /", "0.2.0-rc.1"]) {
    assert.throws(() => resolveVersion(input, "0.1.0", true), /must be MAJOR\.MINOR\.PATCH/u);
  }
});

test("a correct release context has no problem", () => {
  assert.deepEqual(releaseContextProblems(facts()), []);
});

test("each wrong fact gives one problem with the fix", () => {
  const cases = [
    [{ ref: "refs/heads/feat/x" }, "The release must run on main, not on refs/heads/feat/x"],
    [
      { subject: "chore: release v0.2.0 (#70)" },
      "The HEAD commit subject is 'chore: release v0.2.0 (#70)', expected 'chore: release v0.2.0'",
    ],
    [{ originMain: OTHER }, `origin/main is ${OTHER}, not the release commit ${HEAD}`],
    [{ manifestVersion: "0.1.0" }, "The version 0.2.0 is not the Cargo.toml version 0.1.0"],
    [{ tagExists: true }, "The tag v0.2.0 already exists on the remote"],
  ];
  for (const [overrides, expected] of cases) {
    const problems = releaseContextProblems(facts(overrides));
    assert.equal(problems.length, 1, JSON.stringify(overrides));
    assert.ok(problems[0].startsWith(expected), problems[0]);
  }
});

test("the facts come from git and the manifest", () => {
  const calls = [];
  const run = (args) => {
    calls.push(args.join(" "));
    const answers = {
      "rev-parse HEAD": { status: 0, stdout: `${HEAD}\n` },
      "log -1 --format=%s": { status: 0, stdout: "chore: release v0.2.0\n" },
      "ls-remote --exit-code origin refs/heads/main": {
        status: 0,
        stdout: `${HEAD}\trefs/heads/main\n`,
      },
      "ls-remote --exit-code --tags origin refs/tags/v0.2.0": { status: 2, stdout: "" },
    };
    return answers[args.join(" ")];
  };
  assert.deepEqual(
    gatherFacts({ input: "0.2.0", dryRun: false, ref: "refs/heads/main", manifest: MANIFEST, run }),
    facts(),
  );
  assert.ok(calls.includes("ls-remote --exit-code --tags origin refs/tags/v0.2.0"));
});

test("a tag lookup that fails for another reason is an error, not an absent tag", () => {
  const run = (args) =>
    args.includes("--tags") ? { status: 128, stdout: "" } : { status: 0, stdout: `${HEAD}\n` };
  assert.throws(
    () =>
      gatherFacts({
        input: "0.2.0",
        dryRun: false,
        ref: "refs/heads/main",
        manifest: MANIFEST,
        run,
      }),
    /could not read the tag v0\.2\.0 from origin \(git exit 128\)/u,
  );
});

test("a release fails on a problem and a dry run only warns", () => {
  const lines = [];
  const log = (line) => lines.push(line);
  const bad = facts({ tagExists: true });
  assert.equal(verifyReleaseContext({ facts: bad, dryRun: false, log }), 1);
  assert.match(lines.at(-1), /^::error::The tag v0\.2\.0 already exists/u);
  assert.equal(verifyReleaseContext({ facts: bad, dryRun: true, log }), 0);
  assert.match(lines.at(-1), /^::warning::The tag v0\.2\.0 already exists/u);
  assert.equal(verifyReleaseContext({ facts: facts(), dryRun: false, log }), 0);
  assert.match(lines.at(-1), /release context of v0\.2\.0 is correct/u);
});

test("the arguments take a version and the dry-run switch", () => {
  assert.deepEqual(parseArguments(["--version", "0.2.0", "--dry-run", "false"]), {
    input: "0.2.0",
    dryRun: false,
  });
  assert.deepEqual(parseArguments(["--version", "", "--dry-run", "true"]), {
    input: "",
    dryRun: true,
  });
  assert.throws(() => parseArguments(["--version", "0.2.0"]), /--dry-run must be true or false/u);
});
