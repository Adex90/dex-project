// PHASE 8 AUDIT — ZK-P1: intent nullifiers are computable by ANY observer from public chain data, because the
// nullifier key BATCH_NK is a public constant: nullifier = Poseidon(commitment, leafIndex, BATCH_NK).
// Input: ONLY what the chain publishes in the real-proof E2E (contracts/test/fixtures/BatchFixtures.sol):
//   NoteInserted(leafIndex, commitment) for every deposit/transfer, the tx sender, and settleBatch's nullifier[].
// Output: which address's transaction created each batch slot. Run from circuits/: node audit/intent_linkability.mjs
import { readFileSync } from "node:fs";
import { init } from "../test/lib.mjs";
import { BATCH_NK } from "../test/batch.lib.mjs";

const src = readFileSync("../contracts/test/fixtures/BatchFixtures.sol", "utf8");
const fn = (name) => src.slice(src.indexOf(`function ${name}()`), src.indexOf("}\n", src.indexOf(`function ${name}()`)));
const steps = (name) => [...fn(name).matchAll(/actor: (0x[0-9a-fA-F]{40}).*?c0: (0x[0-9a-f]{64}), c1: (0x[0-9a-f]{64})/g)].map((m) => ({ actor: m[1], c: [BigInt(m[2]), BigInt(m[3])] }));
const batchNullifiers = [...fn("batchNullifiers").matchAll(/= (0x[0-9a-f]{64});/g)].map((m) => BigInt(m[1]));

const h = await init();
// replay the public event log: every JoinSplit inserts c0 then c1
const leaves = [];
for (const [kind, s] of [...steps("deposits").map((s) => ["depositNote", s]), ...steps("transfers").map((s) => ["transferNote", s])])
  for (const c of s.c) leaves.push({ index: leaves.length, commitment: c, kind, sender: s.actor });

console.log(`observer sees ${leaves.length} leaves and a batch with ${batchNullifiers.length} nullifiers`);
let linked = 0;
batchNullifiers.forEach((nf, slot) => {
  const hit = leaves.find((l) => h(l.commitment, BigInt(l.index), BATCH_NK) === nf);
  if (hit) { linked++; console.log(`  batch slot ${slot}  <=  leaf ${hit.index} created by ${hit.kind} from ${hit.sender}`); }
  else console.log(`  batch slot ${slot}  <=  (not linked)`);
});
console.log(`${linked}/${batchNullifiers.length} batch slots de-anonymised from public data alone.`);
process.exit(linked > 0 ? 0 : 1);
