#!/usr/bin/env node

// Each pull request writes its changelog entries to its own file under
// changelog.d/. Two pull requests then never change the same lines, so a merge
// of one cannot make the other conflict. The release collects the fragments
// into the [Unreleased] section of CHANGELOG.md.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs as parseOptions } from "node:util";

import { runCliMain } from "./cli-main.mjs";

export const FRAGMENT_DIR = "changelog.d";
const CHANGELOG = "CHANGELOG.md";
const README = "README.md";
const EM_DASH = String.fromCodePoint(0x2014);

/** The `###` headings a fragment can use, in the order of a release section. */
export const KINDS = [
  "Breaking",
  "Added",
  "Changed",
  "Deprecated",
  "Removed",
  "Fixed",
  "Performance",
  "Security",
];

const USAGE = `Usage: node scripts/changelog-fragments.mjs <command> [options]

Commands:
  check [--base <ref>] [--pending-release]
                        Check each fragment in ${FRAGMENT_DIR}/. With --base,
                        also fail when the [Unreleased] section of ${CHANGELOG}
                        is not the same as at the fork point with <ref>: a pull
                        request adds a fragment and does not edit ${CHANGELOG}.
                        A new "## [X.Y.Z]" heading also fails, except in a
                        branch with the commit "chore: release vX.Y.Z".
                        --pending-release also accepts the heading of the
                        Cargo.toml version, for a commit that is not made yet.
  collect [--dry-run]   Move each fragment into the [Unreleased] section of
                        ${CHANGELOG} and delete the fragment files. --dry-run
                        prints the section and changes nothing.`;

const HEADING = /^### (?<name>.+?)\s*$/u;
const FENCE = /^\s*(```|~~~)/u;

/**
 * Splits markdown into the text before the first `###` heading and one block
 * for each heading. A `###` line in a fenced code block is body text.
 */
export const splitSections = (text) => {
  const blocks = [{ name: null, lines: [] }];
  let fenced = false;
  for (const line of text.split(/\r?\n/u)) {
    fenced = FENCE.test(line) !== fenced;
    const heading = fenced ? null : HEADING.exec(line);
    if (heading) {
      blocks.push({ name: heading.groups.name, lines: [] });
    } else {
      blocks.at(-1).lines.push(line);
    }
  }
  const [preamble, ...sections] = blocks.map(({ name, lines }) => ({
    name,
    body: lines.join("\n").trim(),
  }));
  return { preamble: preamble.body, sections };
};

const KIND_LIST = `use one of: ${KINDS.join(", ")}`;

const sectionProblems = (sections) =>
  sections.flatMap(({ name, body }, index) => [
    KINDS.includes(name) ? null : `has the unknown heading "### ${name}" (${KIND_LIST})`,
    sections.findIndex((other) => other.name === name) < index ? `repeats "### ${name}"` : null,
    body === "" ? `has an empty "### ${name}" section` : null,
  ]);

/** Returns the problems of one fragment. Each problem starts with its path. */
export const fragmentProblems = (path, text) => {
  const { preamble, sections } = splitSections(text);
  return [
    preamble === "" ? null : "has text before its first ### heading",
    sections.length === 0 ? `has no ### heading (${KIND_LIST})` : null,
    ...sectionProblems(sections),
    text.includes(EM_DASH) ? "contains an em dash, which the release notes inherit" : null,
  ]
    .filter((problem) => problem !== null)
    .map((problem) => `${path}: ${problem}`);
};

/** Returns the sorted fragment file names in `dir`, without the README. */
export const fragmentNames = (dir) =>
  existsSync(dir)
    ? readdirSync(dir)
        .filter((name) => name.endsWith(".md") && name !== README)
        .toSorted()
    : [];

const UNRELEASED = /^## \[Unreleased\][^\n]*\n/mu;
const NEXT_SECTION = /^## \[/mu;

/** Returns the `[Unreleased]` body and its offsets in the changelog. */
export const unreleasedSection = (changelog) => {
  const header = UNRELEASED.exec(changelog);
  if (!header) {
    throw new Error(`${CHANGELOG} has no "## [Unreleased]" section`);
  }
  const start = header.index + header[0].length;
  const next = NEXT_SECTION.exec(changelog.slice(start));
  const end = next ? start + next.index : changelog.length;
  return { start, end, body: changelog.slice(start, end) };
};

/**
 * Merges the existing `[Unreleased]` body and the fragments, in that order,
 * into one body with each heading one time. The known headings follow
 * `KINDS`. Other headings of the existing body come after them.
 */
export const mergeUnreleased = (existing, fragments) => {
  const merged = new Map();
  const add = ({ name, body }) => {
    if (body !== "") {
      merged.set(name, [...(merged.get(name) ?? []), body]);
    }
  };
  const current = splitSections(existing);
  if (current.preamble !== "") {
    throw new Error(`${CHANGELOG}: the [Unreleased] section has text before its first ### heading`);
  }
  current.sections.forEach(add);
  fragments.forEach((text) => splitSections(text).sections.forEach(add));

  const order = [
    ...KINDS.filter((name) => merged.has(name)),
    ...[...merged.keys()].filter((name) => !KINDS.includes(name)),
  ];
  return order.map((name) => `### ${name}\n\n${merged.get(name).join("\n\n")}\n`).join("\n");
};

const git = (cwd, args) =>
  execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });

