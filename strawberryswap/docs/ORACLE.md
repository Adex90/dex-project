# The zk price feed

> **⚠ Testnet is a TRUSTED BOOTSTRAP.**
>
> On testnet, Rothbard Tecnologías Financieras operates both the price endpoint and the notary that vouches for it.
> The proof verifier uses single-party DEV keys. **Testnet prices are therefore only as trustworthy as the operator.**
> The zero-knowledge proof shows that the price came from the endpoint's response. It does not make the endpoint or
> the notary honest.
>
> This disclosure must appear wherever testnet prices are shown: this document, the contracts' NatSpec, and the app's
> testnet build. It stays until independent providers and a decentralized notary are in place.

`DarkPool` takes its price from an `IZKOracle`, fixed at deployment, and refuses any price older than 60 s or stamped
in the future. Until Phase 1, the only `IZKOracle` implementations were test mocks; the README listed this as mainnet
blocker #2. `ZkPriceFeed` (`contracts/src/oracle/ZkPriceFeed.sol`) is the production implementation. Phase 2 adds the
off-chain pipeline that keeps it fresh: the price endpoint, the notary and the relayer (`oracle/`, below).

## How a price reaches the pool

```text
price endpoint ──TLS──► notary (signs the body's commitment)
       │
       ▼
relayer: Groth16 proof of circuits/zkdeco_oracle ──submit──► ZkPriceFeed ──latestVerifiedPrice──► DarkPool
                                                                │
                                                     ZkDecoOracle.verifyAttestation
                                                     (proof + registered notary)
```

1. **The endpoint** serves the circuit's canonical object, `{"symbol":"ETHUSD","price":3456.789012,"ts":1767225600}`,
   which must fit in 96 bytes.
2. **The notary** signs `(providerId, bodyCommitment)` for that response.
3. **The relayer** proves that the committed body contains the hidden instrument, the price and the timestamp. Proving
   takes about a second for 7,164 constraints. The relayer then calls `submit`. Relaying is permissionless: the relayer
   pays the gas and is trusted for nothing.
4. **`ZkPriceFeed`** checks the attestation and keeps the newest one.
5. **`DarkPool`** reads it on every rebalance and every value-moving batch.

## The off-chain pipeline (Phase 2)

`oracle/` is one Rust crate, `zk-oracle`, with three services. The notary's key and the relayer's gas key live in
AWS KMS: the processes hold a key id, never key material.

```text
Coinbase ┐
Kraken   ├─► price-endpoint ──HTTPS──► notary ──► relayer ──EIP-1559, KMS-signed──► ZkPriceFeed.submit
Bitstamp ┘   GET /v1/ETHUSD           POST /v1/attest    Groth16 proof (snarkjs)
             (fail closed)            KMS signs the       preflight eth_call
                                      body's commitment   every ~15 s
```

### price-endpoint: the data provider

* **Sources.** It polls the public ETH-USD tickers of Coinbase, Kraken and Bitstamp every 2 s. Their prices are
  strings, parsed exactly: no floating point anywhere.
* **The rule.** Quotes received in the last 10 s count. The endpoint takes their median, drops any quote more than
  1 % away from it, and needs **two** quotes left. The price is the median of those.
* **Fail closed.** With fewer than two agreeing quotes, `GET /v1/ETHUSD` answers 503: no price, so the notary attests
  nothing, the feed ages out, and DarkPool stops trading on it. A stale feed stops the pool; a wrong price would be
  traded against.
* **The body.** Otherwise it serves the circuit's canonical object, stamped with its clock:
  `{"symbol":"ETHUSD","price":3456.78,"ts":1767225600}`. Digits are canonical: no leading or trailing zeros, at
  most 10 decimals. It sends `Cache-Control: no-store`, and TLS is terminated in front of it.

### notary: the trust anchor

On `POST /v1/attest`:

1. **It fetches the body itself**, over HTTPS, without following redirects, at most 96 bytes. A relayer cannot hand
   it a body to sign. This is model (a) of the circuit: the notary vouches that the body came from the provider.
2. **It checks** the body against the circuit's grammar, byte for byte. It must be for the configured instrument and
   stamped within 5 s before, or 2 s after, the notary's clock.
