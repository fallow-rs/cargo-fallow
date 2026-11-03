// Policy tests for .github/workflows/release.yml and the action pins of every
// workflow. The release workflow must not publish on a branch, on a pull
// request or on a manual run that keeps the dry-run default.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const ROOT = new URL("../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const RELEASE = read(".github/workflows/release.yml");

const isIgnoredLine = (line) => line.trim() === "" || line.trimStart().startsWith("#");
const indentationOf = (line) => line.length - line.trimStart().length;

/** Returns the lines of the block that starts at `key:` with `indent` spaces. */
const block = (source, key, indent) => {
  const lines = source.split(/\r?\n/u);
  const start = lines.findIndex((line) => line === `${" ".repeat(indent)}${key}:`);
  assert.notEqual(start, -1, `missing ${key} block`);
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => !isIgnoredLine(line) && indentationOf(line) <= indent);
  return lines.slice(start, end === -1 ? lines.length : start + 1 + end).join("\n");
};

/** Returns the job names under `jobs:`. */
const jobNames = (source) =>
  Array.from(block(source, "jobs", 0).matchAll(/^ {2}([a-z][a-z0-9-]*):$/gmu), (match) => match[1]);

/** Returns the `permissions` of a job as a sorted `scope: level` list. */
const jobPermissions = (job) => {
  const lines = job.split("\n");
  const start = lines.findIndex((line) => line === "    permissions:");
  assert.notEqual(start, -1, "each job declares its permissions");
  const result = [];
  for (const line of lines.slice(start + 1)) {
    const match = line.match(/^ {6}([a-z-]+): ([a-z]+)$/u);
    if (match === null) {
      break;
    }
    result.push(`${match[1]}: ${match[2]}`);
  }
  return result.toSorted();
};

const job = (name) => block(RELEASE, name, 2);

test("the release runs only on a v* tag push or a manual run", () => {
  const triggers = block(RELEASE, "on", 0);
  assert.deepEqual(
    Array.from(triggers.matchAll(/^ {2}([a-z_]+):/gmu), (match) => match[1]),
    ["push", "workflow_dispatch"],
  );
  assert.match(triggers, /^ {4}tags: \['v\*'\]$/mu);
  assert.doesNotMatch(triggers, /branches|paths|schedule|pull_request|workflow_run/u);
});

test("a manual run is a dry run by default", () => {
  const input = block(RELEASE, "dry-run", 6);
  assert.match(input, /^ {8}default: true$/mu);
  assert.match(input, /^ {8}type: boolean$/mu);
});

test("only a tag ref can publish", () => {
  const expected =
    "${{ github.ref_type == 'tag' && (github.event_name == 'push' || inputs.dry-run == false) }}";
  assert.equal(RELEASE.match(/^ {2}PUBLISH: (.*)$/mu)?.[1], expected);
  assert.match(job("plan"), /echo "publish=\$\{PUBLISH\}" >> "\$GITHUB_OUTPUT"/u);
});

test("only a release joins the release concurrency group", () => {
  const expected =
    "${{ (github.ref_type == 'tag' && (github.event_name == 'push' || inputs.dry-run == false)) && 'release' || format('release-dry-run-{0}', github.run_id) }}";
  assert.equal(block(RELEASE, "concurrency", 0).match(/^ {2}group: (.*)$/mu)?.[1], expected);
  assert.match(block(RELEASE, "concurrency", 0), /^ {2}cancel-in-progress: false$/mu);
});

test("no job or step sets PUBLISH again", () => {
  assert.equal(RELEASE.match(/^\s+PUBLISH:/gmu)?.length, 1);
});