/**
 * Returns the commit time of the commit that added each fragment, so the
 * release lists the entries in merge order. An untracked fragment has no time.
 */
const addedTimes = (root) => {
  const log = git(root, [
    "log",
    "--diff-filter=A",
    "--format=%x00%ct",
    "--name-only",
    "--",
    FRAGMENT_DIR,
  ]);
  const times = new Map();
  for (const record of log.split("\0").slice(1)) {
    const [time, ...paths] = record.trim().split("\n");
    // `git log` gives the newest commit first. A fragment that a later commit
    // deleted and added again keeps its newest time.
    for (const path of paths.filter((name) => name !== "" && !times.has(name))) {
      times.set(path, Number(time));
    }
  }
  return times;
};

/** Returns the fragment names in merge order. Untracked fragments come last. */
export const orderedFragments = (root) => {
  const times = addedTimes(root);
  const timeOf = (name) => times.get(`${FRAGMENT_DIR}/${name}`) ?? Number.POSITIVE_INFINITY;
  return fragmentNames(join(root, FRAGMENT_DIR)).toSorted(
    (a, b) => timeOf(a) - timeOf(b) || a.localeCompare(b),
  );
};

const assertNoProblems = (problems) => {
  if (problems.length > 0) {
    throw new Error(problems.join("\n"));
  }
};

const checkFragments = (root) => {
  const dir = join(root, FRAGMENT_DIR);
  return fragmentNames(dir).flatMap((name) =>
    fragmentProblems(`${FRAGMENT_DIR}/${name}`, readFileSync(join(dir, name), "utf8")),
  );
};

