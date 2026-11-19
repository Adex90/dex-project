#!/usr/bin/env bash
# Source-verifies every contract of a testnet deployment on the block explorer, from its manifest.
#
#   ETHERSCAN_API_KEY=… ARBITRUM_SEPOLIA_RPC_URL=… script/verify-testnet.sh [deployments/421614.json]
#
# `forge script script/DeployTestnet.s.sol … --verify` already verifies at deployment; this re-runs verification at
# any time (an explorer outage, a second explorer) and is idempotent: a verified contract is reported and skipped.
# * Etherscan's v2 API (one key for every chain) by default; VERIFIER=blockscout verifies on Arbitrum Sepolia's
#   Blockscout instead (no key).
# * Constructor arguments are recovered from each contract's creation transaction (--guess-constructor-args), read
#   through ARBITRUM_SEPOLIA_RPC_URL; the URL is passed by its foundry.toml alias and never printed.
# * The list of contracts is deployments/contracts.tsv. Poseidon has no Solidity source (circomlibjs bytecode):
#   CheckTestnet checks it against Poseidon(1, 2) instead.
set -euo pipefail

cd "$(dirname "$0")/.."
manifest="${1:-deployments/421614.json}"
chain=$(jq -r .chainId "$manifest")
if [ "$chain" != 421614 ]; then
  echo "verify-testnet: $manifest is for chain $chain, not Arbitrum Sepolia (421614)" >&2
  exit 2
fi
: "${ARBITRUM_SEPOLIA_RPC_URL:?set ARBITRUM_SEPOLIA_RPC_URL: the creation transactions are read through it}"

verifier=(--verifier etherscan)
case "${VERIFIER:-etherscan}" in
  etherscan) : "${ETHERSCAN_API_KEY:?set ETHERSCAN_API_KEY (Etherscan v2: one key for every chain)}" ;;
  blockscout) verifier=(--verifier blockscout --verifier-url https://arbitrum-sepolia.blockscout.com/api/) ;;
  *)
    echo "verify-testnet: VERIFIER is etherscan or blockscout" >&2
    exit 2
    ;;
esac

failed=0
while IFS=$'\t' read -r key source name; do
  case "$key" in '#'* | '') continue ;; esac
  address=$(jq -r --arg k "$key" '.contracts[$k]' "$manifest")
  if [[ ! "$address" =~ ^0x[0-9a-fA-F]{40}$ ]]; then
    echo "verify-testnet: the manifest has no address for $key" >&2
    failed=1
    continue
  fi
  echo "verify-testnet: $key $address ($source:$name)"
  if ! forge verify-contract --chain 421614 --rpc-url arbitrum_sepolia "${verifier[@]}" \
    --guess-constructor-args --watch "$address" "$source:$name"; then
    echo "verify-testnet: $key was NOT verified" >&2
    failed=1
  fi
done <deployments/contracts.tsv

echo "verify-testnet: poseidon $(jq -r .contracts.poseidon "$manifest") is circomlibjs bytecode with no Solidity source; CheckTestnet checks it"
exit "$failed"
