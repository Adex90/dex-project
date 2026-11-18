# threshold-signer

The vault that signs StrawberrySwap's transactions on the testnet. This is **iteration 1** (Phase 4).

* **One role per process.** A vault holds one role's key and signs one function on one contract.
* **Its key is in AWS KMS.** It is a secp256k1 key generated inside KMS's HSMs. It is never exported, so the vault
  holds no key material. The ephemeral in-memory key of iteration 0 is gone.
* **Chain-state rules.** The vault reads the chain to sequence its nonces, to check that only it spends them, and to
  cap what it signs per window.

The clients hold no key and no KMS permission. They build transactions and ask over a local Unix socket.

```bash
cargo test                                   # 38 tests, offline (a local KMS stand-in and a mock chain)
cargo test --features insecure-test-key      # 39: also the test-key loader
cargo run --release -- --role operator --chain-id 421614 --rpc-url "$RPC" --kms-key-id "$OPERATOR_KEY" \
  --executor 0x<SearcherExecutor> --max-fee-eth-per-window 0.5 \
  --max-volume0-per-window 50e18 --max-volume1-per-window 175000e18
```

## Roles

| role | client | contract | the one function it signs | what the calldata must hold |
|---|---|---|---|---|
| **operator** | HFT engine (`bots/hft-engine-rust`) | SearcherExecutor | `rebalance(uint256,uint256,uint256,uint256,uint256)` `0xc0661737`; never `sweep` | five words < 2¹²⁸, at least one output |
| **relayer** | oracle relayer (`oracle`, `relayer`) | ZkPriceFeed | `submit((bytes32,bytes32,uint256,uint64),bytes)` | the feed's own provider and instrument, a price within its bounds, a non-zero time, the 353-byte proof wire format |
| **automator** | batch automator (`bots/automator`) | DarkPool | `settleBatch(Batch)` `0x25a5b941` | 1 to `batchSize` notes; nullifiers, commitments and ciphertexts of one length; no zero nullifier or commitment; a 256-byte proof; ciphertexts ≤ 256 bytes; flows < 2¹²⁸ |

A request for another role's function is refused as such, wherever it is addressed. For example, `submit is the
relayer role's function: this vault holds the operator key`. Calldata is decoded strictly: canonical offsets and
padding, no trailing bytes. It is then re-encoded and compared byte for byte.

```text
 HFT engine ──┐ operator.sock    ┌─ operator vault  ── KMS key 1 (the executor's operator) ──┐
 relayer   ───┼ relayer.sock   ──┼─ relayer vault   ── KMS key 2                              ├─► AWS KMS (HSMs)
 automator ───┘ automator.sock   └─ automator vault ── KMS key 3                              ┘
   no key, no KMS permission        policy + chain-state rules; no key material
```

## Every role's rules

| rule | refused otherwise |
|---|---|
| EIP-1559 (type 2) only, canonical RLP, nothing after it | legacy, 2930, 4844 (blobs), 7702 (code delegation) |
| the configured chain id | replay on another chain |
| destination = the role's contract; no contract creation | calls to any other contract |
| value 0, empty access list | ether transfers |
| the role's function only | every other function, the other roles' included |
| gas limit ≤ cap, max fee ≤ cap, priority fee ≤ max fee | fee-burning transactions |
| at most N signatures per second (default 20) | floods |
| the nonce rule and the window's caps (below) | stockpiled signatures, a compromised client's volume |

## Chain-state rules

**Nonce sequencing.** A signature is worth something only for a nonce the chain has not used.

* The vault signs a nonce from the chain's next one (`eth_getTransactionCount` at `latest`) up to
  `--max-pending` − 1 beyond it (default 1: one transaction in flight).
* It never signs past the first nonce it has not signed.
* So no signature can be held back for later: by the time a stockpiled transaction could be sent, its nonce is spent.
* Signing the same nonce again (a replacement) is allowed, and counted in the window.
* A nonce below the last read is refused without a read (nonces only grow). A read is made only when a request is
  beyond what the last one allows.

**Every spent nonce is the vault's.** Each time the chain's next nonce moves, the vault checks every nonce it moved
past. One of the vault's own transactions for that nonce must have a receipt. The nonce and the receipts are read in
one batch, nonce first.

* **Anything else means the key signs somewhere else:** a shared key, a leaked one, or a second vault.
* **The vault then stops signing until it is restarted**, and logs `audit: ALERT`. It reads the chain a second time
  before it stops (a load-balanced RPC can answer from two heights).
* **It also checks every 10 s while idle**, so the stop does not wait for the next request.
* **Pending transactions from before the start are accepted as they are.** A restart is how an operator acknowledges
  an alert.