3. **It commits and signs.** It computes the body's Poseidon commitment and has KMS sign
   `ZkDecoOracle.notaryDigest(providerId, bodyCommitment)`.
4. **It self-checks.** It encodes the signature as the contract wants it (low s, `v` of 27 or 28) and recovers its
   own address from it before handing it out.

`GET /v1/info` names the notary's address, which `ZkDecoOracle` must have registered for the provider. A request within
a second of the last attestation gets that attestation again, so KMS signs at most once a second however often the
notary is asked.

### relayer: trusted for nothing

Every 15 s, one cycle:

1. **Attest.** It asks the notary for an attestation.
2. **Check.** It checks the attestation as the contract will: the provider, the canonical body, the commitment, and
   a low-s signature by the notary registered on chain.
3. **Skip what is not new.** If the feed already holds this observation or a newer one, the cycle ends here.
4. **Prove.** `circuits/scripts/prove-zkdeco.mjs` proves the statement in a child process, in about a second.
   * The witness holds the query salt. It goes to the child on stdin only, never to disk or the command line.
   * The child gets an empty environment and a deadline.
   * The child verifies the proof against the verification key. The relayer then checks its five public signals
     against the attestation.
5. **Preflight.** `submit` runs as an `eth_call` first. If the chain's clock trails the observation's stamp
   (`FromTheFuture`), the relayer waits for it, up to 4 s. Any other refusal ends the cycle without a transaction.
6. **Push.** It sends an EIP-1559 transaction signed by its KMS key, bound to the chain id. The caps:

   | cap | default |
   |---|---|
   | max fee | 2 gwei |
   | priority fee | 0.1 gwei |
   | gas | 2 M, which covers Arbitrum's L1 data cost on top of ~260 k execution gas |

   Above a cap, it does not push.

**At start**, the relayer checks three things and refuses to run on any mismatch:

* the salt and symbol commit to the feed's `queryCommitment`;
* the notary answering is the one `ZkDecoOracle` registered for the feed's provider;
* the RPC's chain id is the one given.

Each cycle prints one line: `PUSHED`, `NOTHING_NEW`, `REFUSED` or `FAILED`.

### Keys and custody (testnet)

| key | where | permissions | what a compromise allows |
|---|---|---|---|
| notary | KMS, `ECC_SECG_P256K1`, `SIGN_VERIFY` | the notary's role: `kms:GetPublicKey`, `kms:Sign` on this key only | attesting any price within the feed's bounds, until a new oracle and feed are deployed (the registry is immutable) |
| relayer gas key | KMS, same spec | the relayer's role, on its key only | spending its gas balance; it cannot push anything the feed would refuse |
| query salt | a file, mode 0600 | read by the relayer | revealing the instrument behind `queryCommitment` |

* **Low s.** KMS returns DER signatures, which may have a high s. alloy's `AwsSigner` normalizes them to low s and
  recovers the parity from the key's public key.
* **Audit trail.** Every signature is a KMS call, logged by CloudTrail.
* **Interim custody.** KMS is the testnet's interim custody (decision 3). A threshold (MPC) signer replaces it before
  mainnet, behind the same `Signer` trait.
* **Never on the command line.** No service takes a key, or the salt, on its command line or from its environment.
  The relayer never shows its RPC URL, which may hold an API key: not at start, and not in its errors, which are
  redacted.

### Proving artifacts

* **Committed, not rebuilt.** `oracle/zk/` holds the circuit's wasm, the DEV proving key and its verification key,
  with `SHA256SUMS`. The setup draws fresh randomness, so a rebuilt key would not match the committed verifier.
* **Checked against the verifier.** `tests/zk_artifacts.rs` checks every constant of `OracleGroth16Verifier.dev.sol`
  against the verification key, and CI checks the hashes.
* **DEV keys.** Like every verifier today, mainnet needs keys from a multi-party ceremony.

### Running it

```bash
# On the endpoint's host (TLS in front of 127.0.0.1:8080)
price-endpoint --listen 127.0.0.1:8080

# On the notary's host: AWS credentials from the instance role
notary --provider-url https://<endpoint host>/v1/ETHUSD --provider-id 0x<providerId> \
       --kms-key-id <notary key ARN> --kms-region <region>

# On the relayer's host
relayer --rpc-url <Arbitrum Sepolia RPC> --chain-id 421614 --feed 0x<ZkPriceFeed> \
        --notary-url https://<notary host> --salt-file /etc/zk-oracle/ethusd.salt \
        --kms-key-id <relayer key ARN> --kms-region <region>
```

