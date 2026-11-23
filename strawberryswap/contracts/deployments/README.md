# Deployment manifests

`script/DeployTestnet.s.sol` writes one manifest per chain here: `<chainId>.json`, with every contract's address, the
deployment block and the inputs it was built from. `421614.json` is the Arbitrum Sepolia testnet's, committed after
each deployment; the frontend's configuration (`frontend/scripts/env-from-manifest.mjs`), the services' and the IPFS
release record are derived from it. Manifests of local runs (`31337.json`) are not committed.

`script/DeployTestnet.s.sol`'s `CheckTestnet` re-checks a deployment from its manifest at any time, read-only.

`contracts.tsv` lists the deployed contracts with their sources: `script/verify-testnet.sh` source-verifies each on the
explorer from a manifest, and `ops/release/publish-ipfs.sh` publishes each one's compiler artifact with the release.