**Per-window caps.** Over a sliding window (`--window-secs`, default 3600), the vault caps:

| cap | flag | roles |
|---|---|---|
| signatures | `--max-signs-per-window` (default 3600) | all |
| fee budget: the sum of gas limit × max fee | `--max-fee-eth-per-window` (required) | all |
| token0 moved, both ways (`amount0Out + pay0`; a batch's `in0 + out0`) | `--max-volume0-per-window` | operator (required), automator |
| token1 moved, both ways (`amount1Out + pay1 + treasuryPay`; `in1 + out1`) | `--max-volume1-per-window` | operator (required), automator |
| notes settled | `--max-notes-per-window` (default 3600) | automator |

A compromised client gets at most one window's worth before the caps stop it. Amounts are base units: `1000e18` is
1,000 tokens of 18 decimals. The window lives in memory, so a restart (a host-level action) clears it.

**If the chain cannot be read, the vault refuses (`chain`) rather than guesses.**

## Start-up checks

The vault refuses to start unless:

* the RPC's chain id is `--chain-id`;
* there is code at the role's contract;
* the role holds on chain:
  * the operator's key is `SearcherExecutor.operator()`, or every rebalance would revert;
  * the relayer's and the automator's keys are not, when `--executor` is given (recommended);
* the role's configuration is read from the chain:
  * the relayer reads the feed's `providerId`, `queryCommitment`, `minPrice` and `maxPrice`;
  * the automator reads the pool's `batchSize`;
* KMS answers `GetPublicKey` for the key: the vault's address comes from it.

Its first lines name the role, the key and the address, then the policy and the rules. The clients check their own
configuration against the `policy` method at start:

* The engine accepts only an operator vault, the relayer only a relayer vault, the automator only an automator vault.
* Each checks the chain, the destination and the caps.

## The key: AWS KMS

* **Spec.** `KeySpec ECC_SECG_P256K1`, `KeyUsage SIGN_VERIFY`, origin `AWS_KMS`. The key is generated in KMS's
  FIPS 140-validated HSMs and cannot be exported.
* **Signing.** KMS signs the transaction's 32-byte signing hash (`MessageType DIGEST`, `ECDSA_SHA_256`) and returns DER.
  alloy's `AwsSigner` normalizes the signature to low s and recovers the parity against the key's public key.
* **One key per role.** The operator, relayer and automator keys are separate KMS keys. The notary's attestation key is
  a fourth one, used by the notary itself: it signs attestations, not transactions.
* **Least privilege.** Each vault's principal may use its own key only. The clients get no KMS permission at all.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    { "Sid": "ReadTheOperatorKey", "Effect": "Allow", "Action": "kms:GetPublicKey",
      "Resource": "arn:aws:kms:REGION:ACCOUNT:key/OPERATOR-KEY-ID" },
    { "Sid": "SignDigestsWithTheOperatorKey", "Effect": "Allow", "Action": "kms:Sign",
      "Resource": "arn:aws:kms:REGION:ACCOUNT:key/OPERATOR-KEY-ID",
      "Condition": { "StringEquals": { "kms:SigningAlgorithm": "ECDSA_SHA_256", "kms:MessageType": "DIGEST" } } }
  ]
}
```

* **Audit.** Every signature is a KMS call, logged by CloudTrail, besides the vault's own audit line.
* **No local key in production.** The release binary links no local-key signer at all. The test key (below) exists
  only behind a debug-only feature.

## Access control on the socket

* **The directory.** The socket lives in a private directory, by default `$XDG_RUNTIME_DIR/strawberryswap-vault/`.
  It must belong to the vault's user and grant nothing to other users, and is created 0700 if missing. The vault
  refuses to start in a directory that is open to others.
* **The socket file.** One per role: `<role>.sock`, 0600. When `--allow-uid` names another user (the client's), it is
  0660, and the directory should be 0750 with a group shared by the two users. The vault refuses to take over a live
  socket or to replace a file that is not a socket.
* **The kernel uid check.** On every connection the kernel reports the peer's uid (`SO_PEERCRED`), and only
  `--allow-uid` is served. Even root, which ignores file permissions, is refused if it is not the allowed uid.
* **Limits.** Request lines are at most 256 KiB (a 64-note batch is about 15 KiB of calldata), idle connections close
  after 30 s, and at most 8 connections are open at once.

## Protocol

One JSON object per line, in each direction.

```json
{"id": 1, "method": "address"}
{"id": 1, "result": {"address": "0xE250…2c09"}}

