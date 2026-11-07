// Adversarial tests for the BatchSwap circuit (N = 4). A violated constraint => no witness => no proof.
import * as snarkjs from "snarkjs";
import { init, Tree, stringify, FIELD, rand } from "./lib.mjs";
import { keccak256, toBytes } from "viem";
import { intentNote, buildBatchInput, clear, WAD, BATCH_PUBKEY } from "./batch.lib.mjs";

const N = Number(process.env.BATCH_N ?? 4);
const WASM = process.env.BATCH_WASM ?? `build/batch_swap_n${N}/main_n${N}_js/main_n${N}.wasm`;
let pass = 0, fail = 0;
const run = (i) => snarkjs.wtns.calculate(stringify(i), WASM, { type: "mem" });
const accepts = async (n, i) => { try { await run(i); pass++; console.log("  ok   accepts ", n); } catch (e) { fail++; console.log("  FAIL accepts ", n, String(e.message).split("\n")[0]); } };
const rejects = async (n, i) => {
  try { await run(i); fail++; console.log("  FAIL rejects ", n, "(ACCEPTED!)"); }
  catch (e) {
    // only a violated CONSTRAINT counts as a rejection; I/O or JS errors must never masquerade as one
    if (!/Assert Failed|Error in template/.test(String(e.message))) { fail++; console.log("  FAIL rejects ", n, "-> not a constraint failure:", String(e.message).split("\n")[0]); return; }
    pass++; const m = String(e.message).match(/template (\w+?)_\d+ line: (\d+)/); console.log("  ok   rejects ", n.padEnd(58), m ? `[${m[1]}:${m[2]}]` : "");
  }
};

const h = await init();
const E = 10n ** 18n;
const X = 3_000_000n * E, Y = 6_000_000n * E;     // effective reserves, spot 2.0
const NOW = 1_800_000_000n, FUTURE = NOW + 3600n, PAST = NOW - 3600n;

// ---- a tree holding intent notes (and one ordinary note) for several users
const tree = new Tree(h);
const mk = (o) => { const n = intentNote(h, { destPubKey: rand(), refundPubKey: rand(), destBlinding: rand(), minOut: 0n, deadline: FUTURE, ...o }); n.index = tree.insert(n.commitment); return n; };
const sell0a = mk({ dir: 0, value: 1_000n * E, minOut: 1_900n * E });     // sells 1000 token0, wants >= 1900 token1
const sell0b = mk({ dir: 0, value: 250n * E });
const sell1a = mk({ dir: 1, value: 800n * E, minOut: 390n * E });         // sells 800 token1, wants >= 390 token0
const sell1b = mk({ dir: 1, value: 40n * E });
const expired = mk({ dir: 0, value: 77n * E, deadline: PAST });           // refundable
const tooTight = mk({ dir: 0, value: 100n * E, minOut: 250n * E });       // wants 250 token1 for 100 token0 (price 2.5): infeasible
const item = (n, mode = "swap") => ({ mode, note: n, index: n.index });
const B = (items, o = {}) => buildBatchInput(h, tree, items, { X, Y, tNow: NOW, N, ...o });

console.log("positive");
await accepts("one swap, one direction", B([item(sell0a)]));
await accepts("two opposite swaps (partial netting)", B([item(sell0a), item(sell1a)]));
await accepts("four swaps, mixed directions", B([item(sell0a), item(sell0b), item(sell1a), item(sell1b)]));
await accepts("swap + refund of an expired intent", B([item(sell0a), item(expired, "refund")]));
await accepts("refund only: nets are zero, price untouched", B([item(expired, "refund")]));
await accepts("empty batch (all padding)", B([]));
{ const c = clear(X, Y, [sell0a, sell1a].map((n) => ({ dir: n.dir, value: n.value })));
  console.log(`       example: clearing price ${Number(c.pLo) / 1e18}, user payouts ${c.out.map((x) => Number(x.o / (E / 1000n)) / 1000)}, net in0/out1 = ${c.netIn0}/${c.netOut1}`); }

