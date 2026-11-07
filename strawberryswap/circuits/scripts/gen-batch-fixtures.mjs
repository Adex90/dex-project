// End-to-end fixtures for DarkPool + batch_swap, using REAL proofs everywhere:
//
//   users (JS, JoinSplit proofs) : depositNote -> transferNote(into an INTENT NOTE)
//   Automator (Go)               : batch-witness (selection, clearing price, witness)  ->  prove (SnarkJS wrapper)
//   users (JS, JoinSplit proofs) : withdrawNote of the batch outputs
//
// Output: contracts/test/fixtures/BatchFixtures.sol (+ BatchSwapGroth16Verifier.dev.sol).
// Requires: AUTOMATOR_BIN (built Go binary) and the dev keys from `setup-dev.mjs batch_swap_n4`.
import * as snarkjs from "snarkjs";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { keccak256, toBytes, getAddress } from "viem";
import { init, Tree, makeNote, buildInput, extDataHash, extDataHashNoAddr, stringify, FIELD } from "../test/lib.mjs";
import { intentNote } from "../test/batch.lib.mjs";

const OUT = "../contracts/test/fixtures";
mkdirSync(OUT, { recursive: true });
const h = await init();
const det = (l) => BigInt(keccak256(toBytes(l))) % FIELD;
const U = 10n ** 18n;
const T0 = 1_800_000_000n, TNOW = T0 + 3600n, N = 4;
const SNARKJS = resolve("node_modules/.bin/snarkjs");
const BIN = process.env.AUTOMATOR_BIN;
if (!BIN) throw new Error("set AUTOMATOR_BIN to the built automator binary");

const JS = { wasm: "build/joinsplit/joinsplit_js/joinsplit.wasm", zkey: "build/joinsplit/joinsplit.zkey", vkey: JSON.parse(readFileSync("build/joinsplit/joinsplit.vkey.json", "utf8")) };
const BS = { wasm: "build/batch_swap_n4/main_n4_js/main_n4.wasm", zkey: "build/batch_swap_n4/batch_swap_n4.zkey" };
const hex32 = (x) => "0x" + BigInt(x).toString(16).padStart(64, "0");
const enc = (n) => "0x" + n.toString(16).padStart(4, "0");

// ---- verifier fixture for the batch circuit
{
  let src = readFileSync("build/batch_swap_n4/batch_swap_n4.Verifier.sol", "utf8").replace(/contract Groth16Verifier/, "contract BatchSwapGroth16Verifier");
  writeFileSync(`${OUT}/BatchSwapGroth16Verifier.dev.sol`, `// ⚠ DEV-ONLY VERIFICATION KEY (single-party local setup). TEST FIXTURE: NEVER DEPLOY.\n` + src);
}

// ---- actors. Each user has ONE key used for ownership, payout and refund.
const mkUser = (label, addr, dir, amount, minOut, deadline) => {
  const priv = det(label + ".priv");
  return { label, addr: getAddress(addr), dir, amount, minOut, deadline, priv, pub: h(priv) };
};
const users = [
  mkUser("A", "0x000000000000000000000000000000000000a001", 0, 1000n * U, 1900n * U, T0 + 7200n), // sells token0, wants >= 1900 token1
  mkUser("B", "0x000000000000000000000000000000000000b002", 1, 800n * U, 390n * U, T0 + 7200n),   // sells token1
  mkUser("C", "0x000000000000000000000000000000000000c003", 0, 250n * U, 0n, T0 + 7200n),
  mkUser("D", "0x000000000000000000000000000000000000d004", 0, 77n * U, 0n, T0 + 60n),             // expires before tNow => refunded
];

const T = new Tree(h);
const steps = { deposit: [], transfer: [], withdraw: [] };
const dummy = (label, assetId) => makeNote(h, { assetId: BigInt(assetId), value: 0n, privKey: det(label + ".k"), blinding: det(label + ".b") });

