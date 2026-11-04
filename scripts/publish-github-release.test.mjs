import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  assetProblems,
  expectedAssetNames,
  notesProblems,
  parseArguments,
  publishGithubRelease,
  runProblems,
  titleProblems,
} from "./publish-github-release.mjs";

const EM_DASH = String.fromCodePoint(0x2014);
const SHA = "c".repeat(40);
const COMPARE = "https://github.com/fallow-rs/cargo-fallow/compare/v0.1.0...v0.2.0";
const TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
];

const CHANGELOG = [
  "# Changelog",
  "",
  "## [Unreleased]",
  "",
  "## [0.2.0] - 2026-12-01",
  "",
  "### Added",
  "",
  "- New.",
  "",
  "## [0.1.0] - 2026-11-04",
  "",
  "### Added",
  "",
  "- First.",
  "",
].join("\n");

const tempDir = (t) => {
  const dir = mkdtempSync(join(tmpdir(), "publish-release-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
};

const sha256 = (text) => createHash("sha256").update(text).digest("hex");

/** Writes a complete asset set, then applies `change` to the file map. */
const writeAssets = (dir, change = (files) => files) => {
  const files = new Map();
  const sums = [];
  for (const target of TARGETS) {
    const archive = `cargo-fallow-0.2.0-${target}.${target.includes("windows") ? "zip" : "tar.gz"}`;
    const content = `binary for ${target}`;
    const line = `${sha256(content)}  ${archive}\n`;
    files.set(archive, content);
    files.set(`${archive}.sha256`, line);
    files.set(`${archive}.sig`, "signature");
    sums.push(line);
  }
  files.set("SHA256SUMS", sums.join(""));
  for (const [name, content] of change(files)) {
    writeFileSync(join(dir, name), content);
  }
};

const run = (overrides = {}) => ({
  headSha: SHA,
  headBranch: "main",
  event: "workflow_dispatch",
  workflowName: "Release",
  status: "completed",
  conclusion: "success",
  jobs: [{ name: "Release ready for signed tag", conclusion: "success" }],
  ...overrides,
});

test("the asset set has an archive, a checksum and a signature per target, and SHA256SUMS", () => {
  const names = expectedAssetNames("0.2.0");
  assert.equal(names.length, TARGETS.length * 3 + 1);
  assert.ok(names.includes("cargo-fallow-0.2.0-x86_64-pc-windows-msvc.zip.sig"));
  assert.ok(names.includes("cargo-fallow-0.2.0-aarch64-apple-darwin.tar.gz.sha256"));
  assert.ok(names.includes("SHA256SUMS"));
});

test("a complete asset set with matching checksums has no problem", (t) => {
  const dir = tempDir(t);
  writeAssets(dir);
  assert.deepEqual(assetProblems(dir, "0.2.0"), []);
});

test("a missing, an extra or an empty asset is a problem", (t) => {
  const dir = tempDir(t);
  writeAssets(dir, (files) => {
    files.delete("cargo-fallow-0.2.0-x86_64-apple-darwin.tar.gz.sig");
    files.set("notes.md", "x");
    files.set("cargo-fallow-0.2.0-aarch64-apple-darwin.tar.gz.sig", "");
    return files;
  });
  assert.deepEqual(assetProblems(dir, "0.2.0"), [
    "release-assets has no cargo-fallow-0.2.0-x86_64-apple-darwin.tar.gz.sig",
    "release-assets has the unexpected file notes.md",
    "cargo-fallow-0.2.0-aarch64-apple-darwin.tar.gz.sig is empty",
  ]);
});

test("an archive that does not match SHA256SUMS is a problem", (t) => {
  const dir = tempDir(t);
  writeAssets(dir, (files) => {
    files.set("cargo-fallow-0.2.0-x86_64-unknown-linux-musl.tar.gz", "other bytes");
    return files;
  });
  assert.deepEqual(assetProblems(dir, "0.2.0"), [
    "cargo-fallow-0.2.0-x86_64-unknown-linux-musl.tar.gz does not match its line in SHA256SUMS",
  ]);
});

test("SHA256SUMS must hold the .sha256 lines and nothing else", (t) => {
  const dir = tempDir(t);
  writeAssets(dir, (files) => {
    files.set("SHA256SUMS", `${files.get("SHA256SUMS")}${"0".repeat(64)}  extra.tar.gz\n`);
    return files;
  });
  assert.deepEqual(assetProblems(dir, "0.2.0"), [
    "SHA256SUMS is not the list of the .sha256 files of the archives",
  ]);
});

test("the title needs the tag prefix, a summary and no em dash", () => {
  assert.deepEqual(titleProblems("v0.2.0: Faster checks", "v0.2.0"), []);
  assert.deepEqual(titleProblems("cargo-fallow v0.2.0", "v0.2.0"), [
    'The title must start with "v0.2.0: " and give a summary',
  ]);
  assert.deepEqual(titleProblems("v0.2.0: ", "v0.2.0"), [
    'The title must start with "v0.2.0: " and give a summary',
  ]);
  assert.deepEqual(titleProblems(`v0.2.0: A ${EM_DASH} B`, "v0.2.0"), [
    "The title contains an em dash",
  ]);
});

test("the notes are not empty, have no em dash and end with the compare URL", () => {
  assert.deepEqual(notesProblems(`Notes.\n\n**Full Changelog**: ${COMPARE}\n\n`, COMPARE), []);
  assert.deepEqual(notesProblems(" \n", COMPARE), [
    "The notes file is empty",
    `The notes must end with ${COMPARE}`,
  ]);
  assert.deepEqual(notesProblems(`${COMPARE}\n\nMore.\n`, COMPARE), [
    `The notes must end with ${COMPARE}`,
  ]);
  assert.deepEqual(notesProblems(`A ${EM_DASH} B.\n${COMPARE}\n`, COMPARE), [
    "The notes contain an em dash",
  ]);
});

test("the run must be a finished release run on main with a passed release-ready job", () => {
  assert.deepEqual(runProblems(run()), []);
  assert.deepEqual(
    runProblems(
      run({
        headBranch: "feat/x",
        event: "push",
        workflowName: "CI",
        conclusion: "failure",
        jobs: [{ name: "Release ready for signed tag", conclusion: "skipped" }],
      }),
    ),
    [
      "The run is of the workflow CI, not Release",
      "The run started from push, not workflow_dispatch",
      "The run is on feat/x, not main",
      "The run is completed with the conclusion failure, not success",
      'The job "Release ready for signed tag" has the conclusion skipped, not success',
    ],
  );
  assert.deepEqual(runProblems(run({ jobs: [] })), [
    'The run has no job "Release ready for signed tag"',
  ]);
});

/**
 * A fake command runner. It answers the git, gh and node commands of a
 * release, and records each command.
 */
const fakeRunner = ({ notesDir, mainSha = SHA, tagStatus = 2, assets = writeAssets } = {}) => {
  const calls = [];
  const runner = (command, args) => {
    const line = [command, ...args].join(" ");
    calls.push(line);
    if (command === "gh" && args[0] === "run" && args[1] === "view") {
      return { status: 0, stdout: JSON.stringify(run()) };
    }
    if (command === "gh" && args[0] === "run" && args[1] === "download") {
      assets(args[args.indexOf("--dir") + 1]);
      return { status: 0, stdout: "" };
    }
    if (line === "git ls-remote --exit-code origin refs/heads/main") {
      return { status: 0, stdout: `${mainSha}\trefs/heads/main\n` };
    }
    if (line === "git ls-remote --exit-code --tags origin refs/tags/v0.2.0") {
      return { status: tagStatus, stdout: "" };
    }
    if (line === `git log -1 --format=%s ${SHA}`) {
      return { status: 0, stdout: "chore: release v0.2.0\n" };
    }
    if (line === `git show ${SHA}:CHANGELOG.md`) {
      return { status: 0, stdout: CHANGELOG };
    }
    if (line === "git rev-parse --verify --quiet refs/tags/v0.2.0") {
      return { status: 1, stdout: "" };
    }
    return { status: 0, stdout: "" };
  };
  const notes = join(notesDir, "notes.md");
  writeFileSync(notes, `Notes.\n\n${COMPARE}\n`);
  return { calls, runner, notes };
};

const options = (notes, overrides = {}) => ({
  runId: "123",
  version: "0.2.0",
  title: "v0.2.0: Faster checks",
  notes,
  checkOnly: false,
  ...overrides,
});

test("a correct release tags the run commit, pushes the tag and creates the release", (t) => {
  const fake = fakeRunner({ notesDir: tempDir(t) });
  const lines = [];
  const code = publishGithubRelease(options(fake.notes), {
    run: fake.runner,
    log: (line) => lines.push(line),
    tempDir: () => tempDir(t),
  });
  assert.equal(code, 0, lines.join("\n"));
  const actions = fake.calls.filter((call) => /^git (tag|verify-tag|push)|^gh release/u.test(call));
  assert.equal(actions.length, 4, actions.join("\n"));
  assert.equal(actions[0], `git tag -s v0.2.0 ${SHA} -m cargo-fallow 0.2.0`);
  assert.equal(actions[1], "git verify-tag v0.2.0");
  assert.equal(actions[2], "git push origin refs/tags/v0.2.0");
  assert.match(
    actions[3],
    /^gh release create v0\.2\.0 --repo fallow-rs\/cargo-fallow --verify-tag --title v0\.2\.0: Faster checks --notes-file \S+notes\.md /u,
  );
  assert.equal(
    actions[3].split(" ").filter((part) => part.includes("/cargo-fallow-0.2.0-")).length,
    15,
  );
  assert.match(actions[3], /\/SHA256SUMS(?: |$)/u);
  const verified = fake.calls.filter((call) =>
    call.includes("action/scripts/verify-signature.mjs"),
  );
  assert.equal(verified.length, TARGETS.length);
});

test("check-only runs every check and creates no tag and no release", (t) => {
  const fake = fakeRunner({ notesDir: tempDir(t) });
  const code = publishGithubRelease(options(fake.notes, { checkOnly: true }), {
    run: fake.runner,
    log: () => {},
    tempDir: () => tempDir(t),
  });
  assert.equal(code, 0);
  assert.ok(fake.calls.some((call) => call.startsWith("gh run download 123")));
  assert.equal(fake.calls.filter((call) => /^git (tag|push)|^gh release/u.test(call)).length, 0);
});

test("a problem stops the release before the tag", (t) => {
  const cases = [
    [{ mainSha: "d".repeat(40) }, /origin\/main is d{40}, not the release commit c{40}/u],
    [{ tagStatus: 0 }, /The tag v0\.2\.0 already exists on the remote/u],
    [
      {
        assets: (dir) =>
          writeAssets(dir, (files) => {
            files.delete("SHA256SUMS");
            return files;
          }),
      },
      /release-assets has no SHA256SUMS/u,
    ],
  ];
  for (const [settings, error] of cases) {
    const fake = fakeRunner({ notesDir: tempDir(t), ...settings });
    const lines = [];
    const code = publishGithubRelease(options(fake.notes), {
      run: fake.runner,
      log: (line) => lines.push(line),
      tempDir: () => tempDir(t),
    });
    assert.equal(code, 1);
    assert.match(lines.join("\n"), error);
    assert.equal(fake.calls.filter((call) => /^git (tag|push)|^gh release/u.test(call)).length, 0);
  }
});

test("a wrong title or notes file stops the release before any download", (t) => {
  const fake = fakeRunner({ notesDir: tempDir(t) });
  const lines = [];
  const code = publishGithubRelease(options(fake.notes, { title: "v0.2.0" }), {
    run: fake.runner,
    log: (line) => lines.push(line),
    tempDir: () => tempDir(t),
  });
  assert.equal(code, 1);
  assert.match(lines.join("\n"), /The title must start with "v0\.2\.0: "/u);
  assert.equal(fake.calls.filter((call) => call.startsWith("gh run download")).length, 0);
});

test("the arguments need a run, a version, a title and a notes file", () => {
  assert.deepEqual(
    parseArguments([
      "--run",
      "123",
      "--version",
      "0.2.0",
      "--title",
      "v0.2.0: Faster checks",
      "--notes",
      "notes.md",
      "--check-only",
    ]),
    {
      runId: "123",
      version: "0.2.0",
      title: "v0.2.0: Faster checks",
      notes: "notes.md",
      checkOnly: true,
    },
  );
  assert.throws(
    () => parseArguments(["--run", "123"]),
    /Usage: node scripts\/publish-github-release\.mjs/u,
  );
  assert.throws(
    () => parseArguments(["--run", "x", "--version", "0.2.0", "--title", "t", "--notes", "n"]),
    /--run must be a workflow run ID/u,
  );
  assert.throws(
    () => parseArguments(["--run", "1", "--version", "v0.2.0", "--title", "t", "--notes", "n"]),
    /--version must be MAJOR\.MINOR\.PATCH/u,
  );
});
