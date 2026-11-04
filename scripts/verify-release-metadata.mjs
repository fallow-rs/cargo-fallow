#!/usr/bin/env node

// Checks the release metadata at two points. The release workflow does not
// create the GitHub release, so the title and the body do not exist while it
// runs.
//
//   1. At dispatch, `release.yml` checks the CHANGELOG section of the version.
//      The owner writes the release notes from that section.
//   2. On `release: published`, `release-published.yml` checks the title and
//      the body. They can still change with `gh release edit`.
//
// The publication check reads the default branch, which can move during a
// release. So it checks only the released section, which does not change.
// The checks of pending work run only at dispatch.
//
//   node scripts/verify-release-metadata.mjs --tag vX.Y.Z [--release-json <file>]

import assert from "node:assert/strict";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { FRAGMENT_DIR, fragmentNames } from "./changelog-fragments.mjs";

const EM_DASH = String.fromCodePoint(0x2014);
const REPOSITORY_URL = "https://github.com/fallow-rs/cargo-fallow";
const RELEASE_HEADING = /^## \[(?<version>[^\]]+)\](?: - (?<date>\S+))?\s*$/u;
const CODE_FENCE = /^\s*(?:```|~~~)/u;
const SUBHEADING = /^### (?<name>.+?)\s*$/u;
const SEMVER = /^(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)$/u;
const ISO_DATE = /^(?<year>\d{4})-(?<month>\d{2})-(?<day>\d{2})$/u;

/** Returns `0.2.0` for `v0.2.0`, and rejects each other shape. */
export const versionOfTag = (tag) => {
  assert.match(tag ?? "", /^v\d+\.\d+\.\d+$/u, `release tag must match vMAJOR.MINOR.PATCH: ${tag}`);
  return tag.slice(1);
};

/**
 * Returns the URL that the release notes end with: the compare URL to the
 * previous tag, or the commit list of a first release.
 */
export const compareUrlOf = (version, previous) =>
  previous === null
    ? `${REPOSITORY_URL}/commits/v${version}`
    : `${REPOSITORY_URL}/compare/v${previous}...v${version}`;

/** A real calendar date, so a `2026-31-12` with the correct shape fails. */
const isCalendarDate = (value) => {
  const parts = value?.match(ISO_DATE)?.groups;
  if (!parts) {
    return false;
  }
  const year = Number(parts.year);
  const month = Number(parts.month);
  const day = Number(parts.day);
  const date = new Date(Date.UTC(year, month - 1, day));
  return (
    date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day
  );
};

const versionOrder = (version) => {
  const { major, minor, patch } = version.match(SEMVER).groups;
  return [Number(major), Number(minor), Number(patch)];
};

const isNewerVersion = (candidate, reference) => {
  const left = versionOrder(candidate);
  const right = versionOrder(reference);
  const differing = left.findIndex((value, index) => value !== right[index]);
  return differing !== -1 && left[differing] > right[differing];
};

/**
 * Returns each `## [...]` section in document order, with the body up to the
 * next heading. A heading in a fenced block does not start a section.
 */
export const changelogSections = (changelog) => {
  const lines = changelog.split(/\r?\n/u);
  const starts = [];
  let fenced = false;
  lines.forEach((line, index) => {
    if (CODE_FENCE.test(line)) {
      fenced = !fenced;
      return;
    }
    const match = fenced ? null : line.match(RELEASE_HEADING);
    if (match) {
      starts.push({ index, version: match.groups.version, date: match.groups.date });
    }
  });
  return starts.map((start, position) => ({
    version: start.version,
    date: start.date,
    line: start.index + 1,
    body: lines.slice(start.index + 1, starts[position + 1]?.index ?? lines.length).join("\n"),
  }));
};

/**
 * Returns each `###` heading that occurs more than one time in a section, with
 * the 1-based line number of each occurrence.
 */
export const repeatedSubheadings = (section) => {
  const occurrences = new Map();
  let fenced = false;
  section.body.split(/\r?\n/u).forEach((line, index) => {
    if (CODE_FENCE.test(line)) {
      fenced = !fenced;
      return;
    }
    const name = fenced ? undefined : line.match(SUBHEADING)?.groups.name;
    if (name !== undefined) {
      occurrences.set(name, [...(occurrences.get(name) ?? []), section.line + 1 + index]);
    }
  });
  return Array.from(occurrences, ([name, lines]) => ({ name, lines })).filter(
    (heading) => heading.lines.length > 1,
  );
};

const hasContent = (body) => body.split(/\r?\n/u).some((line) => line.trim() !== "");

/**
 * Returns the released version directly below `version`, or null for the first
 * release. The sections must descend, so the compare URL never goes back.
 */
export const previousReleasedVersion = (sections, version) => {
  const released = sections.filter((section) => SEMVER.test(section.version));
  const position = released.findIndex((section) => section.version === version);
  assert.notEqual(position, -1, `CHANGELOG.md has no released section for ${version}`);
  const previous = released[position + 1];
  if (previous === undefined) {
    return null;
  }
  assert.ok(
    isNewerVersion(version, previous.version),
    `CHANGELOG.md: ${version} is above ${previous.version}, so the sections are out of order`,
  );
  return previous.version;
};

/**
 * Checks the released section that the notes come from, and returns the URL
 * that the published body must hold. Each rule reads a section that does not
 * change after the release.
 */
export const assertChangelogRelease = (changelog, tag) => {
  const version = versionOfTag(tag);
  const sections = changelogSections(changelog);
  const matching = sections.filter((section) => section.version === version);
  assert.equal(
    matching.length,
    1,
    `CHANGELOG.md must hold exactly one "## [${version}]" section, found ${matching.length}`,
  );
  const [section] = matching;
  assert.ok(
    isCalendarDate(section.date),
    `CHANGELOG.md: "## [${version}]" needs a real YYYY-MM-DD date, found ${section.date ?? "none"}`,
  );
  assert.ok(hasContent(section.body), `CHANGELOG.md: the ${version} section is empty`);
  assert.ok(
    !section.body.includes(EM_DASH),
    `CHANGELOG.md: the ${version} section contains an em dash, which the release notes inherit`,
  );
  const repeated = repeatedSubheadings(section);
  if (repeated.length > 0) {
    const where = repeated.map(
      ({ name, lines }) =>
        `"### ${name}" on lines ${lines.slice(0, -1).join(", ")} and ${lines.at(-1)}`,
    );
    assert.fail(
      `CHANGELOG.md: the ${version} section repeats ${where.join("; ")}. Merge each into one heading.`,
    );
  }
  const previous = previousReleasedVersion(sections, version);
  return {
    version,
    previousTag: previous === null ? null : `v${previous}`,
    compareUrl: compareUrlOf(version, previous),
  };
};

/**
 * Each pending entry must be in this release. This check runs only at
 * dispatch: `[Unreleased]` fills again when the next change merges.
 */
export const assertNothingUnreleased = (changelog) => {
  const unreleased = changelogSections(changelog).filter(
    (section) => section.version === "Unreleased",
  );
  assert.equal(
    unreleased.length,
    1,
    `CHANGELOG.md must hold exactly one "## [Unreleased]" section, found ${unreleased.length}`,
  );
  assert.ok(
    !hasContent(unreleased[0].body),
    "CHANGELOG.md: the Unreleased section still holds entries. Move them into the release section.",
  );
};

/** A fragment that is left at dispatch is not in the release section. */
export const assertNoFragments = (root) => {
  const names = fragmentNames(resolve(root, FRAGMENT_DIR));
  assert.deepEqual(
    names,
    [],
    `${FRAGMENT_DIR}/ still holds fragments. Run "node scripts/changelog-fragments.mjs collect" in the release commit.`,
  );
};

/** Checks a `gh release view --json name,body,isDraft,isPrerelease` document. */
export const assertPublishedRelease = (release, tag, compareUrl) => {
  versionOfTag(tag);
  assert.equal(release.isDraft, false, `${tag} is still a draft`);
  assert.equal(release.isPrerelease, false, `${tag} is marked as a prerelease`);
  const title = release.name ?? "";
  const prefix = `${tag}: `;
  assert.ok(
    title.startsWith(prefix),
    `release title must start with "${prefix}", found "${title}"`,
  );
  assert.ok(
    title.slice(prefix.length).trim() !== "",
    `release title has no summary after "${prefix}"`,
  );
  const body = release.body ?? "";
  assert.ok(body.trim() !== "", `release ${tag} has an empty body`);
  assert.ok(
    body.includes(compareUrl),
    `release ${tag} body is missing the compare URL ${compareUrl}`,
  );
  assert.ok(!title.includes(EM_DASH), "release title contains an em dash");
  assert.ok(!body.includes(EM_DASH), "release body contains an em dash");
};

export const parseArguments = (argv) => {
  const options = { tag: undefined, releaseJson: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--tag") {
      index += 1;
      options.tag = argv[index];
    } else if (flag === "--release-json") {
      index += 1;
      options.releaseJson = argv[index];
    } else {
      assert.fail(`unknown argument: ${flag}`);
    }
  }
  assert.ok(
    options.tag,
    "usage: node scripts/verify-release-metadata.mjs --tag vX.Y.Z [--release-json <file>]",
  );
  return options;
};

