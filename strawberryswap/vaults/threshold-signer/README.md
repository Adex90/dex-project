# threshold-signer

The vault daemon that signs StrawberrySwap's market-making transactions for the HFT engine (`bots/hft-engine-rust`).
This is **iteration 0**. The process boundary, the socket's access control and the transaction policy are real. The
key and the chain-aware parts of the policy are mocked.

```bash
cargo test                                                     # 12 tests, offline
cargo run --release -- --chain-id 31337 --executor 0x<SearcherExecutor address>
```

## Why the signer is a separate process

The engine is the exposed process. It parses market data, talks to RPC nodes over the network, quotes every 100 ms
and links about 440 crates. Whatever compromises it must not reach a key: an exploit in a parser or a dependency, a
memory disclosure, or a careless log line. So the engine holds **no key at all**. The `SearcherExecutor` contract
was written for this: its `operator`, the only address allowed to call `rebalance` and `sweep`, is "MPC/TSS-controlled;
the bot never holds a hot key". That operator is this vault's address.

```text
 RPC, market data ──► HFT engine (user hft)                     vault (user vault)
                        no key                                     policy + key, no network
                        builds an unsigned EIP-1559 rebalance ──►  Unix socket, kernel-checked uid
                                                    signature ◄──  decode → policy → sign
                        broadcasts the signed transaction
```

The vault does one thing: decide whether a transaction may be signed, and sign it. It has no network access (in
production, `PrivateNetwork=yes`), a single local socket, no key export, and an audit line for every decision.

## What a compromised engine can still do

It can only ask for signatures that the policy allows. Anything else is refused with a reason.

| rule | refused otherwise |
|---|---|
| EIP-1559 (type 2) only, canonical RLP, nothing after it | legacy, 2930, 4844 (blobs), 7702 (code delegation) |
| the configured chain id | replay on another chain |
| destination = the SearcherExecutor; no contract creation | calls to any other contract |
| value 0, empty access list | ether transfers |
| selector `rebalance(uint256,uint256,uint256,uint256,uint256)` = `0xc0661737` | `sweep` (`0x62c06767`: inventory out of the executor) and every other function |
| exactly five words, each < 2¹²⁸, at least one of `amount0Out`, `amount1Out` non-zero | malformed calldata, or a rebalance the pool would revert |
| gas limit ≤ cap, max fee ≤ cap, priority fee ≤ max fee | fee-burning transactions |
| at most N signatures per second (default 20; the engine needs at most 10) | floods |

Each signed rebalance is then bounded a second time on chain:
* `DarkPool.flashRebalance` must strictly reduce |spot − oracle price| and pull the Treasury's cut.
* The executor's tokens can only go back to the pool.

So the worst case is a stream of policy-valid rebalances at oracle-bounded prices, limited by the rate and the
executor's inventory. It is not a drain.

## Access control on the socket

* **The directory.** The socket lives in a private directory, by default `$XDG_RUNTIME_DIR/strawberryswap-vault/`.
  It must belong to the vault's user and grant nothing to other users, and is created 0700 if missing. The vault
  refuses to start in a directory that is open to others.
