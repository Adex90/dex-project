// Adversarial tests for the JoinSplit circuit. Every "must reject" case is checked at WITNESS generation:
// if any constraint is violated the circom witness calculator throws, so no proof can exist.
import * as snarkjs from "snarkjs";
import { readFileSync } from "node:fs";
import { init, Tree, makeNote, dummyNote, buildInput, stringify, FIELD, rand } from "./lib.mjs";

const WASM = "build/joinsplit/joinsplit_js/joinsplit.wasm";
let pass = 0, fail = 0;
const wtns = { type: "mem" };

async function accepts(name, input) {
  try { await snarkjs.wtns.calculate(stringify(input), WASM, { type: "mem" }); pass++; console.log(`  ok   accepts  ${name}`); }
  catch (e) { fail++; console.log(`  FAIL accepts  ${name}: ${String(e.message).split("\n")[0]}`); }
}
async function rejects(name, input) {
  try { await snarkjs.wtns.calculate(stringify(input), WASM, { type: "mem" }); fail++; console.log(`  FAIL rejects  ${name}: witness was accepted!`); }
  catch (e) { pass++; console.log(`  ok   rejects  ${name}`); }
}

const h = await init();
const tree = new Tree(h);

// Pre-populate: Alice owns a 100-unit note at leaf 0 and a 7-unit note at leaf 1 (asset 0)
const alice = makeNote(h, { value: 100n });
const alice2 = makeNote(h, { value: 7n });
const i0 = tree.insert(alice.commitment);
const i1 = tree.insert(alice2.commitment);
const bob = makeNote(h, { value: 60n });
const change = makeNote(h, { value: 40n });

console.log("positive");
// shield: 2 dummy inputs, 100 public in -> 2 outputs 60 + 40
await accepts("shield 100 -> 60+40", buildInput(h, tree, { ins: [null, null], outs: [bob, change], publicIn: 100n }));
// private transfer: spend 100 -> 60 + 40, nothing public
await accepts("transfer 100 -> 60+40", buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [bob, change] }));
// two real inputs: 100 + 7 -> 107
await accepts("merge 100+7 -> 107+0", buildInput(h, tree, { ins: [{ note: alice, index: i0 }, { note: alice2, index: i1 }], outs: [makeNote(h, { value: 107n }), dummyNote(h)] }));
// unshield: 100 -> 40 private change + 60 public out
await accepts("unshield 100 -> 40 + 60 out", buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [change, dummyNote(h)], publicOut: 60n }));
// other asset id (ZK-ERC1155 id) must be consistent everywhere
const a7 = makeNote(h, { assetId: 7n, value: 5n });
await accepts("asset 7 shield", buildInput(h, tree, { assetId: 7n, ins: [null, null], outs: [a7, dummyNote(h, 7n)], publicIn: 5n }));

console.log("negative (each must fail to produce a witness)");
const good = () => buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [bob, change] });

{ // S4 conservation: create value from nothing
  const g = buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [makeNote(h, { value: 101n }), change] });
  await rejects("outputs exceed inputs by 1", g);
}
{ // S4: unshield more than the note holds
  await rejects("publicOut exceeds inputs", buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [dummyNote(h), dummyNote(h)], publicOut: 101n }));
}
{ // S4: shield with public amount not matching outputs
  await rejects("publicIn != sum(outputs)", buildInput(h, tree, { ins: [null, null], outs: [bob, change], publicIn: 99n }));
}
{ // S4: field wrap-around — value = FIELD-1 + 101 would balance mod p without range checks
  const neg = makeNote(h, { value: FIELD - 1n });
  const g = buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [neg, makeNote(h, { value: 101n })] });
  await rejects("modular wrap-around output value (FIELD-1)", g);
}
{ // S4: public amount wrap
  await rejects("publicOut = FIELD-1 (wrap)", buildInput(h, tree, { ins: [{ note: alice, index: i0 }, null], outs: [bob, change], publicOut: FIELD - 1n }));
}
{ // S2 membership: a note that is NOT in the tree
  const ghost = makeNote(h, { value: 1_000_000n });
  const g = buildInput(h, tree, { ins: [{ note: ghost, index: i0 }, null], outs: [makeNote(h, { value: 1_000_000n }), dummyNote(h)] });
  await rejects("spend a note that was never inserted", g);
}
{ // S2: claim a bigger value for a real leaf
  const inflated = { ...alice, value: 1_000n };
  inflated.commitment = h(0n, 1_000n, alice.pubKey, alice.blinding);
  const g = buildInput(h, tree, { ins: [{ note: inflated, index: i0 }, null], outs: [makeNote(h, { value: 1_000n }), dummyNote(h)] });
  await rejects("inflate the value of an existing leaf", g);
}
{ // S2: stale/forged root
  const g = good(); g.root = (BigInt(g.root) + 1n).toString();
  await rejects("wrong Merkle root", g);
}
{ // S1 ownership: spend Alice's note with another private key
  const thief = { ...alice, privKey: rand() };
  const g = buildInput(h, tree, { ins: [{ note: thief, index: i0 }, null], outs: [bob, change] });
  await rejects("spend with the wrong private key (commitment no longer matches leaf)", g);
}
{ // S3: forged nullifier
  const g = good(); g.nullifier[0] = (BigInt(g.nullifier[0]) + 1n).toString();
  await rejects("forged nullifier", g);
}
{ // S3: same note as both inputs => equal nullifiers
  const g = buildInput(h, tree, { ins: [{ note: alice, index: i0 }, { note: alice, index: i0 }], outs: [makeNote(h, { value: 200n }), dummyNote(h)] });
  await rejects("double-spend inside one join-split (n0 == n1)", g);
}
{ // S3: change the claimed leaf index to dodge the nullifier
  const g = good(); g.inPathIndices[0][0] = 1 - g.inPathIndices[0][0];
  await rejects("flip a path bit (different leaf index)", g);
}
{ // S5 assetId mixing: public asset differs from the note's asset
  const g = good(); g.assetId = "9";
  await rejects("note asset != public assetId", g);
}
{ // S5 output commitment tampered
  const g = good(); g.commitment[1] = (BigInt(g.commitment[1]) + 1n).toString();
  await rejects("tampered output commitment", g);
}
{ // path index must be a bit
  const g = good(); g.inPathIndices[0][3] = 2;
  await rejects("non-boolean path index", g);
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
