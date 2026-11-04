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
  changelogFragments,
  fragmentNames,
  fragmentProblems,
  mergeUnreleased,
  parseArgs,
  splitSections,
} from "./changelog-fragments.mjs";

const EM_DASH = String.fromCodePoint(0x2014);
const KIND_LIST =
  "use one of: Breaking, Added, Changed, Deprecated, Removed, Fixed, Performance, Security";

const changelog = (...unreleased) =>
  [
    "# Changelog",
    "",
    "## [Unreleased]",
    "",
    ...unreleased,
    "## [0.1.0] - 2026-11-04",
    "",
    "### Added",
    "",
    "- First release.",
    "",
  ].join("\n");

// A repository with a private git config, so the signing settings of the
// person who runs the tests do not apply to the fixture.
const createRepo = (t) => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "changelog-fragments-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const globalConfig = join(root, "gitconfig");
  writeFileSync(
    globalConfig,
    "[user]\n\tname = Test\n\temail = test@example.invalid\n[commit]\n\tgpgsign = false\n[init]\n\tdefaultBranch = main\n",
  );
  const work = join(root, "work");
  mkdirSync(work);
  const env = { ...process.env, GIT_CONFIG_GLOBAL: globalConfig, GIT_CONFIG_NOSYSTEM: "1" };
  for (const name of ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"]) {
    delete env[name];
  }
  let clock = 1_700_000_000;
  const git = (...args) =>
    execFileSync("git", args, { cwd: work, env, encoding: "utf8", stdio: "pipe" }).trim();
  const write = (path, text) => {
    mkdirSync(dirname(join(work, path)), { recursive: true });
    writeFileSync(join(work, path), text);
  };
  const commit = (message, files) => {
    for (const [path, text] of Object.entries(files)) {
      write(path, text);
    }
    git("add", "--all");
    clock += 60;
    const date = `@${clock} +0000`;
    execFileSync("git", ["commit", "--quiet", "-m", message], {
      cwd: work,
      env: { ...env, GIT_AUTHOR_DATE: date, GIT_COMMITTER_DATE: date },
      stdio: "pipe",
    });
  };
  const read = (path) => readFileSync(join(work, path), "utf8");
  git("init", "--quiet");
  return { work, git, write, commit, read };
};

const run = (repo, ...argv) => {
  const lines = [];
  const code = changelogFragments(argv, repo.work, (line) => lines.push(line));
  return { code, output: lines.join("\n") };
};

test("a valid fragment has no problems", () => {
  assert.deepEqual(
    fragmentProblems("changelog.d/a.md", "### Added\n\n- New.\n\n### Fixed\n\n- Fix.\n"),
    [],
  );
});

test("each fragment problem names the path and the rule", () => {
  assert.deepEqual(
    fragmentProblems(
      "changelog.d/a.md",
      `Intro.\n\n### Added\n\n- One ${EM_DASH} two.\n\n### Notes\n\n- X.\n\n### Added\n\n- Again.\n\n### Fixed\n`,
    ),
    [
      "changelog.d/a.md: has text before its first ### heading",
      `changelog.d/a.md: has the unknown heading "### Notes" (${KIND_LIST})`,
      'changelog.d/a.md: repeats "### Added"',
      'changelog.d/a.md: has an empty "### Fixed" section',
      "changelog.d/a.md: contains an em dash, which the release notes inherit",
    ],
  );
  assert.deepEqual(fragmentProblems("changelog.d/b.md", "- No heading.\n"), [
    "changelog.d/b.md: has text before its first ### heading",
    `changelog.d/b.md: has no ### heading (${KIND_LIST})`,
  ]);
});

