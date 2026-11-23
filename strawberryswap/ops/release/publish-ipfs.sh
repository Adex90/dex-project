#!/usr/bin/env bash
# Publishes a StrawberrySwap testnet release to IPFS as two content-addressed directories, and records both CIDs:
#
#   site/       the static frontend (frontend/out), built for this deployment (scripts/env-from-manifest.mjs);
#               served from the root of its CID (a subdomain gateway, ipfs://, DNSLink)
#   artifacts/  what the deployment is built from: its manifest, the compiler artifact (ABI, creation and runtime
#               bytecode, metadata) of every deployed contract, the three dev proving keys, and SHA256SUMS
#
#   ops/release/publish-ipfs.sh --manifest contracts/deployments/421614.json --site frontend/out \
#       [--version v0.5.0-testnet.1] [--contracts-out contracts/out] [--record FILE] [--dry-run]
#
# * --dry-run computes the CIDs only (`ipfs add --only-hash` in a throwaway repo): no daemon, no network.
# * Otherwise the local kubo node (its IPFS_PATH or API) adds and pins both directories. When IPFS_PIN_SERVICE names
#   a remote pinning service configured on that node (`ipfs pin remote service add NAME ENDPOINT KEY`: the node
#   keeps the key, never this script or the repository), both CIDs are pinned there too. When METADATA_BUCKET is set,
#   the record is copied to s3://$METADATA_BUCKET/releases/ (infra/terraform/modules/s3-ipfs-pin).
# * The CIDs are reproducible: CIDv1, raw leaves, a fixed chunker, no file modes or times. Rebuild the site from the
#   same commit and manifest (NEXT_BUILD_ID pins Next's build id) and the same CIDs come out.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd) # strawberryswap/
ADD_OPTS=(--recursive --quieter --cid-version=1 --raw-leaves --chunker=size-262144 --hash=sha2-256)

die() {
  echo "publish-ipfs: $*" >&2
  exit 1
}

manifest="" site="" version="" record="" dry_run=0
contracts_out="$root/contracts/out"
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest) manifest=${2:?}; shift 2 ;;
    --site) site=${2:?}; shift 2 ;;
    --version) version=${2:?}; shift 2 ;;
    --contracts-out) contracts_out=${2:?}; shift 2 ;;
    --record) record=${2:?}; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    -h | --help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done
[ -n "$manifest" ] && [ -n "$site" ] || die "--manifest and --site are required (see --help)"
command -v ipfs >/dev/null || die "the kubo CLI (ipfs) is not installed"
command -v jq >/dev/null || die "jq is not installed"

# ---------------------------------------------------------------------------------------------- the deployment
chain=$(jq -r .chainId "$manifest")
case "$chain" in
  421614) network=arbitrum-sepolia ;;
  31337) network=anvil ;;
  *) die "$manifest is for chain $chain: only Arbitrum Sepolia (421614) and anvil (31337) releases are published here" ;;
