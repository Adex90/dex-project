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
blocker #2. `ZkPriceFeed` (`contracts/src/oracle/ZkPriceFeed.sol`) is the production implementation.

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
`test_gasOfAPush`. Most of that is the Groth16 pairing check. At one push every 15 s, that is about 1.5 B gas a day:
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
"Contracts (Foundry; build, tests)" runs these suites with every other Foundry suite.

## Not in Phase 1

* **The relayer, notary and endpoint** come in Phase 2. Until then, the local deployment (`script/DeployLocal.s.sol`)
  keeps `DevLiveOracle`, because nothing would keep a `ZkPriceFeed` fresh for the local stack.
* **The engine's live reads** come in Phase 3. It will read this feed through `DarkPool.oracle()`.
* **The app's testnet disclaimer** comes in Phase 5, with the testnet build.