* **The socket file.** It is 0600. When `--allow-uid` names another user (the engine's), it is 0660, and the
  directory should be 0750 with a group shared by the two users. The vault refuses to take over a live socket or to
  replace a file that is not a socket.
* **The kernel uid check.** On every connection the kernel reports the peer's uid (`SO_PEERCRED`), and only
  `--allow-uid` is served. Even root, which ignores file permissions, is refused if it is not the allowed uid.
* **Limits.** Request lines are at most 16 KiB, idle connections close after 30 s, and at most 8 connections are open
  at once.

## Protocol

One JSON object per line, in each direction.

```json
{"id": 1, "method": "address"}
{"id": 1, "result": {"address": "0xE250…2c09"}}

{"id": 2, "method": "sign_transaction", "params": {"tx": "0x02f8…"}}
{"id": 2, "result": {"r": "0x…", "s": "0x…", "y_parity": 0, "signing_hash": "0x…", "tx_hash": "0x…", "signed_tx": "0x02f8…"}}
{"id": 3, "error": {"code": "policy", "message": "sweep moves inventory out of the executor: never signed by this vault"}}
```

* **`params.tx`** is the EIP-1559 signing payload: `0x02 ‖ rlp([chain_id, nonce, max_priority_fee_per_gas,
  max_fee_per_gas, gas_limit, to, value, data, access_list])`.
* **`signed_tx`** is ready for `eth_sendRawTransaction`.
* **Error codes:** `parse`, `unknown_method`, `policy`, `rate_limited`, `signer`. Unknown fields in a request are
  errors.

| flag | meaning |
|---|---|
| `--chain-id ID` | the only chain id signed for (required: the vault never guesses) |
| `--executor ADDRESS` | the SearcherExecutor, the only destination (required) |
| `--socket PATH` | default `$XDG_RUNTIME_DIR/strawberryswap-vault/vault.sock`, else `<tmp>/strawberryswap-vault-<uid>/vault.sock` |
| `--allow-uid UID` | the uid allowed to connect (default: the vault's own) |
| `--max-gas GAS`, `--max-fee-gwei GWEI` | caps (defaults 1,000,000 and 500) |
| `--max-signs-per-sec N` | rate limit (default 20) |

## Verification

* **Policy (12 tests in all):**
  * Every rule above, one denial each.
  * Malformed payloads: other transaction types, truncation, trailing bytes, a non-minimal RLP header.
  * The `rebalance` selector is pinned to `0xc0661737`, which is also the C++ bot's constant and what viem computes.
* **Signatures:**
  * The signature recovers to the vault's address.
  * The signed transaction decodes back to the same transaction and recovers to the vault.
  * It hashes to `tx_hash`.
* **Socket:** directory 0700, socket 0600, the full protocol, an oversized line, refusing a live socket or a non-socket
  file, refusing an open directory, and refusing a foreign uid.
* **Across processes:**
  * A separate client process got a rebalance signed. A `sweep`, a wrong chain and attached ether were each denied
    with their reason.
  * User `nobody` could not reach the socket (permission denied on the directory).
  * With `--allow-uid 65534`, a root connection was refused by the kernel uid check.
* **Code checks:**
  * `#![forbid(unsafe_code)]`, and clippy with `-D warnings`.
  * valgrind memcheck on the whole test suite: 0 errors, 0 bytes definitely or indirectly lost.
  * The CI job "Threshold signer (Rust)" runs fmt, clippy and the tests.

## Mocked in this iteration

1. **The key.** It is one secp256k1 key generated in memory at start. k256 zeroizes it on drop, and it is never
   stored or logged. Restarting changes the address.
2. **The policy reads no chain state.** It has no nonce sequencing, no notional or inventory limits, and no price
   check against the zk oracle.

## Next: from the mock key to threshold signing

* **Key generation.** Generate the key by DKG into t-of-n shares, for example 2-of-3: this vault host, an
  HSM/KMS-backed co-signer, and an offline recovery share. The whole key then never exists in one place.
* **Signing.** Sign with a threshold ECDSA protocol for secp256k1 (the CGGMP21 / DKLs23 family) behind the same
  `alloy_signer::Signer` trait. The socket protocol and the engine do not change.
* **Policy at every co-signer.** Each co-signer enforces the policy independently, so one compromised host cannot sign
  alone. Each also adds chain-aware rules: nonce sequencing, notional limits per window, and a price sanity check
  against the oracle.
* **Deployment.** Run the engine and the vault as separate users, under systemd hardening:
  * `PrivateNetwork=yes`, `NoNewPrivileges=yes`, `ProtectSystem=strict`, `MemoryDenyWriteExecute=yes`;
  * `LimitCORE=0`, and no swap or encrypted swap;
  * `RuntimeDirectory=strawberryswap-vault` with `RuntimeDirectoryMode=0750` and a group shared with the engine.
* **Scope.** Withdrawals such as `sweep` belong to the corporate vault (`mpc/tss-vault/`, with human approval). This
  hot-path signer never signs them.
