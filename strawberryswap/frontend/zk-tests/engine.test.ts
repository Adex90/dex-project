// Wallet logic: key derivation, recovery from chain events, intent creation, output detection, spend bookkeeping.
// A recording prover is used here (fast); zk-tests/proving.test.ts runs the same flows with REAL proofs.
import assert from "node:assert/strict";
import { test } from "node:test";
import { ZkEngine } from "../src/zk/engine";
import { bigintToHex32 } from "../src/zk/field";
import { extHashNoAddress, extHashWithAddress, makeIntentNote, refundOutputCommitment, swapOutputCommitment } from "../src/zk/notes";
import { ALICE, FakeChain, RecordingProver, SIG_A, SIG_B, U, sealLikeAutomator } from "./helpers";

async function funded(amount = 1000n * U, asset = 0n) {
  const prover = new RecordingProver();
  const eng = new ZkEngine(prover);
  eng.unlock(SIG_A);
  const chain = new FakeChain();
  const dep = await eng.proveDeposit({ assetId: asset, amount, caller: ALICE });
  chain.pushPair(dep.js.commitments, dep.js.encryptedNotes);
  await eng.addLeaves(chain.leaves);
  return { eng, prover, chain, dep };
}

test("the proof's public signals are exactly what the contract will recompute", async () => {
  const { prover, dep } = await funded();
  const pub = (prover.calls[0] as { pub: bigint[] }).pub;
  // root, n0, n1, c0, c1, publicIn, publicOut, extDataHash, assetId
  assert.equal(pub.length, 9);
  assert.equal(pub[5], 1000n * U);
  assert.equal(pub[6], 0n);
  assert.equal(pub[7], extHashWithAddress(ALICE, dep.js.encryptedNotes[0], dep.js.encryptedNotes[1]), "deposit is bound to the caller + ciphertexts");
  assert.equal(pub[8], 0n);
  assert.equal(bigintToHex32(pub[3] as bigint), dep.js.commitments[0]);
});

test("a fresh session recovers its notes from chain events + the same signature alone", async () => {
  const { chain } = await funded(750n * U, 1n);
  const again = new ZkEngine(new RecordingProver());
  again.unlock(SIG_A);
  const st = await again.addLeaves(chain.leaves);
  assert.equal(st.notes.length, 1);
  assert.equal(st.notes[0]?.value, 750n * U);
  assert.equal(st.notes[0]?.assetId, 1n);

  const stranger = new ZkEngine(new RecordingProver());
  stranger.unlock(SIG_B);
  assert.equal((await stranger.addLeaves(chain.leaves)).notes.length, 0, "another key must see nothing");
});

test("index gaps are refused (a missed event would corrupt every Merkle path)", async () => {
  const eng = new ZkEngine(new RecordingProver());
  eng.unlock(SIG_A);
  await assert.rejects(eng.addLeaves([{ index: 3, commitment: 1n, encryptedNote: "0x00" }]), /leaf gap/);
});

test("intent: opening has no key material, binds the terms, and is recoverable from the chain", async () => {
  const { eng, prover, chain } = await funded();
  const b = await eng.proveIntent({ dir: 0, amount: 400n * U, minOut: 790n * U, deadline: 1_900_000_000n });
  const pub = (prover.calls[1] as { pub: bigint[] }).pub;
  assert.equal(pub[7], extHashNoAddress(b.js.encryptedNotes[0], b.js.encryptedNotes[1]), "transferNote binds only the ciphertexts");
  assert.equal(pub[5], 0n);
  assert.equal(pub[6], 0n);

  const json = JSON.stringify(b.opening);
  assert.deepEqual(Object.keys(b.opening).sort(), ["deadline", "destBlinding", "destPubKey", "dir", "minOut", "refundPubKey", "value"]);
  assert.equal(b.opening.value, (400n * U).toString());
  assert.equal(b.opening.minOut, (790n * U).toString());
  const priv = (eng as unknown as { keys: { privKey: bigint } }).keys.privKey.toString();
  assert.ok(!json.includes(priv), "the private key must never appear in what is sent to the Automator");

  chain.pushPair(b.js.commitments, b.js.encryptedNotes);
  const fresh = new ZkEngine(new RecordingProver());
  fresh.unlock(SIG_A);
  const st = await fresh.addLeaves(chain.leaves);
  assert.equal(st.intents.length, 1);
  assert.equal(st.intents[0]?.state, "created");
  assert.equal(st.intents[0]?.commitment, b.intentCommitment);
  assert.equal(fresh.pendingOpenings()[0]?.opening.destBlinding, b.opening.destBlinding, "a lost session can resubmit the same intent");
  const change = st.notes.find((n) => n.value === 600n * U);
  assert.ok(change, "the 600 change note is recovered too");
  assert.ok(!st.notes.some((n) => n.commitment === b.intentCommitment), "an intent note is NOT spendable by the wallet (owned by the batch key)");
});

