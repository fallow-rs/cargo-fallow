import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  assertChangelogRelease,
  assertNoFragments,
  assertNothingUnreleased,
  assertPublishedRelease,
  compareUrlOf,
  parseArguments,
  verifyReleaseMetadata,
  versionOfTag,
} from "./verify-release-metadata.mjs";

const EM_DASH = String.fromCodePoint(0x2014);
const COMPARE = "https://github.com/fallow-rs/cargo-fallow/compare/v0.1.0...v0.2.0";
const FIRST = "https://github.com/fallow-rs/cargo-fallow/commits/v0.1.0";

const changelog = ({
  unreleased = "",
  release = "### Added\n\n- New.\n",
  date = "2026-12-01",
} = {}) =>
  [
    "# Changelog",
    "",
    "## [Unreleased]",
    "",
    unreleased,
    `## [0.2.0] - ${date}`,
    "",
    release,
    "## [0.1.0] - 2026-11-04",
    "",
    "### Added",
    "",
    "- First release.",
    "",
  ].join("\n");

const fixture = (t, text) => {
  const root = mkdtempSync(join(tmpdir(), "release-metadata-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, "CHANGELOG.md"), text);
  mkdirSync(join(root, "changelog.d"));
  writeFileSync(join(root, "changelog.d", "README.md"), "# Fragments\n");
  return root;
};

const published = (overrides = {}) => ({
  name: "v0.2.0: Faster checks",
  body: `Notes.\n\n**Full Changelog**: ${COMPARE}\n`,
  isDraft: false,
  isPrerelease: false,
  ...overrides,
});

test("a tag must be vMAJOR.MINOR.PATCH", () => {
  assert.equal(versionOfTag("v0.2.0"), "0.2.0");
  for (const tag of ["0.2.0", "v0.2", "v0.2.0-rc.1", undefined]) {
    assert.throws(() => versionOfTag(tag), /release tag must match vMAJOR\.MINOR\.PATCH/u);
  }
});

test("the compare URL spans back to the previous release, or lists the commits of a first release", () => {
  assert.equal(compareUrlOf("0.2.0", "0.1.0"), COMPARE);
  assert.equal(compareUrlOf("0.1.0", null), FIRST);
});

test("a dated section with content gives the compare URL", () => {
  assert.deepEqual(assertChangelogRelease(changelog(), "v0.2.0"), {
    version: "0.2.0",
    previousTag: "v0.1.0",
    compareUrl: COMPARE,
  });
  assert.deepEqual(assertChangelogRelease(changelog(), "v0.1.0"), {
    version: "0.1.0",
    previousTag: null,
    compareUrl: FIRST,
  });
});

test("the release section must exist one time, with a real date and content", () => {
  assert.throws(
    () => assertChangelogRelease(changelog(), "v0.3.0"),
    /must hold exactly one "## \[0\.3\.0\]" section, found 0/u,
  );
  assert.throws(
    () => assertChangelogRelease(changelog({ date: "2026-31-12" }), "v0.2.0"),
    /needs a real YYYY-MM-DD date, found 2026-31-12/u,
  );
  assert.throws(
    () => assertChangelogRelease(changelog({ release: "" }), "v0.2.0"),
    /the 0\.2\.0 section is empty/u,
  );
  assert.throws(
    () => assertChangelogRelease(changelog({ release: `- A ${EM_DASH} B.\n` }), "v0.2.0"),
    /the 0\.2\.0 section contains an em dash/u,
  );
});

test("a repeated ### heading in the release section names each line", () => {
  const release = "### Fixed\n\n- A.\n\n### Fixed\n\n- B.\n";
  assert.throws(
    () => assertChangelogRelease(changelog({ release }), "v0.2.0"),
    /repeats "### Fixed" on lines 8 and 12/u,
  );
});

test("the sections must descend", () => {
  const text = changelog().replace("## [0.1.0] - 2026-11-04", "## [0.3.0] - 2026-11-04");
  assert.throws(() => assertChangelogRelease(text, "v0.2.0"), /out of order/u);
});

test("the Unreleased section must be empty at dispatch", () => {
  assertNothingUnreleased(changelog());
  assert.throws(
    () => assertNothingUnreleased(changelog({ unreleased: "### Added\n\n- Pending.\n" })),
    /the Unreleased section still holds entries/u,
  );
  assert.throws(
    () => assertNothingUnreleased("# Changelog\n\n## [0.1.0] - 2026-11-04\n\n- X.\n"),
    /exactly one "## \[Unreleased\]" section, found 0/u,
  );
});

test("a fragment left in changelog.d fails the dispatch", (t) => {
  const root = fixture(t, changelog());
  assertNoFragments(root);
  writeFileSync(join(root, "changelog.d", "feat-a.md"), "### Added\n\n- A.\n");
  assert.throws(() => assertNoFragments(root), /changelog\.d\/ still holds fragments/u);
});

test("the published release needs the title prefix, a summary, a body and the compare URL", () => {
  assertPublishedRelease(published(), "v0.2.0", COMPARE);
  const cases = [
    [{ isDraft: true }, /v0\.2\.0 is still a draft/u],
    [{ isPrerelease: true }, /marked as a prerelease/u],
    [{ name: "cargo-fallow v0.2.0" }, /title must start with "v0\.2\.0: "/u],
    [{ name: "v0.2.0:  " }, /no summary after "v0\.2\.0: "/u],
    [{ body: " \n" }, /empty body/u],
    [{ body: "Notes.\n" }, /missing the compare URL/u],
    [{ name: `v0.2.0: A ${EM_DASH} B` }, /title contains an em dash/u],
    [{ body: `A ${EM_DASH} B.\n\n${COMPARE}\n` }, /body contains an em dash/u],
  ];
  for (const [overrides, error] of cases) {
    assert.throws(() => assertPublishedRelease(published(overrides), "v0.2.0", COMPARE), error);
  }
});

test("the dispatch checks the changelog, Unreleased and the fragments", (t) => {
  const root = fixture(t, changelog());
  assert.deepEqual(verifyReleaseMetadata({ tag: "v0.2.0", root }), [
    "CHANGELOG.md section 0.2.0 (compares back to v0.1.0)",
    "an empty CHANGELOG.md [Unreleased] section",
    "no fragments in changelog.d/",
  ]);
});

test("the publication check reads the release JSON and skips the pending work", (t) => {
  const root = fixture(t, changelog({ unreleased: "### Added\n\n- Merged later.\n" }));
  writeFileSync(join(root, "changelog.d", "feat-b.md"), "### Added\n\n- B.\n");
  const releaseJson = join(root, "release.json");
  writeFileSync(releaseJson, JSON.stringify(published()));
  assert.deepEqual(verifyReleaseMetadata({ tag: "v0.2.0", releaseJson, root }), [
    "CHANGELOG.md section 0.2.0 (compares back to v0.1.0)",
    "published GitHub release title and body",
  ]);
});

test("the arguments need a tag", () => {
  assert.deepEqual(parseArguments(["--tag", "v0.2.0"]), {
    tag: "v0.2.0",
    releaseJson: undefined,
  });
  assert.throws(() => parseArguments([]), /usage: node scripts\/verify-release-metadata\.mjs/u);
  assert.throws(() => parseArguments(["--nope"]), /unknown argument: --nope/u);
});