{"id": 2, "method": "policy"}
{"id": 2, "result": {"role": "operator", "chain_id": 421614, "to": "0x…", "contract": "SearcherExecutor",
  "function": "rebalance(uint256,uint256,uint256,uint256,uint256)", "selector": "0xc0661737",
  "address": "0x…", "key": "aws-kms", "max_gas": 1000000, "max_fee_per_gas": "500000000000",
  "max_signs_per_sec": 20.0,
  "nonce": {"reads_chain": true, "next": 41, "max_pending": 1, "halted": null},
  "window": {"secs": 3600,
    "caps": {"signatures": 3600, "fee_wei": "500000000000000000", "volume0": "…", "volume1": "…", "notes": null},
    "used": {"signatures": 12, "fee_wei": "…", "volume0": "…", "volume1": "…", "notes": 0}}}}

{"id": 3, "method": "sign_transaction", "params": {"tx": "0x02f8…"}}
{"id": 3, "result": {"r": "0x…", "s": "0x…", "y_parity": 0, "signing_hash": "0x…", "tx_hash": "0x…", "signed_tx": "0x02f8…"}}
{"id": 4, "error": {"code": "policy", "message": "sweep moves inventory out of the executor: never signed by this vault"}}
```

* **`policy`** is read-only. The relayer's also has `provider_id`, `query_commitment`, `min_price` and `max_price`; the
  automator's has `max_notes`.
* **`params.tx`** is the EIP-1559 signing payload: `0x02 ‖ rlp([chain_id, nonce, max_priority_fee_per_gas,
  max_fee_per_gas, gas_limit, to, value, data, access_list])`.
* **`signed_tx`** is ready for `eth_sendRawTransaction`.
* **Error codes:**

  | code | meaning |
  |---|---|
  | `parse` | a malformed request, or `params.tx` not hex |
  | `unknown_method` | not `address`, `policy` or `sign_transaction` |
  | `policy` | the transaction breaks one of the role's rules |
  | `rate_limited` | too many signatures this second |
  | `window_cap` | over one of the window's caps |
  | `nonce` | spent, ahead of the chain, or a gap |
  | `halted` | a nonce was spent by another signer: stopped until restart |
  | `chain` | the chain could not be read for the nonce rule |
  | `signer` | KMS failed |

  Unknown fields in a request are errors.

| flag | meaning |
|---|---|
| `--role ROLE` | `operator`, `relayer` or `automator` (required) |
| `--chain-id ID` | the only chain id signed for (required: the vault never guesses) |
| `--rpc-url URL` | the chain, for the start-up checks and the nonce rule (required; never printed, and no error carries it) |
| `--kms-key-id KEY` | the role's key: a key id or ARN (preferred) or an alias (required) |
| `--kms-region REGION`, `--kms-endpoint URL` | default: the standard AWS configuration; a VPC endpoint |
| `--executor ADDRESS` | the operator's destination (required for it); for the other roles, the contract whose operator the key must not be |
| `--feed ADDRESS` | the ZkPriceFeed (relayer: required) |
| `--pool ADDRESS` | the DarkPool (automator: required) |
| `--socket PATH` | default `$XDG_RUNTIME_DIR/strawberryswap-vault/<role>.sock`, else `<tmp>/strawberryswap-vault-<uid>/<role>.sock` |
| `--allow-uid UID` | the uid allowed to connect (default: the vault's own) |
| `--max-gas GAS` | default: operator 1,000,000, relayer 2,000,000, automator 5,000,000 |
| `--max-fee-gwei GWEI` | max fee per gas cap (default 500) |
| `--max-signs-per-sec N` | rate limit (default 20) |
| `--max-pending N` | nonces signed beyond the chain's next, one included (default 1, at most 16) |
| `--window-secs S` and the `--max-*-per-window` caps | the table above |

Credentials and region come from the standard AWS chain: on EC2, the instance role.

## Verification

* **Policies** (`src/policy.rs`):
  * Every rule above, one denial each, for every role.
  * Each role refuses each other role's function, at its own contract or at the other's.
  * The relayer refuses another provider, another instrument, a price out of bounds and a proof of the wrong length.
  * The automator refuses a batch of no notes or too many, arrays that differ in length, zero notes, long
    ciphertexts, a wrong proof length, flows of 2¹²⁸ and non-canonical calldata.
  * Malformed payloads: other transaction types, truncation, trailing bytes, a non-minimal RLP header.
* **Cross-checks with the clients:**
  * The `rebalance` selector is the C++ bot's constant.
  * The automator's golden settleBatch calldata (checked against Foundry's `cast`) passes the automator policy.
  * The automator's Go EIP-1559 encoder matches alloy's bytes on a pinned payload
    (`bots/automator/testdata/eip1559.vault.json`), which the automator role signs as is.
* **Rules** (`src/rules.rs`):
  * The nonce rule: the next nonce only, up to `max_pending`, never a gap, replacements.
  * A nonce spent by another signer stops the vault, after a second read.
  * A lagging read changes nothing.
  * The window: every cap, and signatures leaving it as it slides.
  * Exact amount parsing.
* **The vault** (`src/vault.rs`):
  * Signatures for all three roles recover to the vault, and the signed transactions decode and hash right.
  * The nonce rule follows the chain; the halt; a chain that cannot be read refuses.
  * The window and the rate limit answer with their codes.
  * The idle watcher catches a foreign nonce.
* **KMS:**
  * The vault signs through `AwsSigner` against a local stand-in for KMS that speaks its wire protocol, high-s
    signatures included. Each signature is exactly one KMS `Sign` of the signing hash.
  * A key that does not exist fails at start.
* **The chain client:**
  * Batched reads, with replies matched by id.
  * No error carries the RPC URL, even when the connection fails.
* **Start-up and the command line:**
  * A key that does not hold its role on chain is refused.
  * The relayer and the automator read their contracts.
  * Each role requires its contract and only its caps.
* **Socket:** directory 0700, socket 0600, the full protocol, an oversized line, refusing a live socket or a non-socket
  file, refusing an open directory, and refusing a foreign uid.
* **End to end:**
  * `bots/hft-engine-rust/tests/vault_e2e.rs`: the engine with this binary (test key, no chain).
  * `oracle/tests/e2e_anvil.rs`: the relayer through a relayer vault on anvil.
  * `bots/hft-engine-rust/tests/live_e2e.rs`: the whole live path, with operator and relayer vaults on stand-in KMS
    keys. The vaults refuse each other's function live. The memory scan finds the operator key only in the KMS
    stand-in: in neither the vault nor the engine.
* **Code checks:**
  * `#![forbid(unsafe_code)]`, and clippy with `-D warnings`, with and without the test-key feature.
  * CI checks that an optimized build with `insecure-test-key` fails to compile.

