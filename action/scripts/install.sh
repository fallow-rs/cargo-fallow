#!/usr/bin/env bash
set -euo pipefail

# Put a `cargo-fallow` binary on the PATH of the job.
#
# Methods (input `install`):
#   path     use the `cargo-fallow` that is on the PATH already.
#   release  download the binary of a GitHub release and check its SHA-256.
#   cargo    run `cargo install cargo-fallow` from crates.io.
#   source   run `cargo install --path` on the source of the action ref.
#   auto     path, then release, then source.
#
# Required env: INPUT_INSTALL, and ACTION_PATH or GITHUB_ACTION_PATH (the
#   source of the action ref)
# Optional env: INPUT_VERSION, RUNNER_OS, RUNNER_ARCH, RUNNER_TEMP,
#   GITHUB_PATH, GITHUB_OUTPUT, CARGO_FALLOW_RELEASE_URL,
#   CARGO_FALLOW_DRY_RUN (1: print the install commands and do not run them)

: "${ACTION_PATH:=${GITHUB_ACTION_PATH:?the action path is unknown}}"
: "${INPUT_VERSION:=}" "${RUNNER_OS:=}" "${RUNNER_ARCH:=}"
: "${RUNNER_TEMP:=${TMPDIR:-/tmp}}" "${GITHUB_PATH:=/dev/null}"
: "${GITHUB_OUTPUT:=/dev/stdout}" "${CARGO_FALLOW_DRY_RUN:=0}"
: "${CARGO_FALLOW_RELEASE_URL:=https://github.com/fallow-rs/cargo-fallow/releases/download}"

INSTALL_ROOT="${RUNNER_TEMP}/cargo-fallow-install"
BIN_DIR="${INSTALL_ROOT}/bin"

fail() {
  echo "::error::$1"
  exit 1
}

run() {
  if [ "$CARGO_FALLOW_DRY_RUN" = "1" ]; then
    echo "DRY RUN: $*"
    return 0
  fi
  "$@"
}

# The version of the action ref: the first `version` line of Cargo.toml.
action_version() {
  awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' "${ACTION_PATH}/Cargo.toml" 2>/dev/null || true
}

# Set VERSION to the version to install. `0.0.0` is the version of a source
# tree that was never released, so it has no release binary and no crates.io
# package. The function runs in the main shell, so `fail` stops the script.
resolve_version() {
  local version="${INPUT_VERSION#v}"
  if [ -z "$version" ]; then
    version="$(action_version)"
  fi
  if [ -n "$version" ] && ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
    fail "Invalid 'version' input: ${INPUT_VERSION}. Use a version such as 0.1.0."
  fi
  if [ "$version" = "0.0.0" ]; then
    version=""
  fi
  VERSION="$version"
}

target_triple() {
  case "${RUNNER_OS}/${RUNNER_ARCH}" in
    Linux/X64) echo "x86_64-unknown-linux-gnu" ;;
    Linux/ARM64) echo "aarch64-unknown-linux-gnu" ;;
    macOS/ARM64) echo "aarch64-apple-darwin" ;;
    macOS/X64) echo "x86_64-apple-darwin" ;;
    Windows/X64) echo "x86_64-pc-windows-msvc" ;;
    *) return 1 ;;
  esac
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

