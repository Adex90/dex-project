# automator — shielded batch relayer

Receives swap **intents** from retail users, batches them, has the `batch_swap` circuit proven, and submits ONE
`DarkPool.settleBatch` transaction. Written in Go, stdlib-only apart from the canonical Poseidon (`go-iden3-crypto`)
and `x/crypto` for Keccak.

```
 user wallet                     Automator (this service)                         chain
 ───────────                     ────────────────────────                         ─────
 1. JoinSplit proof  ──────────────────────────────────────────────────────────►  transferNote
    (own key): note → INTENT NOTE  [owner = unspendable BATCH key; terms in blinding]
 2. intent opening ──(Tor onion)──► POST /v1/intents ─► validate ─► queue (RAM only)
                                    every epoch (± jitter):
                                      sync note tree ◄──────────────── NoteInserted events
                                      batcher: choose feasible intents, uniform price, payouts, nets
                                      prover:  batch_swap Groth16 proof (private tmpfs, wiped)
                                      chain:   settleBatch(calldata) ─(Tor)─► signer gateway ─► tx
 3. scan NoteInserted, open sealed output (AES-GCM under destBlinding) ─────────  payout note
 4. JoinSplit proof (own key): withdrawNote ───────────────────────────────────►  tokens
```

**Trust:** the Automator learns each intent (it must, to prove it) but can *never* spend, redirect or keep a user's
funds — the circuit only pays `destPubKey` ≥ `minOut` before the deadline, or refunds `refundPubKey` after it. It
can censor; any user can then run `automator batch-witness`/`prove` over their own intent (a batch of one) or
simply wait for the deadline and be refunded.

## Packages

| package | role | verified by |
|---|---|---|
| `field`, `notetree` | BN254/Poseidon, incremental tree mirroring `NoteTree.sol` | golden vectors from the JS reference |
| `intent` | intent terms, note commitment/nullifier, sealed outputs | unit tests + golden |
| `batcher` | selection, closed-form clearing price, payouts, nets, witness | golden (entire witness), property test K'≥K, scenario tests |
| `prover` | `snarkjs` wrapper (private tmpfs, process-group kill, public-signal check) | fake-prover tests + **real proof settled on-chain** |
| `chain` | ABI encoder (== `cast`), JSON-RPC reader, remote-signer client | `cast calldata` golden |
| `privacy` | listener/log/HTTP/egress guarantees G1–G5 | `privacy_test.go` |
| `server`, `service` | HTTP intake, epoch loop | fake-chain end-to-end tests |

## API (onion only)
`POST /v1/intents` (JSON, ≤2 KiB, big integers as strings) → `202 {receipt}`; `GET /v1/intents/{receipt}` →
`{state: queued|inflight|settled|refunded|dropped, txHash}`; `GET /v1/info`; `GET /healthz`.
A note must exist in the tree before its intent is accepted — that is the anti-spam rule, because per-IP limits are
impossible by design.

## Privacy guarantees (and their limits) — see `deploy/README.md`
G1 no non-loopback listener · G2 no peer address or identifying header reaches any handler · G3 logs cannot contain
IPs, onion names, identifiers or fine timestamps · G4 all egress via Tor with per-destination isolation · G5 intents
in RAM only, no core dumps. **Not** hidden: that a user connects to Tor (from their ISP); traffic-timing correlation by
a global adversary; the intent *contents* from the Automator itself.

## Run
```bash
go test ./... && go vet ./...
go build -o automator ./cmd/automator
./automator serve --config deploy/config.example.json      # needs Tor + signer gateway, see deploy/README.md
./automator batch-witness --state state.json --out witness.json && ./automator prove --witness witness.json --snarkjs … --wasm … --zkey … --out proof.json
```