## The `insecure-test-key` feature (the engine's end-to-end tests only)

The engine's mock-chain end-to-end tests must show that the signing key never reaches the engine's memory. For that
they need to know the key, so they can look for it.

* **The key file.** With this feature, an **operator** vault takes `--insecure-test-key-file PATH` instead of
  `--kms-key-id`, and may run without `--rpc-url`. Its nonce rule then follows its own signatures only.
* **The key.** keccak256 of a public phrase, `"strawberryswap insecure test key: end-to-end tests only"`, at address
  `0xf8e44d4517A0de6610113165659DaAd8cF9327B2`. No key literal is stored anywhere. The banner and stderr announce
  `INSECURE TEST KEY`.

Three guards keep the feature away from deployments:

* It is off by default.
* `compile_error!` rejects it in any optimized build.
* CI checks that the release build fails.

Without the feature, the binary does not even link the local-key signer.

## Deployment (testnet)

* **One systemd unit and one user per role,** for example `strawberryswap-vault@operator`. The client's user is the
  socket's `--allow-uid`, with a group-shared `RuntimeDirectory` (mode 0750).
* **Separate users mean separate memory.** No client can read a vault's memory (`/proc/<pid>/mem` needs the same uid,
  or CAP_SYS_PTRACE), and none can reach another role's socket.
* **The network.** The vault needs egress to KMS (a VPC endpoint) and to the RPC node, and nothing else:
  * `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`;
  * `IPAddressDeny=any` with `IPAddressAllow=` the endpoint's and the node's addresses;
  * `NoNewPrivileges=yes`, `ProtectSystem=strict`, `MemoryDenyWriteExecute=yes`, `LimitCORE=0`.
* **Credentials.** Each vault's AWS principal may use its own key only (the policy above).
  * On a single EC2 host the instance role is shared. Give each vault its own role through a profile
    (`role_arn`, `credential_source = Ec2InstanceMetadata`), and block instance metadata for the other users with
    iptables `--uid-owner`. Phase 5 sets this up.

## Next

* **Threshold signing before mainnet.** Generate the key by DKG into t-of-n shares, for example 2-of-3. Sign with a
  threshold ECDSA protocol (the CGGMP21 / DKLs23 family) behind the same `alloy_signer::Signer` trait. Each
  co-signer enforces these policies and rules independently.
* **A price check against the oracle** for rebalances, and a persisted window, so that a restart does not clear it.
* **Scope.** Withdrawals such as `sweep` belong to the corporate vault (`mpc/tss-vault/`, with human approval). No role
  here ever signs them.