console.log("negative");
{ const g = B([item(sell0a)]); g.outValue[0] = BigInt(g.outValue[0]) + 1n;     await rejects("payout inflated by 1 wei", g); }
{ const g = B([item(sell0a)]); g.outValue[0] = BigInt(g.outValue[0]) - 1n;     await rejects("payout skimmed by 1 wei (underpaying the user)", g); }
{ const g = B([item(tooTight)]);                                               await rejects("minOut not met (user's limit price violated)", g); }
{ const g = B([item(sell0a)]); g.pLo = BigInt(g.pLo) + 1n;                     await rejects("clearing price off by 1 (not the curve price)", g); }
{ const g = B([item(sell0a), item(sell1a)]); g.pRem = BigInt(g.pRem) + 1n;      await rejects("price remainder forged", g); }
// A smarter attacker keeps every equation consistent by shifting the remainder. Only the `rem < den` and
// `pRem < D` bounds stop these; mutation testing showed no earlier case exercised them.
await rejects("payout skimmed by 1 wei WITH a consistent remainder (rem += den)", B([item(sell0a), item(sell1a)], { adv: { skim: 0 } }));
await rejects("token1 payout skimmed with a consistent remainder", B([item(sell0a), item(sell1a)], { adv: { skim: 1 } }));
await rejects("clearing price understated by 1 with a consistent remainder", B([item(sell0a), item(sell1a)], { adv: { pLoDelta: 1n } }));
await rejects("clearing price understated by 10^6 (a larger, profitable skew)", B([item(sell0a), item(sell1a)], { adv: { pLoDelta: 1_000_000n } }));
{ const g = B([item(sell0a)], { tNow: Number(FUTURE) + 1 });                    await rejects("swap executed after its deadline", g); }
{ const g = B([item(sell0a, "refund")]);                                        await rejects("refund requested before the deadline", g); }
{ const g = B([item(sell0a)]); g.commitment[0] = (BigInt(g.commitment[0]) + 1n).toString(); await rejects("output commitment redirected", g); }
{ const g = B([item(sell0a)]); g.nullifier[0] = (BigInt(g.nullifier[0]) + 1n).toString();   await rejects("forged nullifier", g); }
{ // Phase-8 audit H-7: the nullifier key used to be a PUBLIC constant, so anyone could recompute every intent's
  // nullifier from NoteInserted events and link each batch slot to the transaction that created it.
  const publicKey = BigInt(keccak256(toBytes("StrawberrySwap.batch.nullifierKey"))) % FIELD;
  const g = B([item(sell0a)]); g.nullifier[0] = h(sell0a.commitment, BigInt(sell0a.index), publicKey).toString();
  await rejects("H-7: nullifier derivable from public data (old public key)", g); }
{ const g = B([item(sell0a)]); g.netIn0 = BigInt(g.netIn0) + 1n;                await rejects("net flow overstated (public)", g); }
{ const g = B([item(sell0a), item(sell1a)]); g.netIn1 = 1n; g.netOut1 = 1n;     await rejects("both netIn and netOut non-zero for one asset", g); }
{ const g = B([item(sell0a, "refund"), item(expired, "refund")]); g.isSwap[0] = 1n; await rejects("slot flagged swap AND refund", g); }
{ const g = B([item(sell0a)]); g.isSwap[0] = 2n;                                await rejects("non-boolean mode flag", g); }
{ const g = B([item(sell0a)]); g.nullifier[1] = 5n;                             await rejects("padding slot with a non-zero nullifier", g); }
{ const g = B([item(sell0a)]); g.commitment[2] = 5n;                            await rejects("padding slot with a non-zero commitment", g); }
{ // spend an intent that was never inserted
  const ghost = intentNote(h, { dir: 0, value: 5_000n * E, destPubKey: rand(), refundPubKey: rand(), destBlinding: rand(), minOut: 0n, deadline: FUTURE });
  ghost.index = 0; await rejects("intent note that is not in the tree", B([item(ghost)])); }
{ // an ordinary user-owned note is not an intent note (pubKey != BATCH_PUBKEY)
  const user = rand(); const cm = h(0n, 500n * E, h(user), rand()); const idx = tree.insert(cm);
  const fake = intentNote(h, { dir: 0, value: 500n * E, destPubKey: rand(), refundPubKey: rand(), destBlinding: rand(), minOut: 0n, deadline: FUTURE });
  fake.commitment = cm; fake.index = idx; await rejects("ordinary note passed off as an intent note", B([item(fake)])); }
{ const g = B([item(sell0a)]); g.value[0] = BigInt(g.value[0]) + 1n;           await rejects("note value tampered", g); }
{ const g = B([item(sell0a)]); g.destPubKey[0] = rand().toString();            await rejects("destination changed after the user committed", g); }
{ const g = B([item(sell0a)]); g.minOut[0] = 0n;                               await rejects("minOut lowered after the user committed", g); }
{ const g = B([item(expired, "refund")]); g.refundPubKey[0] = rand().toString(); await rejects("refund redirected to another key", g); }
{ // token1 seller paid with the pool-UNFAVOURABLE price (pLo instead of pHi)
  const g = B([item(sell0a), item(sell1a)]); const v = BigInt(g.value[1]); const c = clear(X, Y, [sell0a, sell1a].map((n) => ({ dir: n.dir, value: n.value })));
  const o = (v * WAD) / c.pLo; g.outValue[1] = o; g.rem[1] = (v * WAD) % c.pLo; await rejects("token1 payout rounded in the user's favour (pLo)", g); }
{ const g = B([item(sell0a), item(sell1a)]); g.isSwap[1] = 0n; g.isRefund[1] = 0n; await rejects("aggregator silently drops an intent (nets must change)", g); }
{ const big = mk({ dir: 0, value: 1n << 112n }); await rejects("value >= 2^112 (range)", B([item(big)])); }
{ const g = B([item(sell0a), item(sell1a)]); g.X = BigInt(g.X) + 1n;           await rejects("public X differs from the reserve the price was computed on", g); }
{ const g = B([item(sell0a), item(sell1a)]); g.Y = BigInt(g.Y) + 1n;           await rejects("public Y differs from the reserve the price was computed on", g); }
{ // drop an intent CONSISTENTLY (flags + public nullifier/commitment zeroed) but keep the original nets
  const g = B([item(sell0a), item(sell1a)]); g.isSwap[1] = 0n; g.nullifier[1] = 0n; g.commitment[1] = 0n;
  await rejects("consistently dropped intent but original net flow kept", g); }

// ---- property test (pure BigInt, no circuit): K never decreases at the clearing price, for ANY batch
let kChecked = 0;
for (let t = 0; t < 20000; t++) {
  const r = () => BigInt(Math.floor(Math.random() * 2 ** 52)) * BigInt(Math.floor(Math.random() * 2 ** 40) + 1);
  const x = r() + 1_000_000n, y = r() + 1_000_000n;
  const swaps = Array.from({ length: 1 + Math.floor(Math.random() * 16) }, () => ({ dir: Math.random() < 0.5 ? 0 : 1, value: (r() % (x / 4n + 1n)) + 1n }));
  const c = clear(x, y, swaps);
  const x2 = x + c.A0 - c.O0, y2 = y + c.A1 - c.O1;
  if (x2 * y2 < x * y) { fail++; console.log("  FAIL K decreased", { x, y, swaps }); break; }
  kChecked++;
}
pass++; console.log(`  ok   K' >= K at the clearing price for ${kChecked} random batches (pure BigInt property test)`);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
