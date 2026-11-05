// The release skill repeats steps of the release contract. This test fails
// when a contract file changes and the pin in the skill stays the same, so a
// change to the release flow also updates the skill.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";

const ROOT = new URL("../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

const SKILL = ".agents/skills/cargo-fallow-release/SKILL.md";
const CONTRACT = [
  "docs/release-procedure.md",
  ".github/workflows/release.yml",
  "scripts/verify-release-context.mjs",
  "scripts/publish-github-release.mjs",
  "scripts/verify-release-ci.mjs",
  "scripts/verify-release-metadata.mjs",
  "scripts/changelog-fragments.mjs",
  "changelog.d/README.md",
];
// Dependabot moves the action pins of release.yml. A pin does not change the
// release steps, so the hash leaves out each `uses:` line.
const USES = /^\s*(- )?uses: .*$/gmu;
const PIN = /^release-contract: ([0-9a-f]{64})$/mu;

const contractHash = (readFile = read) => {
  const hash = createHash("sha256");
  for (const path of CONTRACT) hash.update(`${path}\n${readFile(path).replace(USES, "")}\n`);
  return hash.digest("hex");
};

test("the release skill pins the current release contract", () => {
  const pin = PIN.exec(read(SKILL))?.[1];
  const hash = contractHash();
  assert.ok(pin, `${SKILL} has no 'release-contract: <sha256>' line. The current hash is ${hash}.`);
  assert.equal(
    pin,
    hash,
    `A release contract file changed: ${CONTRACT.join(", ")}. ` +
      `Update the matching steps of ${SKILL}, then set its release-contract line to ${hash}.`,
  );
});

const NEW_SHA = "0".repeat(40);

test("a new action pin in release.yml keeps the hash", () => {
  const bumped = (path) =>
    path === ".github/workflows/release.yml"
      ? read(path).replace(
          /(uses: actions\/checkout@)[0-9a-f]{40}/u,
          (_, head) => `${head}${NEW_SHA}`,
        )
      : read(path);
  assert.notEqual(bumped(".github/workflows/release.yml"), read(".github/workflows/release.yml"));
  assert.equal(contractHash(bumped), contractHash());
});

test("a new release step in release.yml changes the hash", () => {
  const changed = (path) =>
    path === ".github/workflows/release.yml"
      ? `${read(path)}\n      - run: echo new step\n`
      : read(path);
  assert.notEqual(contractHash(changed), contractHash());
});
