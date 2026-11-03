#!/usr/bin/env node

// Check the file list of the crate package. Cargo prints the list, and this
// script reads it from standard input:
//
//   cargo package --list --locked | node scripts/check-package.mjs
//
// The package may hold only the source, the manifest, the lock file, the
// license, the README and the CHANGELOG. Test fixtures, corpus data, the fuzz
// crate and repository tooling stay out. Each file must be smaller than
// MAX_FILE_BYTES. See docs/release-procedure.md.

import { readFileSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** The largest file that the package may hold. The largest source file is about 72 KiB. */
export const MAX_FILE_BYTES = 128 * 1024;

// Cargo writes these files into the package. They are not in the repository.
const GENERATED = new Set([".cargo_vcs_info.json", "Cargo.toml.orig"]);

const TOP_LEVEL = new Set(["Cargo.toml", "Cargo.lock", "LICENSE", "README.md", "CHANGELOG.md"]);

/** The files that each package must hold. */
export const REQUIRED = [...TOP_LEVEL, "src/main.rs", "src/lib.rs"];

const SOURCE = /^src\/(?:[a-z0-9_]+\/)*[a-z0-9_]+\.rs$/u;

/** Returns true when the package may hold `path`. */
export const isAllowed = (path) => GENERATED.has(path) || TOP_LEVEL.has(path) || SOURCE.test(path);

/**
 * Returns one problem line for each file that the package must not hold, each
 * required file that it does not hold, and each file over MAX_FILE_BYTES.
 * `sizeOf` returns the byte size of a repository path.
 */
export const checkPackage = (paths, sizeOf) => {
  const problems = [];
  const listed = new Set(paths);
  for (const path of paths) {
    if (!isAllowed(path)) {
      problems.push(`not allowed in the package: ${path}`);
      continue;
    }
    if (GENERATED.has(path)) {
      continue;
    }
    const size = sizeOf(path);
    if (size > MAX_FILE_BYTES) {
      problems.push(`larger than ${MAX_FILE_BYTES} bytes: ${path} (${size} bytes)`);
    }
  }
  for (const path of REQUIRED) {
    if (!listed.has(path)) {
      problems.push(`missing from the package: ${path}`);
    }
  }
  return problems;
};

/** Splits the output of `cargo package --list` into paths. */
export const parseList = (text) =>
  text
    .split(/\r?\n/u)
    .map((line) => line.trim().replaceAll("\\", "/"))
    .filter((line) => line !== "");

const main = () => {
  const paths = parseList(readFileSync(0, "utf8"));
  if (paths.length === 0) {
    console.error("check-package: the package list on standard input is empty.");
    return 2;
  }
  const problems = checkPackage(paths, (path) => statSync(path).size);
  if (problems.length > 0) {
    console.error(
      [
        `check-package: ${problems.length} problem(s) in the package list.`,
        ...problems.map((line) => `  ${line}`),
        "Change `include` in Cargo.toml, or move the file out of the package.",
      ].join("\n"),
    );
    return 1;
  }
  console.log(`check-package: ${paths.length} files, all allowed.`);
  return 0;
};

const invokedPath =
  process.argv[1] === undefined ? null : pathToFileURL(resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  process.exitCode = main();
}