test("a release checks the tag, the manifest and the branch before it publishes", () => {
  const plan = job("plan");
  assert.match(plan, /if \[ "\$TAG_NAME" != "v\$\{VERSION\}" \]/u);
  assert.match(plan, /\.publish'\)"\n\s+if \[ "\$publish_field" != "null" \]/u);
  assert.match(plan, /git merge-base --is-ancestor "\$GITHUB_SHA" origin\/main/u);
  assert.match(plan, /node scripts\/release-notes\.mjs "\$VERSION" > notes/u);
  assert.match(job("publish-crate"), /^ {4}needs: \[plan, build, attest\]$/mu);
  assert.match(job("github-release"), /^ {4}needs: \[plan, attest, publish-crate\]$/mu);
  assert.match(job("attest"), /^ {4}needs: \[plan, build\]$/mu);
});

test("the workflow denies all permissions by default", () => {
  assert.match(RELEASE, /^permissions: \{\}$/mu);
});

test("each job has exactly the permissions it needs", () => {
  const expected = {
    plan: ["contents: read"],
    build: ["contents: read"],
    attest: ["attestations: write", "contents: read", "id-token: write"],
    "publish-crate": ["contents: read", "id-token: write"],
    "github-release": ["contents: write"],
  };
  assert.deepEqual(jobNames(RELEASE).toSorted(), Object.keys(expected).toSorted());
  for (const [name, permissions] of Object.entries(expected)) {
    assert.deepEqual(jobPermissions(job(name)), permissions, name);
  }
});

test("each job that can write or holds a token runs only for a release, in the release environment", () => {
  for (const name of jobNames(RELEASE)) {
    const text = job(name);
    const privileged =
      /^ {6}(?:id-token|attestations|contents): write$/mu.test(text) ||
      /secrets\.(?!GITHUB_TOKEN\b)/u.test(text) ||
      /CARGO_REGISTRY_TOKEN/u.test(text) ||
      /gh release/u.test(text);
    const gated =
      /^ {4}environment: release$/mu.test(text) &&
      /^ {4}if: \$\{\{ needs\.plan\.outputs\.publish == 'true' \}\}$/mu.test(text);
    assert.equal(gated, privileged, `${name}: privileged=${privileged} gated=${gated}`);
  }
});

test("only the publish job runs a real cargo publish", () => {
  for (const name of jobNames(RELEASE)) {
    // Join continued lines, so a flag on the next line still counts.
    const real = job(name)
      .replaceAll(/\\\n\s*/gu, " ")
      .split("\n")
      .filter((line) => /\bcargo publish\b/u.test(line) && !line.trimStart().startsWith("#"))
      .filter((line) => !line.includes("--dry-run"));
    assert.equal(real.length > 0, name === "publish-crate", name);
  }
  assert.match(job("publish-crate"), /cargo publish --locked --no-verify/u);
  assert.match(job("plan"), /cargo publish --dry-run --locked --allow-dirty/u);
});

test("only the dry run removes publish = false, and it restores the manifest", () => {
  const plan = job("plan");
  assert.match(plan, /sed -i '\/\^publish = false\$\/d' Cargo\.toml/u);
  assert.match(plan, /git checkout -- Cargo\.toml/u);
  const others = jobNames(RELEASE).filter((name) => name !== "plan");
  for (const name of others) {
    assert.doesNotMatch(job(name), /publish = false/u, name);
  }
  assert.match(plan, /Cargo\.toml still has 'publish = false'/u);
});

test("each checkout keeps no credentials", () => {
  const checkouts = RELEASE.split("\n").filter((line) => line.includes("actions/checkout@"));
  const persisted = RELEASE.match(/persist-credentials: false/gu) ?? [];
  assert.notEqual(checkouts.length, 0);
  assert.equal(persisted.length, checkouts.length);
});

test("each action in each workflow is pinned to a commit", () => {
  const files = [
    ...readdirSync(new URL(".github/workflows/", ROOT)).map((name) => `.github/workflows/${name}`),
    ...readdirSync(new URL(".github/actions/", ROOT)).map(
      (name) => `.github/actions/${name}/action.yml`,
    ),
  ].filter((path) => /\.ya?ml$/u.test(path));
  assert.ok(files.includes(".github/workflows/release.yml"));
  for (const path of files) {
    for (const match of read(path).matchAll(/^\s*-?\s*uses:\s*(\S+)/gmu)) {
      const reference = match[1];
      const local = reference.startsWith("./");
      const pinned = /^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/u.test(reference);
      assert.ok(local || pinned, `${path}: ${reference} is not pinned to a commit`);
    }
  }
});