test("insufficient funds and double use of one note are refused", async () => {
  const { eng } = await funded(100n * U);
  await assert.rejects(eng.proveIntent({ dir: 0, amount: 101n * U, minOut: 0n, deadline: 2n }), /no single shielded note/);
  await assert.rejects(eng.proveIntent({ dir: 1, amount: 1n * U, minOut: 0n, deadline: 2n }), /no single shielded note/, "wrong asset");
  await eng.proveIntent({ dir: 0, amount: 60n * U, minOut: 0n, deadline: 2n });
  await assert.rejects(eng.proveIntent({ dir: 0, amount: 10n * U, minOut: 0n, deadline: 2n }), /no single shielded note/, "the note is reserved until the first tx resolves");
});

test("the Automator's sealed output is detected as settled — but only if it matches the on-chain leaf", async () => {
  const { eng, chain } = await funded();
  const b = await eng.proveIntent({ dir: 0, amount: 400n * U, minOut: 790n * U, deadline: 1_900_000_000n });
  chain.pushPair(b.js.commitments, b.js.encryptedNotes);
  await eng.addLeaves(chain.leaves);
  const terms = { dir: 0 as const, value: 400n * U, destPubKey: eng.state().pubKey as bigint, refundPubKey: eng.state().pubKey as bigint, destBlinding: BigInt(b.opening.destBlinding), minOut: 790n * U, deadline: 1_900_000_000n };
  assert.equal(makeIntentNote(terms).commitment, b.intentCommitment);

  // a forged blob: opens under the right key but its value does not hash to the leaf => ignored
  chain.push(swapOutputCommitment(terms, 799n * U), await sealLikeAutomator(terms.destBlinding, 1, 12345n));
  let st = await eng.addLeaves(chain.leaves);
  assert.equal(st.intents[0]?.state, "created");
  assert.ok(!st.notes.some((n) => n.assetId === 1n));

  // the genuine output
  chain.push(swapOutputCommitment(terms, 799n * U), await sealLikeAutomator(terms.destBlinding, 1, 799n * U));
  st = await eng.addLeaves(chain.leaves);
  assert.equal(st.intents[0]?.state, "settled");
  assert.equal(st.notes.find((n) => n.assetId === 1n)?.value, 799n * U);
});

test("a refund is recognised as a refund", async () => {
  const { eng, chain } = await funded();
  const b = await eng.proveIntent({ dir: 0, amount: 250n * U, minOut: 0n, deadline: 5n });
  chain.pushPair(b.js.commitments, b.js.encryptedNotes);
  await eng.addLeaves(chain.leaves);
  const pub = eng.state().pubKey as bigint;
  const terms = { dir: 0 as const, value: 250n * U, destPubKey: pub, refundPubKey: pub, destBlinding: BigInt(b.opening.destBlinding), minOut: 0n, deadline: 5n };
  chain.push(refundOutputCommitment(terms), await sealLikeAutomator(terms.destBlinding, 0, 250n * U));
  const st = await eng.addLeaves(chain.leaves);
  assert.equal(st.intents[0]?.state, "refunded");
  assert.equal(st.notes.filter((n) => n.assetId === 0n && n.value === 250n * U).length, 1);
});

test("withdraw binds the recipient and reserves the note; spent notes cannot be withdrawn", async () => {
  const { eng, prover } = await funded(321n * U);
  const note = eng.state().notes[0];
  assert.ok(note);
  const w = await eng.proveWithdraw({ commitment: note.commitment, recipient: ALICE });
  const pub = (prover.calls[1] as { pub: bigint[] }).pub;
  assert.equal(pub[6], 321n * U, "publicOut == the note's value");
  assert.equal(pub[7], extHashWithAddress(ALICE, w.js.encryptedNotes[0], w.js.encryptedNotes[1]));
  await assert.rejects(eng.proveWithdraw({ commitment: note.commitment, recipient: ALICE }), /not spendable/);
  eng.markSpent([note.nullifier]);
  assert.equal(eng.state().notes[0]?.spent, true);
});

test("lock() forgets everything", async () => {
  const { eng } = await funded();
  eng.lock();
  const st = eng.state();
  assert.deepEqual([st.unlocked, st.notes.length, st.intents.length, st.treeSize], [false, 0, 0, 0]);
  await assert.rejects(eng.proveDeposit({ assetId: 0n, amount: 1n, caller: ALICE }), /locked/);
});
