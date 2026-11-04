#!/usr/bin/env node

// Print the CHANGELOG section of one version. The owner writes the release
// notes from it, and the release workflow shows it in the run summary:
//
//   node scripts/release-notes.mjs 0.1.0 [--allow-unreleased]
//
// A section starts at `## [0.1.0]` and ends before the next `## ` heading.
// `--allow-unreleased` prints the `## [Unreleased]` section when the version
// has no section. A dry run before the release commit uses it.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { runCliMain } from "./cli-main.mjs";

const USAGE = "Usage: node scripts/release-notes.mjs <version> [--allow-unreleased]";

/** Returns the body of the section `## [name]`, or null when there is none. */
export const sectionOf = (changelog, name) => {
  const lines = changelog.split(/\r?\n/u);
  const start = lines.findIndex(
    (line) => line === `## [${name}]` || line.startsWith(`## [${name}] `),
  );
  if (start === -1) {
    return null;
  }
  const relativeEnd = lines.slice(start + 1).findIndex((line) => line.startsWith("## "));
  const end = relativeEnd === -1 ? lines.length : start + 1 + relativeEnd;
  const body = lines
    .slice(start + 1, end)
    .join("\n")
    .trim();
  return body === "" ? null : `${body}\n`;
};

/** Returns the notes for `version`, or an error message. */
export const releaseNotes = (changelog, version, allowUnreleased) => {
  const notes = sectionOf(changelog, version);
  if (notes !== null) {
    return { ok: true, notes };
  }
  const unreleased = allowUnreleased ? sectionOf(changelog, "Unreleased") : null;
  if (unreleased !== null) {
    return { ok: true, notes: unreleased };
  }
  return { ok: false, error: `CHANGELOG.md has no section with content for ${version}.` };
};

const main = () => {
  const args = process.argv.slice(2);
  const allowUnreleased = args.includes("--allow-unreleased");
  const positional = args.filter((arg) => arg !== "--allow-unreleased");
  if (positional.length !== 1 || positional[0].startsWith("-")) {
    console.error(USAGE);
    return 2;
  }
  const result = releaseNotes(readFileSync("CHANGELOG.md", "utf8"), positional[0], allowUnreleased);
  if (!result.ok) {
    console.error(`release-notes: ${result.error}`);
    return 1;
  }
  process.stdout.write(result.notes);
  return 0;
};

const invokedPath =
  process.argv[1] === undefined ? null : pathToFileURL(resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  runCliMain(main);
}
