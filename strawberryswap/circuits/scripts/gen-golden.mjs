// Emits golden vectors from the JS reference implementation for the Go Automator's cross-language tests:
//   bots/automator/testdata/golden.json
// Go must reproduce every value EXACTLY (Poseidon, tree root/paths, intent commitments/nullifiers, clearing
// price, payouts, nets, and the complete circuit witness).
import { writeFileSync, mkdirSync } from "node:fs";
import { keccak256, toBytes } from "viem";
import { init, Tree, stringify, FIELD } from "../test/lib.mjs";
import { intentNote, buildBatchInput, clear, nullifierOf, BATCH_PUBKEY, BATCH_NK } from "../test/batch.lib.mjs";

const h = await init();
const det = (l) => BigInt(keccak256(toBytes(l))) % FIELD;
const E = 10n ** 18n;
const X = 3_000_000n * E, Y = 6_000_000n * E, R0 = 1_000_000n * E, R1 = 2_000_000n * E;
const NOW = 1_800_000_000n, N = 4;

const tree = new Tree(h);
const mk = (label, o) => {
  const n = intentNote(h, { destPubKey: det(label + ".d"), refundPubKey: det(label + ".r"), destBlinding: det(label + ".b"), minOut: 0n, deadline: NOW + 3600n, ...o });
  n.index = tree.insert(n.commitment); n.label = label; return n;
};
// unrelated leaves first so indices are non-trivial
for (let i = 0; i < 3; i++) tree.insert(det("filler" + i));
const notes = [
  mk("a", { dir: 0, value: 1000n * E, minOut: 1900n * E, deadline: NOW + 3000n }),
  mk("b", { dir: 1, value: 800n * E, minOut: 390n * E, deadline: NOW + 4000n }),
  mk("c", { dir: 0, value: 250n * E, deadline: NOW + 5000n }),
  mk("exp", { dir: 0, value: 77n * E, deadline: NOW - 10n }),
];

// ordering rule shared with the Go batcher: swaps by (deadline, commitment) ascending, then refunds
const cmp = (a, b) => (a.deadline !== b.deadline ? (a.deadline < b.deadline ? -1 : 1) : a.commitment < b.commitment ? -1 : 1);
const swaps = notes.filter((n) => n.deadline >= NOW).sort(cmp), refunds = notes.filter((n) => n.deadline < NOW).sort(cmp);
const items = [...swaps.map((n) => ({ mode: "swap", note: n, index: n.index })), ...refunds.map((n) => ({ mode: "refund", note: n, index: n.index }))];
const input = buildBatchInput(h, tree, items, { X, Y, tNow: NOW, N });
const c = clear(X, Y, swaps.map((n) => ({ dir: n.dir, value: n.value })));

const out = {
  constants: { batchPubKey: BATCH_PUBKEY.toString(), batchNK: BATCH_NK.toString(), zero: tree.zeros[0].toString() },
  poseidon: [[1n, 2n], [0n], [1n, 2n, 3n], [1n, 2n, 3n, 4n], [1n, 2n, 3n, 4n, 5n]].map((v) => ({ in: v.map(String), out: h(...v).toString() })),
  tree: { leaves: tree.leaves.map(String), root: tree.root.toString(), path3: tree.path(3).pathElements.map(String), bits3: tree.path(3).pathIndices },
  intents: notes.map((n) => ({
    label: n.label, index: n.index,
    json: { dir: n.dir, value: n.value.toString(), destPubKey: n.destPubKey.toString(), refundPubKey: n.refundPubKey.toString(), destBlinding: n.destBlinding.toString(), minOut: n.minOut.toString(), deadline: Number(n.deadline) },
    commitment: n.commitment.toString(), nullifier: nullifierOf(h, n.commitment, n.index).toString(),
  })),
  pool: { X: X.toString(), Y: Y.toString(), R0: R0.toString(), R1: R1.toString() },
  tNow: Number(NOW), N,
  clearing: { pLo: c.pLo.toString(), pRem: c.pRem.toString(), payouts: c.out.map((o) => o.o.toString()), netIn0: c.netIn0.toString(), netIn1: c.netIn1.toString(), netOut0: c.netOut0.toString(), netOut1: c.netOut1.toString() },
  order: items.map((i) => i.note.label),
  witness: stringify(input),
};
mkdirSync("../bots/automator/testdata", { recursive: true });
writeFileSync("../bots/automator/testdata/golden.json", JSON.stringify(out, null, 1));
console.log("golden vectors written; order:", out.order.join(","));