# Download, check and unpack one release archive. Returns 1 when the release
# has no binary for this runner, so `auto` can try the next method.
install_release() {
  local version="$1" target asset url work expected actual binary
  if [ -z "$version" ]; then
    echo "The action ref has no released version, so it has no release binary."
    return 1
  fi
  if ! target="$(target_triple)"; then
    echo "No release binary for runner ${RUNNER_OS}/${RUNNER_ARCH}."
    return 1
  fi
  asset="cargo-fallow-${target}.tar.xz"
  if [ "$RUNNER_OS" = "Windows" ]; then
    asset="cargo-fallow-${target}.zip"
  fi
  url="${CARGO_FALLOW_RELEASE_URL}/v${version}/${asset}"
  if [ "$CARGO_FALLOW_DRY_RUN" = "1" ]; then
    echo "DRY RUN: download ${url} and check ${url}.sha256"
    return 0
  fi
  work="$(mktemp -d "${RUNNER_TEMP}/cargo-fallow-release.XXXXXX")"
  # Only HTTPS, also after a redirect. `file` serves the local release of the tests.
  if ! curl -fsSL --proto '=https,file' --proto-redir '=https' --tlsv1.2 --retry 3 \
    -o "${work}/${asset}" "$url" \
    || ! curl -fsSL --proto '=https,file' --proto-redir '=https' --tlsv1.2 --retry 3 \
      -o "${work}/${asset}.sha256" "${url}.sha256"; then
    echo "No release asset at ${url}."
    return 1
  fi
  expected="$(awk '{ print $1; exit }' "${work}/${asset}.sha256")"
  actual="$(sha256_of "${work}/${asset}")"
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    fail "The SHA-256 of ${asset} is ${actual}, but the release lists ${expected:-nothing}."
  fi
  mkdir -p "$BIN_DIR" || fail "Could not make ${BIN_DIR}."
  case "$asset" in
    *.zip) unzip -q -o "${work}/${asset}" -d "$work" ;;
    *) tar -xJf "${work}/${asset}" -C "$work" ;;
  esac || fail "Could not unpack ${asset}."
  binary="$(find "$work" -type f \( -name cargo-fallow -o -name cargo-fallow.exe \) | head -n 1)"
  if [ -z "$binary" ]; then
    fail "The archive ${asset} has no cargo-fallow binary."
  fi
  # The function runs in a condition, so `set -e` is off here.
  cp "$binary" "$BIN_DIR/" || fail "Could not copy ${binary} to ${BIN_DIR}."
  chmod +x "$BIN_DIR/$(basename "$binary")" || fail "Could not make the binary executable."
  return 0
}

install_cargo() {
  local version="$1"
  if [ -n "$version" ]; then
    run cargo install --locked --root "$INSTALL_ROOT" --version "$version" cargo-fallow
  else
    run cargo install --locked --root "$INSTALL_ROOT" cargo-fallow
  fi
}

install_source() {
  if [ -n "$INPUT_VERSION" ]; then
    echo "::notice::install: source builds the action ref, so the 'version' input has no effect."
  fi
  run cargo install --locked --root "$INSTALL_ROOT" --path "$ACTION_PATH"
}

# True when cargo-fallow is on the PATH and, with a version input, has that
# version.
path_binary_fits() {
  local found
  command -v cargo-fallow > /dev/null 2>&1 || return 1
  [ -n "$INPUT_VERSION" ] || return 0
  found="$(cargo-fallow --version 2> /dev/null | awk '{ print $2; exit }')"
  if [ "$found" = "${INPUT_VERSION#v}" ]; then
    return 0
  fi
  echo "The cargo-fallow on the PATH has version ${found:-unknown}, not ${INPUT_VERSION#v}."
  return 1
}

main() {
  local method="$INPUT_INSTALL" version used
  resolve_version
  version="$VERSION"
  case "$method" in
    path)
      command -v cargo-fallow > /dev/null 2>&1 \
        || fail "install: path needs cargo-fallow on the PATH, but the PATH has no cargo-fallow."
      used="path"
      ;;
    release)
      install_release "$version" || fail "install: release found no binary. Use install: source or install: cargo."
      used="release"
      ;;
    cargo)
      install_cargo "$version"
      used="cargo"
      ;;
    source)
      install_source
      used="source"
      ;;
    auto)
      if path_binary_fits; then
        used="path"
      elif install_release "$version"; then
        used="release"
      else
        echo "Building cargo-fallow from the source of the action ref."
        install_source
        used="source"
      fi
      ;;
    *)
      fail "Invalid 'install' input: ${method}. Use auto, path, release, cargo or source."
      ;;
  esac

  if [ "$used" != "path" ]; then
    echo "$BIN_DIR" >> "$GITHUB_PATH"
    export PATH="${BIN_DIR}:${PATH}"
  fi
  echo "method=${used}" >> "$GITHUB_OUTPUT"
  if [ "$CARGO_FALLOW_DRY_RUN" != "1" ]; then
    echo "cargo-fallow: $(cargo-fallow --version) (install: ${used})"
  fi
}

main
