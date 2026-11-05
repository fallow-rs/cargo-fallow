import assert from "node:assert/strict";
import test from "node:test";

import { MAX_FILE_BYTES, REQUIRED, checkPackage, isAllowed, parseList } from "./check-package.mjs";

const small = () => 100;

test("a package of source and top-level files passes", () => {
  const paths = [...REQUIRED, "src/parse.rs", "src/mutation/engine.rs", ".cargo_vcs_info.json"];
  assert.deepEqual(checkPackage(paths, small), []);
});

test("fixtures, fuzz, corpus, experimental and tooling files are not allowed", () => {
  for (const path of [
    "tests/fixtures/ws-ok/Cargo.toml",
    "tests/cli.rs",
    "fuzz/fuzz_targets/fuzz_parse.rs",
    "corpus/ripgrep.json",
    "experimental/precise/src/main.rs",
    ".github/workflows/release.yml",
    "scripts/check-package.mjs",
    "src/data.json",
    "src/hooks/other.sh",
    "src/../Cargo.toml",
  ]) {
    assert.equal(isAllowed(path), false, path);
    assert.deepEqual(checkPackage([...REQUIRED, path], small), [
      `not allowed in the package: ${path}`,
    ]);
  }
});

test("a missing required file and a large file are problems", () => {
  const paths = REQUIRED.filter((path) => path !== "CHANGELOG.md").concat("src/big.rs");
  const sizeOf = (path) => (path === "src/big.rs" ? MAX_FILE_BYTES + 1 : 1);
  assert.deepEqual(checkPackage(paths, sizeOf), [
    `larger than ${MAX_FILE_BYTES} bytes: src/big.rs (${MAX_FILE_BYTES + 1} bytes)`,
    "missing from the package: CHANGELOG.md",
  ]);
});

test("the list parser reads Windows separators and line endings", () => {
  assert.deepEqual(parseList("src\\lib.rs\r\nCargo.toml\r\n\r\n"), ["src/lib.rs", "Cargo.toml"]);
});