test("a ### line in a fenced code block is entry text", () => {
  const { sections } = splitSections("### Added\n\n- Example:\n\n  ```md\n  ### Fixed\n  ```\n");
  assert.deepEqual(
    sections.map(({ name }) => name),
    ["Added"],
  );
  assert.match(sections[0].body, /### Fixed/u);
});

test("the merge keeps the existing entries first and gives each heading once", () => {
  const merged = mergeUnreleased("\n### Fixed\n\n- Old fix.\n\n", [
    "### Fixed\n\n- New fix.\n",
    "### Added\n\n- New feature.\n",
  ]);
  assert.equal(merged, "### Added\n\n- New feature.\n\n### Fixed\n\n- Old fix.\n\n- New fix.\n");
  assert.equal(mergeUnreleased("\n", []), "");
});

test("the fragment list has no README and is sorted", (t) => {
  const repo = createRepo(t);
  repo.write("changelog.d/README.md", "# Fragments\n");
  repo.write("changelog.d/b.md", "### Added\n\n- B.\n");
  repo.write("changelog.d/a.md", "### Added\n\n- A.\n");
  repo.write("changelog.d/notes.txt", "x");
  assert.deepEqual(fragmentNames(join(repo.work, "changelog.d")), ["a.md", "b.md"]);
  assert.deepEqual(fragmentNames(join(repo.work, "missing")), []);
});

test("check fails on a fragment with a problem", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", {
    "CHANGELOG.md": changelog(),
    "changelog.d/a.md": "- No heading.\n",
  });
  assert.throws(() => run(repo, "check"), /changelog\.d\/a\.md: has no ### heading/u);
});

test("check with a base fails when the branch edits the Unreleased section", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", { "CHANGELOG.md": changelog() });
  repo.git("branch", "base");
  repo.commit("feat: edit", { "CHANGELOG.md": changelog("### Added", "", "- Direct.", "") });
  assert.throws(
    () => run(repo, "check", "--base", "base"),
    /this branch changes the \[Unreleased\] section\. Write changelog entries to a file in changelog\.d\//u,
  );
});

test("check with a base passes for a fragment and for a release commit", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", { "CHANGELOG.md": changelog() });
  repo.git("branch", "base");
  repo.commit("feat: fragment", { "changelog.d/feat-a.md": "### Added\n\n- A.\n" });
  assert.equal(run(repo, "check", "--base", "base").code, 0);

  repo.git("checkout", "--quiet", "--detach", "base");
  repo.commit("chore: pending", { "CHANGELOG.md": changelog("### Added", "", "- A.", "") });
  repo.git("branch", "-f", "base", "HEAD");
  const released = readFileSync(join(repo.work, "CHANGELOG.md"), "utf8").replace(
    "## [Unreleased]\n\n### Added\n\n- A.\n\n",
    "## [Unreleased]\n\n## [0.2.0] - 2026-12-01\n\n### Added\n\n- A.\n\n",
  );
  repo.commit("chore: release v0.2.0", { "CHANGELOG.md": released });
  assert.equal(run(repo, "check", "--base", "base").code, 0);
});

// A branch from `base` with one pending entry in [Unreleased] on `base`.
const pendingRepo = (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", { "CHANGELOG.md": changelog("### Added", "", "- A.", "") });
  repo.git("branch", "base");
  return repo;
};

const released = (repo, version) =>
  repo
    .read("CHANGELOG.md")
    .replace(
      "## [Unreleased]\n\n### Added\n\n- A.\n\n",
      `## [Unreleased]\n\n## [${version}] - 2026-12-01\n\n### Added\n\n- A.\n\n`,
    );

test("check with a base fails when a commit that is not the release adds a version heading", (t) => {
  const repo = pendingRepo(t);
  repo.commit("feat: something", { "CHANGELOG.md": released(repo, "0.2.0") });
  assert.throws(
    () => run(repo, "check", "--base", "base"),
    /CHANGELOG\.md: this branch adds the version heading "## \[0\.2\.0\]"\. Only a commit with the subject "chore: release v0\.2\.0" can add it\./u,
  );
});

test("check with a base fails when a release commit adds the heading of another version", (t) => {
  const repo = pendingRepo(t);
  repo.commit("chore: release v0.3.0", { "CHANGELOG.md": released(repo, "0.2.0") });
  assert.throws(
    () => run(repo, "check", "--base", "base"),
    /adds the version heading "## \[0\.2\.0\]"/u,
  );
});

test("check with a base fails on a new version section that leaves Unreleased as it is", (t) => {
  const repo = pendingRepo(t);
  const text = repo
    .read("CHANGELOG.md")
    .replace(
      "## [0.1.0] - 2026-11-04",
      "## [0.1.1] - 2026-11-05\n\n### Fixed\n\n- B.\n\n## [0.1.0] - 2026-11-04",
    );
  repo.commit("docs: add a section", { "CHANGELOG.md": text });
  assert.throws(
    () => run(repo, "check", "--base", "base"),
    /adds the version heading "## \[0\.1\.1\]"/u,
  );
});

