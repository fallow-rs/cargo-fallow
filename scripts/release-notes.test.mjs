import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { releaseNotes, sectionOf } from "./release-notes.mjs";

const CHANGELOG = [
  "# Changelog",
  "",
  "## [Unreleased]",
  "",
  "- Next change.",
  "",
  "## [0.1.0] - 2026-12-01",
  "",
  "### Added",
  "",
  "- First release.",
  "",
  "## [0.0.9]",
  "",
  "- Old.",
  "",
].join("\n");

test("the section of a version ends before the next heading", () => {
  assert.equal(sectionOf(CHANGELOG, "0.1.0"), "### Added\n\n- First release.\n");
  assert.equal(sectionOf(CHANGELOG, "0.0.9"), "- Old.\n");
});

test("a version prefix does not match another version", () => {
  assert.equal(sectionOf(CHANGELOG, "0.1"), null);
});

test("a missing version is an error unless the unreleased section is allowed", () => {
  assert.deepEqual(releaseNotes(CHANGELOG, "0.2.0", false), {
    ok: false,
    error: "CHANGELOG.md has no section with content for 0.2.0.",
  });
  assert.deepEqual(releaseNotes(CHANGELOG, "0.2.0", true), { ok: true, notes: "- Next change.\n" });
  assert.deepEqual(releaseNotes(CHANGELOG, "0.1.0", true), {
    ok: true,
    notes: "### Added\n\n- First release.\n",
  });
});

test("an empty section is an error", () => {
  assert.equal(releaseNotes("## [1.0.0]\n\n## [0.9.0]\n- x\n", "1.0.0", false).ok, false);
});

test("the repository CHANGELOG has notes for a dry run of the manifest version", () => {
  const changelog = readFileSync(new URL("../CHANGELOG.md", import.meta.url), "utf8");
  const manifest = readFileSync(new URL("../Cargo.toml", import.meta.url), "utf8");
  const version = /^version = "([^"]+)"$/m.exec(manifest)?.[1];
  assert.ok(version, "Cargo.toml has a package version");
  assert.equal(releaseNotes(changelog, version, true).ok, true);
});