Each binary documents its flags with `--help` and prints the trusted-bootstrap disclosure when it starts.

## The rules `submit` enforces

Cheap checks come first and the proof last. A refused push changes nothing.

| rule | refused with |
|---|---|
| Only the configured provider is accepted. | `WrongProvider` |
| Only the configured instrument (its `queryCommitment`) is accepted. A valid XAUUSD proof cannot be pushed to an ETHUSD feed. | `WrongInstrument` |
| The observation must be strictly newer than the stored one. There are no replays and no going back. | `NotNewer` |
| The observation must not be in the future. DarkPool would treat a future stamp as stale and stop until the chain caught up. | `FromTheFuture` |
| The observation must be at most `maxSubmissionDelay` old when pushed (at most 60 s; 30 s on testnet). A relayer cannot hold attestations back and pick the price it prefers. | `TooOld` |
| `minPrice <= price <= maxPrice <= 1e24` (DarkPool's `MAX_PRICE`). This is a sanity range against unit or parsing errors, not a substitute for honesty. | `PriceOutOfBounds` |
| The attestation must verify in `ZkDecoOracle`: the Groth16 proof, plus the registered notary's signature. The call is a STATICCALL and cannot re-enter. | `InvalidAttestation` |

**Units.** The circuit attests the price of one whole token0 in token1, scaled by 1e18. DarkPool prices raw units, but
it only accepts 18-decimal tokens (`BadDecimals`), so the attested value *is* the pool's price. For the same reason,
the testnet pool needs an **18-decimal USD stablecoin**. Circle's test USDC has 6 decimals, so we will deploy an
18-decimal test USD.

**Freshness.** `latestVerifiedPrice()` returns the **observation** time, so DarkPool's 60 s window measures how old the
data really is. Before the first push it returns `(0, 0)`, which DarkPool treats as stale. Keeping the feed fresh is
the relayers' job. On testnet, a relayer pushes every ~15 s, and anyone can run another.

**Cost.** A push costs the relayer **259,974 gas**: the first push, into a cold storage slot, as measured in CI by
`test_gasOfAPush`. Most of that is the Groth16 pairing check. As whole transactions (base cost and calldata included),
the pipeline's end-to-end test measured 284,694 gas for the first push and 267,608 for each one after it; proving took
0.78 s, and a full cycle 1.85 s. At one push every 15 s, that is about 1.5 B gas a day:
affordable on an L2 such as Arbitrum, and not on Ethereum mainnet.

## Trust model

A price is as honest as:

1. **The data provider.** On testnet, our endpoint.
2. **The provider's notary**, registered immutably in `ZkDecoOracle`. A compromised notary key can attest any price
   within the feed's bounds for its provider. Until in-circuit TLS verification exists, the notary is the trust anchor.
3. **The circuit and its trusted setup.** Today these are DEV keys from a single party.
4. **The chain's clock.** Freshness is measured against `block.timestamp`. On Arbitrum the sequencer sets it, and the
   protocol only keeps it within hours of L1 time. So the feed's windows, and DarkPool's own 60 s rule, assume an honest
   sequencer clock. A sequencer that backdated blocks could make a stale price look fresh.

A wrong price is not harmless. `flashRebalance` moves the pool toward the oracle, so a false price lets a searcher move
the pool to it and keep the difference at true prices. That is why the price source is a mainnet blocker, not a detail.

### Before mainnet

* **Independent providers.** Each has its own pinned response template. The feed takes a median across k of n.
* **A decentralized notary.** A threshold (TSS) notary key, and in-circuit TLS verification eventually, which removes
  the notary. `IZKDecoOracle`'s public inputs are designed so that this swap needs no contract change.
* **A multi-party trusted-setup ceremony** for every circuit (audit item C-0).
* **An external audit.**

## Testnet configuration (decided)

| | |
|---|---|
| Chain | Arbitrum Sepolia: sub-second blocks, first-come-first-served sequencing |
| Market | WETH / 18-decimal test USD, priced by attested ETHUSD |
| Heartbeat | a push every ~15 s, decoupled from rebalances |
| `maxSubmissionDelay` | 30 s (inside the engine's 45 s and the pool's 60 s windows) |
| Endpoint and notary | operated by Rothbard: the trusted bootstrap disclosed above |
| Operator key | AWS KMS, interim. A threshold (MPC) vault replaces it before mainnet. |

## Tests

| suite | what it shows |
|---|---|
| `test/ZkPriceFeed.t.sol` | Every rule above, one by one, against a verifier the test controls. The verifier is called with STATICCALL. A fuzzed sequence of pushes is accepted exactly when the rules allow, and the stored time never goes back. |
| `test/ZkPriceFeedIntegration.t.sol` | Real Groth16 proofs of an ETHUSD series 15 s apart, against the DEV verifier and a test notary, read by a real DarkPool. Freshness ages out at 60 s from the observation. The series advances and replays are refused. Tampered price, time, body commitment, signer or length are refused, as is another provider's genuine attestation. A flash rebalance lands the pool on the attested price and pays the Treasury 80 % of the harvest. |

`test/fixtures/FeedFixtures.sol` is generated by `circuits/scripts/gen-feed-fixtures.mjs`. Every proof is verified
locally before it is written, and the proving key matches the committed `OracleGroth16Verifier.dev.sol`. The CI job
"Contracts (Foundry; build, tests)" runs these suites with every other Foundry suite. `test/NotaryVector.t.sol` pins
`notaryDigest` to the vector the Rust notary is tested against.

The pipeline's suites (`oracle/`, CI job "Oracle pipeline"):

| suite | what it shows |
|---|---|
| unit (`src/`) | **The circuit's grammar.** The canonical body matches the fixture generator. Poseidon commitments match circomlibjs on the ETHUSD and XAUUSD fixtures. The notary digest matches the contract's. **Aggregation.** The median, outlier and quorum rule. **The command line.** Flags, and the salt file's mode. |
| `tests/kms.rs` | **AWS KMS, through the production code path.** A stand-in speaks KMS's wire protocol and signs with a test key, so alloy's `AwsSigner` runs as in production. Signatures come out low s even when KMS answers high s. Transactions are signed for the relayer's chain only. |
| `tests/endpoint.rs` | **The endpoint over stand-in exchanges.** The median, two of three, an outlier dropped. It fails closed with one source, a split, or malformed tickers, and recovers. |
| `tests/notary.rs` | **The notary over KMS.** It signs only a fresh canonical body of its instrument. It refuses stale or future stamps, another symbol, padding, a non-canonical price, reordered keys, oversize bodies, errors and redirects, and KMS sees none of these. It signs at most once a second under load, and it needs https. |
| `tests/relayer.rs` | **The relayer with the real notary and the real prover.** The pushed attestation and 353-byte proof are exactly what the feed checks. It refuses an unregistered notary, a wrong salt, and a feed preflight refusal, all without a transaction. It waits for a lagging chain clock. It runs on its heartbeat and stops on shutdown. Its errors never show the RPC URL's key. The prover rejects other public signals, a bad witness and a missed deadline. |
| `tests/zk_artifacts.rs` | **The committed verification key is the deployed verifier's.** All 26 constants. |
| `tests/e2e_anvil.rs` | **End to end, in CI.** The three binaries run as processes against stand-in exchanges and KMS. The dev verifier, `ZkDecoOracle` and `ZkPriceFeed` are deployed on anvil. Four cycles push proven, KMS-signed prices, and when the market moves to 3,500 the feed follows. Every `PriceUpdated` comes from the relayer's KMS address. A relayer with the wrong salt refuses to start. |

## Not done yet

* **The local stack.** The local deployment (`script/DeployLocal.s.sol`) keeps `DevLiveOracle`. The pipeline runs
  against `ZkPriceFeed` in the anvil end-to-end test, but no deploy script wires them together yet.
* **The engine's live reads** come in Phase 3. It will read this feed through `DarkPool.oracle()`.
* **The testnet deployment** comes in Phase 5: the KMS keys, the hosts, the 18-decimal test USD, and the app's testnet
  disclaimer.