esac
[ "$(jq -r .devKeyVerifiers "$manifest")" = true ] || die "$manifest does not flag its dev-key verifiers"
pool=$(jq -r .contracts.pool "$manifest")
[[ "$pool" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "$manifest has no pool address"
version=${version:-$(git -C "$root" describe --tags --always --dirty 2>/dev/null || echo unversioned)}
[[ "$version" =~ ^[A-Za-z0-9._+-]+$ ]] || die "--version: letters, digits and . _ + - only"
commit=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)

# ---------------------------------------------------------------------------------------------- the site
[ -f "$site/index.html" ] || die "$site/index.html is missing: build the frontend first (npm run build)"
# The bundle inlines its configuration: it must have been built for THIS deployment, as a testnet build.
grep -rqiF -- "$pool" "$site/_next" || die "$site was not built for this deployment (the pool $pool is not in its bundle; see frontend/scripts/env-from-manifest.mjs)"
grep -qF 'data-testid="testnet-banner"' "$site/index.html" || die "$site/index.html has no testnet banner: not a testnet build"

# ---------------------------------------------------------------------------------------------- the artifacts
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
art="$stage/artifacts"
mkdir -p "$art/contracts" "$art/zk"
cp "$manifest" "$art/deployment.json"

while IFS=$'\t' read -r key source name; do
  case "$key" in '#'* | '') continue ;; esac
  file="$contracts_out/$(basename "$source")/$name.json"
  [ -f "$file" ] || die "$file is missing: run forge build in contracts/ (or pass --contracts-out)"
  address=$(jq -r --arg k "$key" '.contracts[$k]' "$manifest")
  [[ "$address" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "$manifest has no address for $key"
  jq --sort-keys --arg key "$key" --arg address "$address" --arg source "$source" --arg name "$name" \
    '{key: $key, address: $address, source: $source, contractName: $name, abi: .abi,
      bytecode: .bytecode.object, deployedBytecode: .deployedBytecode.object, metadata: .rawMetadata}' \
    "$file" >"$art/contracts/$key.json"
done <"$root/contracts/deployments/contracts.tsv"

# The proving keys of the three dev-key verifiers, each set checked against its committed SHA256SUMS first.
for set in "frontend/public/zk:joinsplit" "bots/automator/zk:batch_swap_n4" "oracle/zk:zkdeco_oracle"; do
  dir=${set%%:*} circuit=${set##*:}
  mkdir -p "$art/zk/$circuit"
  if [ "$circuit" = joinsplit ]; then
    # The browser's key is pinned by the bundle itself (frontend/src/zk/manifest.generated.json).
    jq -r '.artifacts | to_entries[] | "\(.value.sha256)  \(.value.path | sub("^zk/"; ""))"' \
      "$root/frontend/src/zk/manifest.generated.json" | (cd "$root/$dir" && sha256sum --check --quiet --strict -) ||
      die "$dir does not match frontend/src/zk/manifest.generated.json"
  else
    (cd "$root/$dir" && sha256sum --check --quiet --strict SHA256SUMS) || die "$dir does not match its SHA256SUMS"
  fi
  for f in "$root/$dir"/*.wasm "$root/$dir"/*.zkey "$root/$dir"/*.vkey.json; do
    [ -f "$f" ] || die "$dir: no $(basename "$f")"
    cp "$f" "$art/zk/$circuit/"
  done
done
(cd "$art" && find . -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) >"$stage/SHA256SUMS"
mv "$stage/SHA256SUMS" "$art/SHA256SUMS"

# ---------------------------------------------------------------------------------------------- add (and pin)
if [ "$dry_run" = 1 ]; then
  IPFS_PATH="$stage/repo"
  export IPFS_PATH
  ipfs init --profile=test --empty-repo >/dev/null 2>&1 || die "ipfs init failed"
  ADD_OPTS+=(--only-hash)
fi
site_cid=$(ipfs add "${ADD_OPTS[@]}" "$site")
artifacts_cid=$(ipfs add "${ADD_OPTS[@]}" "$art")
[ -n "$site_cid" ] && [ -n "$artifacts_cid" ] || die "ipfs add returned no CID"

remote=null
if [ "$dry_run" = 0 ] && [ -n "${IPFS_PIN_SERVICE:-}" ]; then
  for pair in "site:$site_cid" "artifacts:$artifacts_cid"; do
    ipfs pin remote add --service="$IPFS_PIN_SERVICE" --name="strawberryswap-$network-$version-${pair%%:*}" --background "${pair#*:}" >/dev/null
  done
  remote="\"$IPFS_PIN_SERVICE\""
fi

# ---------------------------------------------------------------------------------------------- the record
record_json=$(jq -n --sort-keys \
  --arg version "$version" --arg network "$network" --argjson chainId "$chain" --arg commit "$commit" \
  --arg manifestSha256 "$(sha256sum "$manifest" | cut -d' ' -f1)" --arg pool "$pool" \
  --arg site "$site_cid" --arg artifacts "$artifacts_cid" \
  --arg sums "$(sha256sum "$art/SHA256SUMS" | cut -d' ' -f1)" \
  --arg kubo "$(ipfs version --number)" --arg addOptions "${ADD_OPTS[*]}" \
  --argjson dryRun "$([ "$dry_run" = 1 ] && echo true || echo false)" --argjson remote "$remote" \
  '{version: $version, network: $network, chainId: $chainId, gitCommit: $commit, manifestSha256: $manifestSha256,
    pool: $pool, site: {cid: $site, uri: ("ipfs://" + $site)},
    artifacts: {cid: $artifacts, uri: ("ipfs://" + $artifacts), sha256sumsSha256: $sums},
    pinned: {local: ($dryRun | not), remoteService: $remote}, dryRun: $dryRun, kubo: $kubo, addOptions: $addOptions}')
if [ -n "$record" ]; then
  printf '%s\n' "$record_json" >"$record"
  echo "publish-ipfs: wrote $record" >&2
fi
if [ "$dry_run" = 0 ] && [ -n "${METADATA_BUCKET:-}" ]; then
  printf '%s\n' "$record_json" | aws s3 cp - "s3://$METADATA_BUCKET/releases/$network/$version.json" --content-type application/json >/dev/null
  echo "publish-ipfs: copied the record to s3://$METADATA_BUCKET/releases/$network/$version.json" >&2
fi
printf '%s\n' "$record_json"
