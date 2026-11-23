#!/usr/bin/env bash
# RustSec audit of one StrawberrySwap Rust crate: its Cargo.lock against the advisory database, under the policy in
# its .cargo/audit.toml. Any known vulnerability, unsound crate or yanked version fails; unmaintained crates are
# reported but pass. It is the backend's gate (backend/Dockerfile, backend/.cargo/audit.toml), applied to the
# engine, the oracle and the vault. CI runs it in each crate's job (.github/workflows/ci.yml).
#
#   cd bots/hft-engine-rust && CARGO_AUDIT_VERSION=0.22.2 CARGO_AUDIT_SHA256=<tarball sha256> ../../ops/ci/cargo-audit.sh
#
# * cargo-audit is the backend gate's version. One of that version on PATH is used as is. Otherwise its release
#   tarball (x86_64 Linux) is downloaded and checked against CARGO_AUDIT_SHA256 before it runs.
# * An exception in audit.toml may rest on this premise: production builds hold no private key, because every key is in
#   AWS KMS. So the audit first fails if a local private-key signer (alloy-signer-local) is in the crate's production
#   build graph: normal and build dependencies, default features. Tests and debug-only features may use one.
# * The advisory database is fetched fresh on every run, so a new advisory fails the next run, whatever changed.
set -euo pipefail

die() {
  echo "cargo-audit.sh: $*" >&2
  exit 2
}

: "${CARGO_AUDIT_VERSION:?the cargo-audit version, e.g. 0.22.2 (as in the backend gate)}"
[ -f Cargo.lock ] || die "run it from a crate's directory: no Cargo.lock in $PWD"
[ -f .cargo/audit.toml ] || die "no .cargo/audit.toml in $PWD: every audited crate states its policy there"

audit=$(command -v cargo-audit || true)
if [ -z "$audit" ] || [ "$("$audit" --version)" != "cargo-audit ${CARGO_AUDIT_VERSION}" ]; then
  [ "$(uname -s)-$(uname -m)" = "Linux-x86_64" ] ||
    die "no cargo-audit ${CARGO_AUDIT_VERSION} on PATH: cargo install cargo-audit --locked --version ${CARGO_AUDIT_VERSION}"
  : "${CARGO_AUDIT_SHA256:?the SHA-256 of the cargo-audit release tarball, pinned in .github/workflows/ci.yml}"
  dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/cargo-audit-${CARGO_AUDIT_VERSION}"
  if [ ! -x "$dir/cargo-audit" ]; then
    name="cargo-audit-x86_64-unknown-linux-musl-v${CARGO_AUDIT_VERSION}"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    curl -fsSL --retry 3 -o "$tmp/$name.tgz" \
      "https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv${CARGO_AUDIT_VERSION}/$name.tgz"
    echo "${CARGO_AUDIT_SHA256}  $tmp/$name.tgz" | sha256sum --check --strict --quiet - ||
      die "$name.tgz does not match CARGO_AUDIT_SHA256"
    tar -xzf "$tmp/$name.tgz" -C "$tmp" "$name/cargo-audit"
    mkdir -p "$dir"
    install -m 0755 "$tmp/$name/cargo-audit" "$dir/cargo-audit"
  fi
  audit="$dir/cargo-audit"
fi
"$audit" --version

graph=$(cargo tree --locked -e normal,build --prefix none --format '{p}')
if grep -q '^alloy-signer-local ' <<<"$graph"; then
  echo "::error::alloy-signer-local is in $(basename "$PWD")'s production build graph:" \
    "the premise of the exceptions in .cargo/audit.toml (no private key outside KMS) no longer holds" >&2
  exit 1
fi
echo "production build graph: no local private-key signer"

"$audit" audit --file Cargo.lock
