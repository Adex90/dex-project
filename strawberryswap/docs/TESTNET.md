# Deploying the Arbitrum Sepolia testnet

This guide goes from an empty AWS account to:

* a verified testnet deployment;
* services that sign through KMS;
* a published frontend that discloses what it is.

Its last step is the 72-hour soak, `ops/soak/RUNBOOK.md`. CI rehearses everything but the AWS and explorer steps on
anvil, on every change: the `testnet-release` job.

> **Trusted bootstrap.**
>
> * Rothbard operates the ETHUSD price endpoint and the notary.
> * The zero-knowledge verifiers use **development keys** from a single-party setup.
> * tWETH and tUSD are faucet tokens with no value.
>
> The deployment says so on chain (`TestnetDisclosure`), and the frontend says so on every page (a banner that cannot
> be dismissed). The deployment script refuses any chain but Arbitrum Sepolia (421614) and anvil (31337).

## 1. Keys: KMS, one per role (`infra/terraform`)

```bash
cd infra/terraform/envs/prod
cp terraform.tfvars.example terraform.tfvars    # alert_email; deployer_principal_arns for a KMS deployer key
terraform init && terraform apply
terraform output signing_key_arns               # operator, relayer, automator, notary (and deployer)
```

`modules/signing-keys` creates one secp256k1 key (`ECC_SECG_P256K1`, `SIGN_VERIFY`) per signer, plus one IAM role per
key.

* Each key's policy is its only grant. Its role may `kms:Sign` digests (`ECDSA_SHA_256`, `DIGEST`) and read the
  public key.
* Administrators manage the keys but cannot sign.
* The bot host's instance role may assume only the four host signers' roles. Each signer runs as its own user, with an
  AWS profile that assumes its role.
* The bootstrap opens instance metadata to root and the signer users only. The clients (engine, relayer client,
  automator) get no AWS credentials.

The keys are generated inside KMS and cannot be exported. Each address comes from its public key:

```bash
for k in operator relayer automator notary deployer; do
  printf '%s ' "$k"; AWS_KMS_KEY_ID="alias/strawberryswap/$k" cast wallet address --aws
done
```

This needs only `kms:GetPublicKey`, which administrators hold. Without a deployer key, deploy from a hardware wallet
instead (`--ledger`).

## 2. The feed's constants (`oracle/`)

On the relayer's host, as the relayer's user, create its secret query salt and derive the feed's constants:

```bash
cargo run --release --bin query-commitment -- --salt-file /etc/strawberryswap/relayer.salt --new-salt \
    --provider-label rothbard/ethusd-endpoint/testnet
# QUERY_COMMITMENT=0x…   PROVIDER_ID=0x…
```

* The salt file is created mode 0600 and never overwritten: a new salt would orphan the deployed feed.
* The salt is never printed. Only its commitment, `Poseidon(pack("ETHUSD"), salt)`, goes on chain.
* `PROVIDER_ID` is the notary's key in `ZkDecoOracle`, a field element derived from the label.

## 3. Deploy (`contracts/`)

```bash
export OPERATOR_ADDRESS=0x… NOTARY_ADDRESS=0x… RELAYER_ADDRESS=0x… AUTOMATOR_ADDRESS=0x…   # step 1
export PROVIDER_ID=0x… QUERY_COMMITMENT=0x…                                                  # step 2
export INITIAL_PRICE=$(cast to-wei 3456.78)        # tUSD per tWETH now: the price endpoint's median
export BOT_GAS_WEI=$(cast to-wei 0.05)             # testnet gas for each bot, from the deployer
export ARBITRUM_SEPOLIA_RPC_URL=… ETHERSCAN_API_KEY=…   # read from the environment, never printed
AWS_KMS_KEY_ID=alias/strawberryswap/deployer \
  forge script script/DeployTestnet.s.sol --rpc-url arbitrum_sepolia --aws --sender <deployer address> \
  --broadcast --verify --slow
```

The script deploys, in one broadcast:

* tWETH and tUSD. Each is faucet-capped: 1 tWETH or 5,000 tUSD a day per address, with global caps. tWETH is token0,
  through the deployer's nonce order.