/** Runs the dispatch check, or the publication check with `releaseJson`. */
export const verifyReleaseMetadata = ({
  tag,
  releaseJson,
  root = resolve(dirname(fileURLToPath(import.meta.url)), ".."),
}) => {
  const changelog = readFileSync(resolve(root, "CHANGELOG.md"), "utf8");
  const { version, previousTag, compareUrl } = assertChangelogRelease(changelog, tag);
  const since = previousTag === null ? "first release" : `compares back to ${previousTag}`;
  const surfaces = [`CHANGELOG.md section ${version} (${since})`];
  if (releaseJson === undefined) {
    assertNothingUnreleased(changelog);
    surfaces.push("an empty CHANGELOG.md [Unreleased] section");
    assertNoFragments(root);
    surfaces.push(`no fragments in ${FRAGMENT_DIR}/`);
  } else {
    assertPublishedRelease(JSON.parse(readFileSync(releaseJson, "utf8")), tag, compareUrl);
    surfaces.push("published GitHub release title and body");
  }
  return surfaces;
};

// `process.argv[1]` keeps symlinks, and Node resolves the ESM entry to its
// real path. A value that does not resolve means that a module imported this
// file.
const realPathOrNull = (value) => {
  try {
    return value === undefined ? null : realpathSync(value);
  } catch {
    return null;
  }
};

if (realPathOrNull(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  try {
    for (const surface of verifyReleaseMetadata(parseArguments(process.argv.slice(2)))) {
      console.log(`Verified ${surface}.`);
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
