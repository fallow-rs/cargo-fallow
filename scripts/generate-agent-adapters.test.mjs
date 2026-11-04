import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";

import {
  MARKER,
  adapterProblems,
  renderAdapter,
  writeAdapters,
} from "./generate-agent-adapters.mjs";

const SKILL = "---\nname: ship\ndescription: Land the work.\n---\n\n# Ship\n\n1. Merge.\n";

const write = (root, path, text) => {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), text);
};

const git = (root, ...args) => {
  const env = { ...process.env };
  delete env.GIT_DIR;
  delete env.GIT_WORK_TREE;
  delete env.GIT_INDEX_FILE;
  return execFileSync("git", args, { cwd: root, env, stdio: "pipe" });
};

// A repository with one canonical skill. The test removes it at the end.
const createRepo = (t) => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "agent-adapters-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  git(root, "init", "-q");
  write(root, ".agents/skills/ship/SKILL.md", SKILL);
  return root;
};

const adapterPath = (name) => `.claude/skills/${name}/SKILL.md`;

test("renderAdapter puts the marker right after the frontmatter", () => {
  assert.equal(
    renderAdapter(SKILL, "ship"),
    `---\nname: ship\ndescription: Land the work.\n---\n${MARKER}\n\n# Ship\n\n1. Merge.\n`,
  );
});

test("renderAdapter rejects a skill without frontmatter", () => {
  assert.throws(() => renderAdapter("# Ship\n", "ship"), /ship: missing YAML frontmatter/u);
});

test("renderAdapter rejects a name that is not the directory name", () => {
  assert.throws(() => renderAdapter(SKILL, "review"), /review: the name is "ship"/u);
});

test("a missing adapter is a problem until writeAdapters writes it", (t) => {
  const root = createRepo(t);
  assert.deepEqual(adapterProblems(root), [`${adapterPath("ship")}: missing`]);
  writeAdapters(root);
  assert.equal(readFileSync(join(root, adapterPath("ship")), "utf8"), renderAdapter(SKILL, "ship"));
  assert.deepEqual(adapterProblems(root), []);
});

test("a hand edit of an adapter is a problem", (t) => {
  const root = createRepo(t);
  writeAdapters(root);
  write(root, adapterPath("ship"), `${renderAdapter(SKILL, "ship")}extra\n`);
  assert.deepEqual(adapterProblems(root), [`${adapterPath("ship")}: differs from the source`]);
});

test("a tracked adapter without a source is stale, and writeAdapters removes it", (t) => {
  const root = createRepo(t);
  writeAdapters(root);
  write(root, adapterPath("old"), `---\nname: old\n---\n${MARKER}\n`);
  git(root, "add", "-f", "-A", ".");
  assert.deepEqual(adapterProblems(root), [`${adapterPath("old")}: no source in .agents/skills`]);
  writeAdapters(root);
  assert.equal(existsSync(join(root, adapterPath("old"))), false);
  assert.deepEqual(adapterProblems(root), []);
});

test("an untracked local skill under .claude/skills stays and is no problem", (t) => {
  const root = createRepo(t);
  writeAdapters(root);
  git(root, "add", "-f", "-A", ".");
  write(root, adapterPath("fallow"), `---\nname: fallow\n---\n${MARKER}\n`);
  assert.deepEqual(adapterProblems(root), []);
  writeAdapters(root);
  assert.equal(existsSync(join(root, adapterPath("fallow"))), true);
});

test("renderAdapter reads a source with CRLF line ends and a BOM", () => {
  const crlf = `\uFEFF${SKILL.replaceAll("\n", "\r\n")}`;
  assert.equal(renderAdapter(crlf, "ship"), renderAdapter(SKILL, "ship"));
});

test("writeAdapters keeps an untracked file next to a stale adapter", (t) => {
  const root = createRepo(t);
  write(root, adapterPath("old"), `---\nname: old\n---\n${MARKER}\n`);
  git(root, "add", "-f", "-A", ".");
  write(root, ".claude/skills/old/notes.md", "local notes\n");
  writeAdapters(root);
  assert.equal(existsSync(join(root, adapterPath("old"))), false);
  assert.equal(existsSync(join(root, ".claude/skills/old/notes.md")), true);
});