* The dev-key verifiers behind their adapters.
* Poseidon, checked against `Poseidon(1, 2)`.
* `ZkDecoOracle`, with the notary's address.
* `ZkPriceFeed`.
* A minimal `Treasury`.
* `DarkPool` at `INITIAL_PRICE`.
* `SearcherExecutor`, bound to the operator's KMS address.
* `TestnetDisclosure`.

It then seeds the pool's liquidity and the executor's inventory, sends each bot its gas, checks every wiring, and
writes `deployments/421614.json`. Commit that manifest.

```bash
forge script script/DeployTestnet.s.sol:CheckTestnet --rpc-url arbitrum_sepolia   # any time later, read-only
script/verify-testnet.sh                       # re-verify on the explorer (idempotent); VERIFIER=blockscout too
```

Every contract but Poseidon is source-verified. Poseidon is circomlibjs bytecode with no Solidity source, and
CheckTestnet checks it against the circuits' hash.

## 4. Services

Each service runs under systemd as its own user. The table names the manifest address each one needs:

| service | key | its configuration from the manifest |
| --- | --- | --- |
| `threshold-signer --role operator` | operator KMS key | `--executor <executor>`, `--chain-id 421614` |
| `threshold-signer --role relayer` | relayer KMS key | `--feed <feed>` |
| `threshold-signer --role automator` | automator KMS key | `--pool <pool>` |
| `notary` | notary KMS key | `--provider-id <PROVIDER_ID>` |
| `relayer` | none: its vault signs | `--feed <feed> --salt-file /etc/strawberryswap/relayer.salt --vault-socket …` |
| `hft-engine` | none: its vault signs | `--rpc-url http://127.0.0.1:8547` (the failover proxy), `--feed <feed> --executor <executor> --chain-id 421614 --target-token0 20 --calibration /etc/strawberryswap/calibration.json --json --metrics-every 10 --log-txs` |
| `automator` | none: its vault signs | `pool`, `vaultSocket`; proving keys from `bots/automator/zk/` |
| `rpc_failover.py` | none (the provider URLs, mode 0600) | `--upstreams /etc/strawberryswap/rpc-upstreams --drills` |

The batch proving key the automator uses is committed with its checksums, `bots/automator/zk/`. It matches the
deployed batch verifier.

## 5. Frontend, and the release on IPFS

```bash
cd frontend
node scripts/env-from-manifest.mjs ../contracts/deployments/421614.json --out .env.production.local
NEXT_BUILD_ID=$(git rev-parse --short=12 HEAD) npm run build && npm run verify:static
cd .. && ops/release/publish-ipfs.sh --manifest contracts/deployments/421614.json --site frontend/out \
    --version v0.5.0-testnet.1 --record releases/421614-v0.5.0-testnet.1.json
```

* The build configuration comes from the manifest:
  * chain 421614;
  * Arbitrum's public RPC (it ships in the bundle, so it must never be a URL with a key);
  * the pool;
  * the deployment block;
  * the explorer;
  * the disclosure;
  * the faucets.
* Every page opens with the trusted-bootstrap banner, and a TESTNET badge stays in the header.
* The site is reproducible: the same commit and manifest give the same IPFS CID.
* `publish-ipfs.sh` adds two directories:
  * the site;
  * the artifacts: the manifest, every deployed contract's compiler artifact, the three dev proving keys and
    `SHA256SUMS`.
* It pins both on the local kubo node, and on the remote pinning service named in `IPFS_PIN_SERVICE` (the node holds
  that key). It copies the release record to the metadata bucket (`METADATA_BUCKET`).
* It refuses a site built for another deployment, a non-test chain, or a proving key that does not match its pinned
  hash. `--dry-run` computes the CIDs without a node.

## 6. Then the soak

Follow `ops/soak/RUNBOOK.md`:

* the preflight checklist;
* 72 hours under telemetry;
* five chaos drills (vault down, stale feed, RPC failover, chain reorg, competing searcher);
* the service levels;
* the comparison with the calibration lab at γ = 0.0041.
