// Policy tests for .github/workflows/release.yml, release-published.yml and
// the action pins of every workflow. The release workflow must not publish on
// a manual run that keeps the dry-run default. It never creates the tag or the
// GitHub release: the owner does that last, with
// scripts/publish-github-release.mjs.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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

const PUBLISH_EXPRESSION =
  "${{ github.event_name == 'workflow_dispatch' && inputs.dry-run == false }}";
const RELEASE_IF = "    if: ${{ needs.plan.outputs.publish == 'true' }}";
const PUBLISHED = read(".github/workflows/release-published.yml");

test("the release starts only by hand, never on a push or a tag", () => {
  const triggers = block(RELEASE, "on", 0);
  assert.deepEqual(
    Array.from(triggers.matchAll(/^ {2}([a-z_]+):/gmu), (match) => match[1]),
    ["workflow_dispatch"],
  );
  assert.doesNotMatch(triggers, /push|tags|branches|paths|schedule|pull_request|workflow_run/u);
});

test("a manual run takes the version and is a dry run by default", () => {
  const dryRun = block(RELEASE, "dry-run", 6);
  assert.match(dryRun, /^ {8}default: true$/mu);
  assert.match(dryRun, /^ {8}type: boolean$/mu);
  const version = block(RELEASE, "version", 6);
  assert.match(version, /^ {8}type: string$/mu);
  assert.match(version, /^ {8}required: false$/mu);
});

test("only a manual run with dry-run set to false can publish", () => {
  assert.equal(RELEASE.match(/^ {2}PUBLISH: (.*)$/mu)?.[1], PUBLISH_EXPRESSION);
  assert.match(job("plan"), /echo "publish=\$\{PUBLISH\}" >> "\$GITHUB_OUTPUT"/u);
});

test("only a release joins the release concurrency group", () => {
  const expected =
    "${{ (github.event_name == 'workflow_dispatch' && inputs.dry-run == false) && 'release' || format('release-dry-run-{0}', github.run_id) }}";
  assert.equal(block(RELEASE, "concurrency", 0).match(/^ {2}group: (.*)$/mu)?.[1], expected);
  assert.match(block(RELEASE, "concurrency", 0), /^ {2}cancel-in-progress: false$/mu);
});

test("no job or step sets PUBLISH again", () => {
  assert.equal(RELEASE.match(/^\s+PUBLISH:/gmu)?.length, 1);
});