async function proveJS(kind, actor, { assetId, ins, outs, publicIn = 0n, publicOut = 0n, encs, ext }) {
  const input = buildInput(h, T, { assetId: BigInt(assetId), ins, outs, publicIn, publicOut, ext });
  const pub = [input.root, ...input.nullifier, ...input.commitment, publicIn, publicOut, ext, BigInt(assetId)];
  const { proof, publicSignals } = await snarkjs.groth16.fullProve(stringify(input), JS.wasm, JS.zkey);
  if (!(await snarkjs.groth16.verify(JS.vkey, publicSignals, proof))) throw new Error("local verification failed");
  if (publicSignals.some((s, i) => BigInt(s) !== BigInt(pub[i]))) throw new Error("public signal order mismatch");
  const [a, b, c] = JSON.parse(`[${await snarkjs.groth16.exportSolidityCallData(proof, publicSignals)}]`);
  const bytes = "0x" + [...a, ...b[0], ...b[1], ...c].map((x) => BigInt(x).toString(16).padStart(64, "0")).join("");
  const idx = outs.map((o) => T.insert(o.commitment)); // the contract inserts commitments[0] then [1]
  steps[kind].push({ actor: actor.addr, assetId, proof: bytes, root: input.root, n0: input.nullifier[0], n1: input.nullifier[1], c0: input.commitment[0], c1: input.commitment[1], enc0: encs[0], enc1: encs[1], amount: publicIn || publicOut });
  console.log(`  proved ${kind} ${actor.label}`);
  return idx;
}

// ---- 1. users deposit into DarkPool notes
let k = 0;
for (const u of users) {
  u.depNote = makeNote(h, { assetId: BigInt(u.dir), value: u.amount, privKey: u.priv, blinding: det(u.label + ".dep") });
  const encs = [enc(0x1000 + ++k), enc(0x1000 + ++k)];
  const [i0] = await proveJS("deposit", u, { assetId: u.dir, ins: [null, null], outs: [u.depNote, dummy(u.label + ".d1", u.dir)], publicIn: u.amount, encs, ext: extDataHash(u.addr, encs[0], encs[1]) });
  u.depIndex = i0;
}

// ---- 2. users move their note into an INTENT NOTE (owned by the unspendable BATCH_PUBKEY)
for (const u of users) {
  u.intent = intentNote(h, { dir: u.dir, value: u.amount, destPubKey: u.pub, refundPubKey: u.pub, destBlinding: det(u.label + ".dest"), minOut: u.minOut, deadline: u.deadline });
  const encs = [enc(0x2000 + ++k), enc(0x2000 + ++k)];
  const [i0] = await proveJS("transfer", u, { assetId: u.dir, ins: [{ note: u.depNote, index: u.depIndex }, null], outs: [u.intent, dummy(u.label + ".d2", u.dir)], encs, ext: extDataHashNoAddr(encs[0], encs[1]) });
  u.intentIndex = i0;
}

// ---- 3. the Go Automator plans the batch and proves it
// price: the fresh zk-oracle price the batch is checked against (DarkPool I5); BatchSwapE2E sets the same value.
const pool = { X: 3_000_000n * U, Y: 6_000_000n * U, R0: 1_000_000n * U, R1: 2_000_000n * U, price: 2n * U };
const state = {
  leaves: T.leaves.map(String), pool: Object.fromEntries(Object.entries(pool).map(([a, b]) => [a, b.toString()])), tNow: Number(TNOW), batchSize: N, minSwaps: 2,
  intents: users.map((u) => ({ dir: u.dir, value: u.amount.toString(), destPubKey: u.pub.toString(), refundPubKey: u.pub.toString(), destBlinding: det(u.label + ".dest").toString(), minOut: u.minOut.toString(), deadline: Number(u.deadline) })),
};
mkdirSync("build/e2e", { recursive: true });
writeFileSync("build/e2e/state.json", JSON.stringify(state));
execFileSync(BIN, ["batch-witness", "--state", "build/e2e/state.json", "--out", "build/e2e/witness.json"], { stdio: "inherit" });
console.log("  go: batch planned");
execFileSync(BIN, ["prove", "--witness", "build/e2e/witness.json", "--snarkjs", SNARKJS, "--wasm", BS.wasm, "--zkey", BS.zkey, "--out", "build/e2e/proof.json"], { stdio: "inherit" });
console.log("  go: batch proved (Go SnarkJS wrapper)");
const plan = JSON.parse(readFileSync("build/e2e/witness.json", "utf8"));
const bproof = JSON.parse(readFileSync("build/e2e/proof.json", "utf8"));
const root = BigInt(plan.public[0]);
if (root !== T.root) throw new Error("Go's tree root differs from the JS mirror");
// public layout: [root, tNow, X, Y, in0, in1, out0, out1, nullifiers(N), commitments(N)]
const nullifiers = plan.public.slice(8, 8 + N), commitments = plan.public.slice(8 + N, 8 + 2 * N);
const userOf = (slot) => users.find((u) => u.intentIndex === slot.leafIndex); // slot -> user via the intent note's leaf index
const slots = plan.slots.map((s) => ({ ...s, user: userOf(s) }));
// the contract inserts the batch's commitments in slot order
const slotIdx = slots.map((s) => T.insert(BigInt(s.commitment)));

