#!/usr/bin/env node

// Writes the Claude adapter `.claude/skills/<name>/SKILL.md` for each
// maintainer skill in `.agents/skills/<name>/SKILL.md`. Codex reads
// `.agents/skills` directly. The adapter is the same text with a marker line,
// so a person edits only the source. `--check` fails on drift, and CI runs it.

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { runCliMain } from "./cli-main.mjs";

export const MARKER = "<!-- Generated from .agents/skills. Do not edit. -->";
const SOURCE_ROOT = join(".agents", "skills");
const ADAPTER_ROOT = join(".claude", "skills");
const SKILL_FILE = "SKILL.md";
const FRONTMATTER = /^---\n(?<head>[\s\S]*?)\n---\n(?<body>[\s\S]*)$/u;
const NAME = /^name:\s*(?<name>[a-z0-9-]+)\s*$/mu;

const USAGE = `Usage: node scripts/generate-agent-adapters.mjs [--check]

Writes ${ADAPTER_ROOT}/<name>/${SKILL_FILE} from ${SOURCE_ROOT}/<name>/${SKILL_FILE}.
--check  Change nothing. Exit 1 when an adapter is missing, differs from its
         source, or has no source.`;

/** The adapter text for the source text of the skill in directory `dir`. */
export const renderAdapter = (source, dir) => {
  const match = FRONTMATTER.exec(source.replace(/^\uFEFF/u, "").replaceAll("\r\n", "\n"));
  if (!match) {
    throw new Error(`${dir}: missing YAML frontmatter`);
  }
  const name = NAME.exec(match.groups.head)?.groups.name;
  if (name !== dir) {
    throw new Error(`${dir}: the name is "${name ?? ""}", expected "${dir}"`);
  }
  return `---\n${match.groups.head}\n---\n${MARKER}\n${match.groups.body}`;
};

const posix = (path) => path.replaceAll("\\", "/");

const skillDirs = (root, base) => {
  const dir = join(root, base);
  if (!existsSync(dir)) {
    return [];
  }
  return readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && existsSync(join(dir, entry.name, SKILL_FILE)))
    .map((entry) => entry.name)
    .toSorted();
};

// The paths under ADAPTER_ROOT that git tracks. A host can keep its own
// skills there, so an untracked file is never stale. `null` means that git
// cannot answer: then each file with the marker counts as tracked.
const trackedAdapters = (root) => {
  const env = { ...process.env };
  delete env.GIT_DIR;
  delete env.GIT_WORK_TREE;
  delete env.GIT_INDEX_FILE;
  const result = spawnSync("git", ["ls-files", "-z", "--", posix(ADAPTER_ROOT)], {
    cwd: root,
    encoding: "utf8",
    env,
  });
  if (result.error || result.status !== 0) {
    return null;
  }
  return new Set(result.stdout.split("\0").filter(Boolean));
};

const expectedAdapters = (root) =>
  new Map(
    skillDirs(root, SOURCE_ROOT).map((name) => [
      posix(join(ADAPTER_ROOT, name, SKILL_FILE)),
      renderAdapter(readFileSync(join(root, SOURCE_ROOT, name, SKILL_FILE), "utf8"), name),
    ]),
  );

const staleAdapters = (root, expected) => {
  const tracked = trackedAdapters(root);
  return skillDirs(root, ADAPTER_ROOT)
    .map((name) => posix(join(ADAPTER_ROOT, name, SKILL_FILE)))
    .filter((path) => !expected.has(path))
    .filter((path) => readFileSync(join(root, path), "utf8").includes(MARKER))
    .filter((path) => tracked === null || tracked.has(path));
};

/** One line for each adapter that is missing, out of date, or stale. */
export const adapterProblems = (root) => {
  const expected = expectedAdapters(root);
  const problems = [];
  for (const [path, text] of expected) {
    const file = join(root, path);
    if (!existsSync(file)) {
      problems.push(`${path}: missing`);
    } else if (readFileSync(file, "utf8") !== text) {
      problems.push(`${path}: differs from the source`);
    }
  }
  for (const path of staleAdapters(root, expected)) {
    problems.push(`${path}: no source in ${posix(SOURCE_ROOT)}`);
  }
  return problems;
};

/** Writes each adapter and removes each stale adapter. */
export const writeAdapters = (root) => {
  const expected = expectedAdapters(root);
  for (const path of staleAdapters(root, expected)) {
    rmSync(join(root, path));
    if (readdirSync(dirname(join(root, path))).length === 0) {
      rmdirSync(dirname(join(root, path)));
    }
  }
  for (const [path, text] of expected) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
};

const main = () => {
  const { values } = parseArgs({
    options: { check: { type: "boolean" }, help: { type: "boolean" } },
  });
  if (values.help) {
    console.log(USAGE);
    return 0;
  }
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  if (!values.check) {
    writeAdapters(root);
    return 0;
  }
  const problems = adapterProblems(root);
  for (const problem of problems) {
    console.error(problem);
  }
  if (problems.length > 0) {
    console.error("Run: npm run generate:agent-adapters");
    return 1;
  }
  return 0;
};

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runCliMain(main);
}