const VERSION_HEADING = /^## \[\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?\]/gmu;
const RELEASE_SUBJECT =
  /^chore: release v(?<version>\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?: \(#\d+\))?$/u;
// The `version` key of the `[package]` table, up to the next table header.
const MANIFEST_VERSION =
  /^\[package\][^\S\n]*\r?\n(?:(?!\[)[^\n]*\n)*?version\s*=\s*"(?<version>[^"]+)"/mu;

const headingVersion = (heading) => heading.slice("## [".length, -"]".length);

const versionHeadings = (changelog) => new Set(changelog.match(VERSION_HEADING) ?? []);

const refExists = (root, ref) => {
  try {
    git(root, ["rev-parse", "--verify", "--quiet", `${ref}^{commit}`]);
    return true;
  } catch {
    return false;
  }
};

const forkPointOf = (root, base) => {
  if (!refExists(root, base)) {
    throw new Error(
      `--base ${base} does not resolve to a commit. Fetch it (git fetch origin main) or give another ref.`,
    );
  }
  return git(root, ["merge-base", "HEAD", base]).trim();
};

/** The hook step that a release `collect` commit skips. See hk.pkl. */
const HOOK_STEP = "changelog-fragments";

/**
 * A pull request that edits `[Unreleased]` brings back the conflicts that the
 * fragments prevent. So the check compares the section with the fork point.
 * A release commit empties `[Unreleased]` into a new version section. The
 * release metadata check requires that, so it is not a change.
 */
const checkUnreleasedUnchanged = (root, base) => {
  const forkPoint = forkPointOf(root, base);
  const forkChangelog = git(root, ["show", `${forkPoint}:${CHANGELOG}`]);
  const changelog = readFileSync(join(root, CHANGELOG), "utf8");
  const current = unreleasedSection(changelog).body.trim();
  if (current === unreleasedSection(forkChangelog).body.trim()) {
    return [];
  }
  const forkHeadings = versionHeadings(forkChangelog);
  const addsVersion = [...versionHeadings(changelog)].some((heading) => !forkHeadings.has(heading));
  if (current === "" && addsVersion) {
    return [];
  }
  return [
    [
      `${CHANGELOG}: this branch changes the [Unreleased] section. Write changelog entries to a file in ${FRAGMENT_DIR}/ (see ${FRAGMENT_DIR}/${README}), not to ${CHANGELOG}.`,
      `  Release \`collect\` commit only: set HK_SKIP_STEPS=${HOOK_STEP}. The release commit adds the version heading and passes.`,
    ].join("\n"),
  ];
};

// The versions that the commits of the branch release. Merge commits do not
// count: in CI the HEAD of a pull request is a merge commit.
const releasedVersions = (root, forkPoint) =>
  new Set(
    git(root, ["log", "--no-merges", "--format=%s", `${forkPoint}..HEAD`])
      .split("\n")
      .map((subject) => RELEASE_SUBJECT.exec(subject)?.groups.version)
      .filter((version) => version !== undefined),
  );

const manifestVersion = (root) => {
  const path = join(root, "Cargo.toml");
  return existsSync(path)
    ? MANIFEST_VERSION.exec(readFileSync(path, "utf8"))?.groups.version
    : undefined;
};

/**
 * Only the release commit `chore: release vX.Y.Z` adds the heading
 * `## [X.Y.Z]`. A pre-commit hook runs before the commit has a subject. With
 * `pendingRelease`, the version in `Cargo.toml` also counts, and CI checks
 * the subject later.
 */
const checkVersionHeadings = (root, base, pendingRelease) => {
  const forkPoint = forkPointOf(root, base);
  const forkHeadings = versionHeadings(git(root, ["show", `${forkPoint}:${CHANGELOG}`]));
  const added = [...versionHeadings(readFileSync(join(root, CHANGELOG), "utf8"))].filter(
    (heading) => !forkHeadings.has(heading),
  );
  if (added.length === 0) {
    return [];
  }
  const allowed = releasedVersions(root, forkPoint);
  if (pendingRelease) {
    allowed.add(manifestVersion(root));
  }
  return added
    .filter((heading) => !allowed.has(headingVersion(heading)))
    .map(
      (heading) =>
        `${CHANGELOG}: this branch adds the version heading "${heading}". Only a commit with the subject "chore: release v${headingVersion(heading)}" can add it.`,
    );
};

const collect = (root, dryRun, log) => {
  assertNoProblems(checkFragments(root));
  const dir = join(root, FRAGMENT_DIR);
  const names = orderedFragments(root);
  const path = join(root, CHANGELOG);
  const changelog = readFileSync(path, "utf8");
  const { start, end, body } = unreleasedSection(changelog);
  const merged = mergeUnreleased(
    body,
    names.map((name) => readFileSync(join(dir, name), "utf8")),
  );
  if (dryRun) {
    log(merged);
    return 0;
  }
  const section = merged === "" ? "\n" : `\n${merged}\n`;
  writeFileSync(path, `${changelog.slice(0, start)}${section}${changelog.slice(end)}`);
  for (const name of names) {
    rmSync(join(dir, name));
  }
  log(`Collected ${names.length} fragment(s) into the [Unreleased] section of ${CHANGELOG}.`);
  return 0;
};

const COMMAND_OPTIONS = {
  check: { base: { type: "string" }, "pending-release": { type: "boolean" } },
  collect: { "dry-run": { type: "boolean" } },
};

export const parseArgs = ([command, ...args]) => {
  if (!Object.hasOwn(COMMAND_OPTIONS, command ?? "")) {
    throw new Error(USAGE);
  }
  const { values } = parseOptions({ args, options: COMMAND_OPTIONS[command], strict: true });
  return {
    command,
    base: values.base,
    dryRun: values["dry-run"] === true,
    pendingRelease: values["pending-release"] === true,
  };
};

/** Runs one command on the repository at `root` and returns the exit code. */
export const changelogFragments = (
  argv,
  root = resolve(dirname(fileURLToPath(import.meta.url)), ".."),
  log = console.log,
) => {
  const options = parseArgs(argv);
  if (options.command === "collect") {
    return collect(root, options.dryRun, log);
  }
  assertNoProblems([
    ...checkFragments(root),
    ...(options.base === undefined
      ? []
      : [
          ...checkUnreleasedUnchanged(root, options.base),
          ...checkVersionHeadings(root, options.base, options.pendingRelease),
        ]),
  ]);
  log(`${FRAGMENT_DIR}: ${fragmentNames(join(root, FRAGMENT_DIR)).length} fragment(s) are valid.`);
  return 0;
};

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runCliMain(() => changelogFragments(process.argv.slice(2)));
}