test("check with a base accepts the release commit among other commits of the branch", (t) => {
  const repo = pendingRepo(t);
  repo.commit("chore: release v0.2.0 (#64)", { "CHANGELOG.md": released(repo, "0.2.0") });
  repo.commit("fix: a late fix", { "src.txt": "x\n" });
  assert.equal(run(repo, "check", "--base", "base").code, 0);
});

test("check with --pending-release accepts the heading of the Cargo.toml version", (t) => {
  const repo = pendingRepo(t);
  repo.write("CHANGELOG.md", released(repo, "0.2.0"));
  repo.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.2.0"\n');
  assert.equal(run(repo, "check", "--base", "base", "--pending-release").code, 0);
  assert.throws(() => run(repo, "check", "--base", "base"), /adds the version heading/u);
  repo.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.3.0"\n');
  assert.throws(
    () => run(repo, "check", "--base", "base", "--pending-release"),
    /adds the version heading "## \[0\.2\.0\]"/u,
  );
});

test("check with a base fails on a new pre-release heading", (t) => {
  const repo = pendingRepo(t);
  repo.commit("feat: something", { "CHANGELOG.md": released(repo, "1.0.0-rc.1") });
  assert.throws(
    () => run(repo, "check", "--base", "base"),
    /adds the version heading "## \[1\.0\.0-rc\.1\]"/u,
  );
});

test("check with --pending-release reads the version of the [package] table", (t) => {
  const repo = pendingRepo(t);
  repo.write("CHANGELOG.md", released(repo, "0.2.0"));
  repo.write(
    "Cargo.toml",
    '[dependencies.y]\nversion = "0.2.0"\n\n[package]\nname = "x"\nversion = "0.1.0"\n',
  );
  assert.throws(
    () => run(repo, "check", "--base", "base", "--pending-release"),
    /adds the version heading "## \[0\.2\.0\]"/u,
  );
});

test("check with a base that does not resolve names the fix", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", { "CHANGELOG.md": changelog() });
  assert.throws(() => run(repo, "check", "--base", "nope"), /--base nope does not resolve/u);
});

test("collect moves the fragments into Unreleased in merge order and deletes them", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", { "CHANGELOG.md": changelog() });
  repo.commit("feat: z", { "changelog.d/z.md": "### Fixed\n\n- Z first.\n" });
  repo.commit("feat: a", { "changelog.d/a.md": "### Fixed\n\n- A second.\n\n### Added\n\n- A.\n" });
  repo.write("changelog.d/README.md", "# Fragments\n");

  const dry = run(repo, "collect", "--dry-run");
  assert.equal(dry.output, "### Added\n\n- A.\n\n### Fixed\n\n- Z first.\n\n- A second.\n");
  assert.ok(existsSync(join(repo.work, "changelog.d/z.md")));

  assert.equal(run(repo, "collect").code, 0);
  assert.equal(
    repo.read("CHANGELOG.md"),
    changelog("### Added", "", "- A.", "", "### Fixed", "", "- Z first.", "", "- A second.", ""),
  );
  assert.deepEqual(fragmentNames(join(repo.work, "changelog.d")), []);
  assert.ok(existsSync(join(repo.work, "changelog.d/README.md")));
});

test("collect refuses an invalid fragment and changes nothing", (t) => {
  const repo = createRepo(t);
  repo.commit("chore: init", {
    "CHANGELOG.md": changelog(),
    "changelog.d/a.md": "### Notes\n\n- X.\n",
  });
  assert.throws(() => run(repo, "collect"), /unknown heading "### Notes"/u);
  assert.equal(repo.read("CHANGELOG.md"), changelog());
});

test("an unknown command prints the usage", () => {
  assert.throws(() => parseArgs(["nope"]), /Usage: node scripts\/changelog-fragments\.mjs/u);
  assert.throws(() => parseArgs([]), /Usage:/u);
  assert.deepEqual(parseArgs(["collect", "--dry-run"]), {
    command: "collect",
    base: undefined,
    dryRun: true,
    pendingRelease: false,
  });
});

test("the fragments of the repository are valid", () => {
  const root = new URL("../", import.meta.url).pathname;
  assert.equal(
    changelogFragments(["check"], root, () => {}),
    0,
  );
});