test("the plan job checks the release context and the metadata before each build", () => {
  const plan = job("plan");
  assert.match(
    plan,
    /node scripts\/verify-release-context\.mjs --version "\$VERSION_INPUT" --dry-run "\$DRY_RUN"/u,
  );
  assert.match(plan, /VERSION_INPUT: \$\{\{ inputs\.version \}\}/u);
  assert.match(plan, /DRY_RUN: \$\{\{ inputs\.dry-run \}\}/u);
  assert.match(plan, /node scripts\/verify-release-metadata\.mjs --tag "v\$\{VERSION\}"/u);
  assert.match(plan, /\.publish'\)"\n\s+if \[ "\$publish_field" != "null" \]/u);
  // An input reaches a script only through an environment variable.
  for (const line of RELEASE.split("\n").filter((text) => /\$\{\{ inputs\./u.test(text))) {
    assert.match(line, /^\s*[A-Za-z_-]+: /u, line);
    assert.doesNotMatch(line, /^\s*run: /u, line);
  }
});

test("the jobs run in the release order", () => {
  const expected = {
    plan: null,
    build: "plan",
    attest: "[plan, build]",
    sign: "[plan, build]",
    "publish-crate": "[plan, build, attest, sign]",
    "release-assets": "[plan, publish-crate]",
    "release-ready": "[plan, release-assets]",
  };
  assert.deepEqual(jobNames(RELEASE), Object.keys(expected));
  for (const [name, needs] of Object.entries(expected)) {
    const actual = job(name).match(/^ {4}needs: (.*)$/mu)?.[1] ?? null;
    assert.equal(actual, needs, name);
  }
  assert.match(job("release-ready"), /^ {4}name: Release ready for signed tag$/mu);
});

test("no job creates a tag or a release", () => {
  const scripts = RELEASE.split("\n").filter((line) => !line.trimStart().startsWith("#"));
  for (const line of scripts) {
    assert.doesNotMatch(
      line,
      /gh release (?:create|upload|edit)|git tag|git push|git\/refs/u,
      line,
    );
  }
  assert.doesNotMatch(RELEASE, /contents: write/u);
});

test("release-assets uploads the complete flat asset set as one artifact", () => {
  const assets = job("release-assets");
  assert.ok(assets.split("\n").includes(RELEASE_IF));
  assert.match(assets, /pattern: binary-\*/u);
  assert.match(assets, /name: signatures/u);
  assert.match(assets, /cat \.\/\*\.sha256 > SHA256SUMS/u);
  assert.match(assets, /name: release-assets\n\s+path: release-assets\//u);
  assert.match(assets, /if-no-files-found: error/u);
  assert.match(assets, /expected=\$\(\( 5 \* 3 \+ 1 \)\)/u);
});

test("release-assets and release-ready require the tag to stay absent", () => {
  for (const name of ["release-assets", "release-ready"]) {
    const text = job(name);
    assert.match(
      text,
      /git ls-remote --exit-code --tags "https:\/\/github\.com\/\$\{GITHUB_REPOSITORY\}\.git" "refs\/tags\/\$\{TAG_NAME\}"/u,
      name,
    );
    assert.match(text, /if \[ "\$status" -eq 0 \]; then/u, name);
    assert.match(text, /if \[ "\$status" -ne 2 \]; then/u, name);
  }
});

/** Returns the `run: |` script of the step `name` in a job, without its indent. */
const stepScript = (jobText, name) => {
  const lines = jobText.split("\n");
  const start = lines.indexOf(`      - name: ${name}`);
  assert.notEqual(start, -1, `missing step ${name}`);
  const run = lines.indexOf("        run: |", start);
  assert.notEqual(run, -1, `step ${name} has no run block`);
  const body = [];
  for (const line of lines.slice(run + 1)) {
    if (line.trim() !== "" && !line.startsWith(" ".repeat(10))) {
      break;
    }
    body.push(line.slice(10));
  }
  return body.join("\n");
};

// GitHub runs a `shell: bash` step as `bash --noprofile --norc -eo pipefail`.
// The step must survive the exit code 2 of `git ls-remote --exit-code` for an
// absent tag, and fail for a tag that exists or a lookup error.
test("the tag check passes only for an absent tag, under the bash flags of GitHub", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "release-tag-check-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, "git"), '#!/bin/sh\nexit "$STUB_GIT_STATUS"\n');
  chmodSync(join(dir, "git"), 0o755);
  for (const name of ["release-assets", "release-ready"]) {
    const script = stepScript(job(name), "Require the release tag to stay absent");
    for (const [gitStatus, expected] of [
      ["2", 0],
      ["0", 1],
      ["128", 1],
    ]) {
      const result = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", "-c", script], {
        encoding: "utf8",
        env: {
          PATH: `${dir}:${process.env.PATH}`,
          STUB_GIT_STATUS: gitStatus,
          TAG_NAME: "v0.2.0",
          VERSION: "0.2.0",
          GITHUB_REPOSITORY: "fallow-rs/cargo-fallow",
          GITHUB_RUN_ID: "1",
          GITHUB_STEP_SUMMARY: join(dir, "summary.md"),
        },
      });
      assert.equal(result.status, expected, `${name}, git exit ${gitStatus}: ${result.stdout}`);
    }
  }
});

test("the published release has its own metadata check", () => {
  const triggers = block(PUBLISHED, "on", 0);
  assert.match(triggers, /^ {2}release:\n {4}types: \[published\]$/mu);
  assert.doesNotMatch(triggers, /push|pull_request|workflow_dispatch/u);
  assert.match(PUBLISHED, /^permissions: \{\}$/mu);
  const check = block(PUBLISHED, "release-metadata", 2);
  assert.deepEqual(jobPermissions(check), ["contents: read"]);
  assert.doesNotMatch(check, /environment:/u);
  assert.match(
    check,
    /node scripts\/verify-release-metadata\.mjs \\\n\s+--tag "\$TAG_NAME" \\\n\s+--release-json release\.json/u,
  );
  assert.match(check, /TAG_NAME: \$\{\{ github\.event\.release\.tag_name \}\}/u);
});

test("the workflow denies all permissions by default", () => {
  assert.match(RELEASE, /^permissions: \{\}$/mu);
});

test("each job has exactly the permissions it needs", () => {
  const expected = {
    plan: ["contents: read"],
    build: ["contents: read"],
    attest: ["attestations: write", "contents: read", "id-token: write"],
    sign: ["contents: read"],
    "publish-crate": ["contents: read", "id-token: write"],
    "release-assets": ["contents: read"],
    "release-ready": ["contents: read"],
  };
  assert.deepEqual(jobNames(RELEASE).toSorted(), Object.keys(expected).toSorted());
  for (const [name, permissions] of Object.entries(expected)) {
    assert.deepEqual(jobPermissions(job(name)), permissions, name);
  }
});

test("each job that holds a credential runs only for a release, in the release environment", () => {
  for (const name of jobNames(RELEASE)) {
    const text = job(name);
    const privileged =
      /^ {6}(?:id-token|attestations|contents): write$/mu.test(text) ||
      /secrets\./u.test(text) ||
      /CARGO_REGISTRY_TOKEN/u.test(text);
    const inEnvironment = /^ {4}environment: release$/mu.test(text);
    assert.equal(inEnvironment, privileged, `${name}: privileged=${privileged}`);
  }
});

test("each job after the build runs only for a release", () => {
  for (const name of ["attest", "sign", "publish-crate", "release-assets", "release-ready"]) {
    assert.ok(job(name).split("\n").includes(RELEASE_IF), name);
  }
  for (const name of ["plan", "build"]) {
    assert.doesNotMatch(job(name), /^ {4}if:/mu, name);
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

test("a release signs each archive and checks the signature with the action key", () => {
  const sign = job("sign");
  assert.match(sign, /^ {4}needs: \[plan, build\]$/mu);
  assert.match(
    sign,
    /ED25519_BINARY_SIGNING_PRIVATE_KEY: \$\{\{ secrets\.ED25519_BINARY_SIGNING_PRIVATE_KEY \}\}/u,
  );
  assert.match(sign, /node \.github\/scripts\/sign-binary\.mjs "\$archive" "\$\{archive\}\.sig"/u);
  assert.match(
    sign,
    /node action\/scripts\/verify-signature\.mjs "\$archive" "\$\{archive\}\.sig"/u,
  );
  assert.doesNotMatch(sign, /CARGO_FALLOW_SIGNING_PUBLIC_KEY/u);
  assert.doesNotMatch(RELEASE.replace(job("sign"), ""), /ED25519_BINARY_SIGNING_PRIVATE_KEY/u);
});