// ---- 4. users withdraw what the batch produced
for (const [i, s] of slots.entries()) {
  const u = s.user;
  const outNote = { assetId: BigInt(s.assetOut), value: BigInt(s.valueOut), privKey: u.priv, pubKey: u.pub, blinding: BigInt(s.destBlinding), commitment: BigInt(s.commitment) };
  const encs = [enc(0x3000 + ++k), enc(0x3000 + ++k)];
  await proveJS("withdraw", u, { assetId: s.assetOut, ins: [{ note: outNote, index: slotIdx[i] }, null], outs: [dummy(u.label + ".d3", s.assetOut), dummy(u.label + ".d4", s.assetOut)], publicOut: BigInt(s.valueOut), encs, ext: extDataHash(u.addr, encs[0], encs[1]) });
}

// ---- Solidity
const stepSol = (st) => `Step({actor: ${st.actor}, assetId: ${st.assetId}, proof: hex"${st.proof.slice(2)}", root: ${hex32(st.root)}, n0: ${hex32(st.n0)}, n1: ${hex32(st.n1)}, c0: ${hex32(st.c0)}, c1: ${hex32(st.c1)}, enc0: hex"${st.enc0.slice(2)}", enc1: hex"${st.enc1.slice(2)}", amount: ${st.amount}})`;
const arr = (name, list) => `    function ${name}() internal pure returns (Step[] memory s) {\n        s = new Step[](${list.length});\n${list.map((x, i) => `        s[${i}] = ${stepSol(x)};`).join("\n")}\n    }\n`;
const sol = `// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
// GENERATED by circuits/scripts/gen-batch-fixtures.mjs — REAL proofs: JoinSplit (users) + batch_swap (Go Automator).

library BatchFixtures {
    uint64 internal constant T0 = ${T0};
    uint64 internal constant TNOW = ${TNOW};
    uint256 internal constant R0 = ${pool.R0};
    uint256 internal constant R1 = ${pool.R1};
    uint256 internal constant V0 = ${pool.X - pool.R0};
    uint256 internal constant V1 = ${pool.Y - pool.R1};

    struct Step {
        address actor; uint256 assetId; bytes proof; bytes32 root; bytes32 n0; bytes32 n1; bytes32 c0; bytes32 c1; bytes enc0; bytes enc1; uint256 amount;
    }

${arr("deposits", steps.deposit)}
${arr("transfers", steps.transfer)}
${arr("withdraws", steps.withdraw)}
    // ---- the batch (slot order == order of the arrays below)
    bytes internal constant BATCH_PROOF = hex"${bproof.proof.slice(2)}";
    bytes32 internal constant BATCH_ROOT = ${hex32(root)};
    uint256 internal constant NET_IN0 = ${plan.netIn0};
    uint256 internal constant NET_IN1 = ${plan.netIn1};
    uint256 internal constant NET_OUT0 = ${plan.netOut0};
    uint256 internal constant NET_OUT1 = ${plan.netOut1};
    uint256 internal constant PRICE_LO = ${plan.price};

    function batchNullifiers() internal pure returns (bytes32[] memory a) {
        a = new bytes32[](${N});
${nullifiers.slice(0, slots.length).map((x, i) => `        a[${i}] = ${hex32(x)};`).join("\n")}
    }

    function batchCommitments() internal pure returns (bytes32[] memory a) {
        a = new bytes32[](${N});
${commitments.slice(0, slots.length).map((x, i) => `        a[${i}] = ${hex32(x)};`).join("\n")}
    }

    // expected results per slot (computed by the Go Automator): who gets which asset and how much
    function expectedActor(uint256 i) internal pure returns (address) {
${slots.map((s, i) => `        if (i == ${i}) return ${s.user.addr};`).join("\n")}
        revert();
    }

    function expectedAsset(uint256 i) internal pure returns (uint256) {
${slots.map((s, i) => `        if (i == ${i}) return ${s.assetOut};`).join("\n")}
        revert();
    }

    function expectedValue(uint256 i) internal pure returns (uint256) {
${slots.map((s, i) => `        if (i == ${i}) return ${s.valueOut};`).join("\n")}
        revert();
    }

    function isRefund(uint256 i) internal pure returns (bool) {
${slots.map((s, i) => `        if (i == ${i}) return ${s.mode === "refund"};`).join("\n")}
        revert();
    }
}
`;
writeFileSync(`${OUT}/BatchFixtures.sol`, sol);
console.log("fixtures written:", slots.map((s) => `${s.user.label}:${s.mode}->asset${s.assetOut}:${Number(BigInt(s.valueOut) / 10n ** 15n) / 1000}`).join("  "));
// snarkjs keeps its worker threads alive after proving; exit explicitly so scripted pipelines do not hang.
process.exit(0);
