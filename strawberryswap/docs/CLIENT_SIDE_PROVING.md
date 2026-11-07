# Client-side proving and the shielded wallet

Phase 7 moves every secret-dependent computation of the private swap flow into the user's browser. This document
states what runs where, what is guaranteed by code, what is only *argued*, and what is still open.

## 1. Architecture

```
┌──────────────────────────── browser tab ────────────────────────────┐
│ UI thread (React hooks)                      Web Worker (prover.worker.ts)           │
│  useProver  ─ status/progress ◄────────────  artifact loader (fetch → SHA-256 → Cache) │
│  useShieldedWallet ─ signature ───────────►  ZkEngine ── keys, notes, intents, tree   │
│  usePrivateSwap ◄── public bundles ────────  witness builder → snarkjs Groth16 prover │
│        │ txs (wallet)      │ intent OPENING                                           │
└────────┼───────────────────┼─────────────────────────────────────────────────────────┘
         ▼                   ▼
     DarkPool           Automator (onion service)
```

* **Everything secret lives in the worker.** The UI thread sends the wallet signature once (`unlock`) and thereafter
  only receives public data: proofs, commitments, ciphertexts, note *summaries* and the intent *opening*.
* **The worker API is a typed contract** (`src/zk/protocol.ts`): a discriminated union of requests and a mapped
  response type, so the compiler checks every call site. Requests are processed one at a time.
* **Proving never blocks the UI**: downloading ≈11.5 MB, WASM witness generation and Groth16 proving all happen in the
  worker (`snarkjs` additionally spawns its own helper workers for multi-threaded MSM/FFT).
* **Static-export friendly**: the worker is emitted as a static chunk (`new Worker(new URL(...))`), the proving
  artifacts are copied into `public/zk/`, and their SHA-256 hashes are compiled into the bundle (see §3).

## 2. What never leaves the device (enforced + tested)

| Secret | Where it lives | Evidence |
|---|---|---|
| shielded private key | worker memory only; derived from a wallet signature | `engine.test.ts` (key absent from the opening); e2e: no request body contains the signature or any `privKey`/`witness` field |
| note openings, blindings, balances | worker memory (`ZkEngine`) | same |
| witness (all private circuit inputs) | built and consumed inside the worker | worker error messages strip long digit runs |
| trade parameters until the intent is sent | worker | e2e: the relay receives exactly one POST with the 7 opening fields |

What *does* leave, by design: proofs and public inputs (in transactions), the self-encrypted ciphertexts, and the
**intent opening** (terms: direction, amount, destination/refund *public* keys, `minOut`, deadline, output blinding).
The opening is sent to the Automator because the Automator must prove it; it contains no key. The Automator can therefore
see *this user's* intent (never their address — see `docs/` for the Tor layer), but cannot spend, redirect or keep the
funds (`docs/BATCH_SETTLEMENT.md` §1).

## 3. Integrity of the proving artifacts
`scripts/prepare-zk.mjs` copies `joinsplit.wasm`, `joinsplit.zkey` and `joinsplit.vkey.json` into `public/zk/` and writes
`src/zk/manifest.generated.json` (SHA-256 + size). The manifest is imported by the worker, so it is **part of the JS
bundle**: the site's content hash (its IPFS CID) commits to the exact keys. The worker:
1. downloads each file (streaming progress),
2. refuses it if its hash differs from the compiled-in value,
3. caches verified bytes in the Cache API and **re-verifies on every load**,
4. verifies every generated proof locally against the pinned verification key before returning it (so a bad proof never
   costs gas).
`scripts/verify-static.mjs` re-checks the exported hashes and that the browser bundle carries no Node-only module loading.

## 4. Key derivation and recovery
`privKey = keccak256(signature) mod p`, where `signature` is a wallet signature over a fixed message that includes the
chain id and pool address (`keyDerivationMessage`). Nothing is stored: reloading forgets everything, and signing again
re-derives the key. **Notes are recovered from the chain**: every note and intent this wallet creates carries an
AES-256-GCM ciphertext (key = SHA-256 of the private key, fixed length 189 B) in the `NoteInserted` event, and the
Automator's outputs are sealed under the intent's `destBlinding`, which the wallet re-learns from its own intent
ciphertext. A fresh session therefore restores balances, pending intents (so they can be resubmitted) and settled
outputs from the chain alone (`engine.test.ts: a fresh session recovers …`).

**Safety rule: the signature must be deterministic.** ECDSA with RFC 6979 (MetaMask, most software wallets, anvil) is
deterministic; a wallet that randomises signatures would derive a *different key every time* and strand any funds
shielded under the first one. Unlock therefore asks for the signature twice and refuses to proceed unless both are
identical — before any value is shielded.

## 5. The transaction flows
* **Deposit** `approve → proof → depositNote`: public amount and caller are bound into the proof (`extDataHash`).
* **Private swap** `check Automator → proof → transferNote → POST opening → poll`:
  1. `GET /v1/info` must report the batch key this app's circuits use; otherwise nothing is spent;
  2. the JoinSplit proof moves funds into an *intent note* owned by the unspendable batch key (+ change note);
  3. only after the transaction is mined is the opening POSTed (retrying while the relay syncs the chain);
  4. if step 3 fails the funds are **not lost**: the intent is recoverable from the chain and can be resubmitted, and after
     its deadline it can be refunded by any batch.
* **Withdraw** `proof → withdrawNote`: the recipient is bound into the proof so a relayer cannot redirect it.
A note used by an in-flight bundle is *reserved* (cannot be double-spent by a second click) and released if the
transaction fails.

## 6. Transport rules (`src/lib/automator.ts`, unit-tested)
https, or http only for `.onion` and loopback; no credentials, referrer, cache or redirects; no custom headers; the body
is only the opening; responses are size-capped and validated; a relay advertising a different batch key is refused.
**Use Tor Browser for `.onion` relays.** A normal browser cannot resolve them, and an HTTPS page calling an `http://*.onion`
URL is blocked as mixed content outside Tor Browser.

## 7. Measured (headless Chromium, local chain)
First load: ≈11.5 MB of artifacts (then cached). Deposit proof + tx ≈ 10 s; intent proof + tx + relay hand-over ≈ 21 s;
Go batch proof + settlement ≈ one epoch + ≈ 40 s. Browser, wallet, chain and Automator all ran in
`e2e/run.mjs`, including the final withdrawal.

## 8. Known limits (read before trusting it)
* **DEV proving keys** (single-party setup): a holder of that setup could forge proofs. The UI says so while the manifest
  has `devKeys: true`.
* One key per wallet; one input note per swap (no automatic merging); the fixed-length self-ciphertext (189 B) differs
  from the Automator's sealed output (61 B), so an observer can tell the two kinds of output apart.
* Deposit and withdrawal edges reveal amount and caller by construction.
* The wallet scans `NoteInserted` from the pool's deployment block on every unlock; for a busy pool this needs either a
  checkpoint or an indexer (an indexer would learn which tree the user is interested in, not which notes).
* Browser storage is intentionally not used for secrets. The cost is a rescan per session.
* Memory-safety of the browser (extensions with page access, a compromised dependency in the bundle) is outside this
  design; the pinned content hash and the absence of third-party origins reduce, but do not remove, that surface.
