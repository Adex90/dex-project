// The browser crypto core must agree EXACTLY with the circuits' JS reference and the Go Automator.
// Vectors come from bots/automator/testdata (golden.json: Poseidon, tree, intents; sealed.golden.json: Go-sealed output).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { BATCH_PUBKEY, TREE_ZERO } from "../src/zk/field";
import { makeIntentNote, openSealedOutput, type IntentTerms } from "../src/zk/notes";
import { H } from "../src/zk/poseidon";
import { NoteTree } from "../src/zk/tree";

interface Golden {
  constants: { batchPubKey: string; zero: string };
  poseidon: { in: string[]; out: string }[];
  tree: { leaves: string[]; root: string; path3: string[]; bits3: number[] };
  intents: { label: string; index: number; json: { dir: 0 | 1; value: string; destPubKey: string; refundPubKey: string; destBlinding: string; minOut: string; deadline: number }; commitment: string; nullifier: string }[];
}
const g: Golden = JSON.parse(readFileSync(new URL("../../bots/automator/testdata/golden.json", import.meta.url), "utf8"));

test("nothing-up-my-sleeve constants match the circuit", () => {
  assert.equal(BATCH_PUBKEY.toString(), g.constants.batchPubKey);
  assert.equal(TREE_ZERO.toString(), g.constants.zero);
});

test("Poseidon matches circom for arities 1-5", () => {
  for (const v of g.poseidon) assert.equal(H(...v.in.map(BigInt)).toString(), v.out, `arity ${v.in.length}`);
});

test("note tree: root and Merkle path match the reference", () => {
  const t = new NoteTree();
  for (const l of g.tree.leaves) t.insert(BigInt(l));
  assert.equal(t.root.toString(), g.tree.root);
  const p = t.path(3);
  assert.deepEqual(p.elements.map(String), g.tree.path3);
  assert.deepEqual(p.bits, g.tree.bits3);
});

test("intent notes: commitment and batch nullifier match the Go Automator", () => {
  for (const it of g.intents) {
    const terms: IntentTerms = {
      dir: it.json.dir, value: BigInt(it.json.value), destPubKey: BigInt(it.json.destPubKey), refundPubKey: BigInt(it.json.refundPubKey),
      destBlinding: BigInt(it.json.destBlinding), minOut: BigInt(it.json.minOut), deadline: BigInt(it.json.deadline),
    };
    const note = makeIntentNote(terms);
    assert.equal(note.commitment.toString(), it.commitment, `${it.label}: commitment`);
    // the key is the intent's own destBlinding, never a public constant (Phase-8 audit H-7)
    assert.equal(H(note.commitment, BigInt(it.index), terms.destBlinding).toString(), it.nullifier, `${it.label}: nullifier`);
  }
});

test("a blob sealed by the Go Automator opens in the browser (and only with the right key)", async () => {
  const s = JSON.parse(readFileSync(new URL("../../bots/automator/testdata/sealed.golden.json", import.meta.url), "utf8")) as { blinding: string; assetOut: 0 | 1; value: string; blob: `0x${string}` };
  const out = await openSealedOutput(BigInt(s.blinding), s.blob);
  assert.deepEqual(out, { assetOut: s.assetOut, value: BigInt(s.value) });
  assert.equal(await openSealedOutput(BigInt(s.blinding) + 1n, s.blob), undefined, "wrong key must not open");
  const flipped = (s.blob.slice(0, -2) + (s.blob.endsWith("00") ? "01" : "00")) as `0x${string}`;
  assert.equal(await openSealedOutput(BigInt(s.blinding), flipped), undefined, "tampered blob must not open");
});
