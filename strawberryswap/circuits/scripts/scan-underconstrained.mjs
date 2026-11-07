// Exact single-wire under-constraint scan over the COMPILED R1CS (what the verifier key encodes).
// For every prover-controlled wire i and every constraint k it appears in, with a valid witness w:
//     g_k(δ) = (A_k·w + a_i δ)(B_k·w + b_i δ) − (C_k·w + c_i δ) = δ·(L_k + δ·Q_k),
//     L_k = A_k·w·b_i + B_k·w·a_i − c_i,   Q_k = a_i·b_i         (A_k·w·B_k·w = C_k·w holds at w)
// so the set of δ ≠ 0 that keep constraint k satisfied is: everything (L=Q=0), nothing (Q=0, L≠0) or {−L/Q}.
// Wire i admits a SECOND valid value (holding every other wire and every public input fixed) iff the
// intersection over its constraints is non-empty. Unlike random fuzzing this is exhaustive for one-wire changes.
// Run from circuits/:  node scripts/scan-underconstrained.mjs   (after compile; batch_swap_n4 needs its wasm)
import * as snarkjs from "snarkjs";
import { readFileSync } from "node:fs";
import { init, Tree, makeNote, dummyNote, buildInput, stringify, rand, FIELD as P } from "../test/lib.mjs";
import { intentNote, buildBatchInput } from "../test/batch.lib.mjs";

const md = (a) => ((a % P) + P) % P;
const inv = (a) => { let r = 1n, b = md(a), e = P - 2n; while (e > 0n) { if (e & 1n) r = (r * b) % P; b = (b * b) % P; e >>= 1n; } return r; };
const quiet = { info() {}, warn() {}, debug() {}, error: console.error };

async function scan(name, base, input) {
  const wasm = `${base}_js/${base.split("/").pop()}.wasm`;
  const wt = { type: "mem" };
  await snarkjs.wtns.calculate(stringify(input), wasm, wt);
  const w = (await snarkjs.wtns.exportJson(wt, quiet)).map(BigInt);
  const r = await snarkjs.r1cs.exportJson(`${base}.r1cs`, quiet);
  const names = new Map();
  for (const line of readFileSync(`${base}.sym`, "utf8").split("\n")) {
    const [, v, , n] = line.split(","); if (n && v !== "-1" && !names.has(Number(v))) names.set(Number(v), n);
  }
  const lc = (t) => Object.entries(t).reduce((s, [j, c]) => s + BigInt(c) * w[Number(j)], 0n) % P;
  const vals = r.constraints.map(([A, B, C]) => [lc(A), lc(B), lc(C)]);
  vals.forEach(([a, b, c], k) => { if (md(a * b - c) !== 0n) throw new Error(`${name}: witness violates constraint ${k}`); });
  const occ = new Map();
  r.constraints.forEach((abc, k) => abc.forEach((t, s) => { for (const j of Object.keys(t)) { const i = Number(j); if (!occ.has(i)) occ.set(i, new Map()); const m = occ.get(i); if (!m.has(k)) m.set(k, [0n, 0n, 0n]); m.get(k)[s] = BigInt(t[j]); } }));
  const firstPrivate = 1 + r.nOutputs + r.nPubInputs;
  const free = [], unconstrained = [];
  for (let i = firstPrivate; i < r.nVars; i++) {
    const cons = occ.get(i);
    if (!cons) { unconstrained.push(i); continue; }
    let cand = null, any = true, dead = false;
    for (const [k, [ai, bi, ci]] of cons) {
      const [A, B] = vals[k];
      const L = md(A * bi + B * ai - ci), Q = md(ai * bi);
      if (Q === 0n) { if (L !== 0n) { dead = true; break; } continue; }
      const d = md(-L * inv(Q));
      if (d === 0n) { dead = true; break; }
      if (any) { cand = d; any = false; } else if (d !== cand) { dead = true; break; }
    }
    if (!dead) free.push([i, any ? "any δ" : `δ=${cand > P / 2n ? "-" + (P - cand) : cand}`]);
  }
  const label = (i) => names.get(i) ?? `wire#${i}`;
  const benign = (n) => /\.isz?\.inv$|IsZero.*inv|\.inv$/.test(n);
  const real = free.filter(([i]) => !benign(label(i)));
  console.log(`\n== ${name}: ${r.nConstraints} constraints, ${r.nVars} wires (${r.nPubInputs} public)`);
  console.log(`   wires in no constraint at all     : ${unconstrained.length}${unconstrained.length ? "  e.g. " + unconstrained.slice(0, 5).map(label).join(", ") : ""}`);
  console.log(`   one-wire alternative witnesses    : ${free.length}  (${free.length - real.length} are IsZero 'inv' hints at in = 0: by design, they feed no other constraint)`);
  for (const [i, d] of real.slice(0, 20)) console.log(`     !! ${label(i)}  (${d})`);
  const ben = [...new Set(free.filter(([i]) => benign(label(i))).map(([i]) => label(i).replace(/\[\d+\]/g, "[]")))];
  console.log(`     hint wires (deduplicated): ${ben.join(" | ")}`);
  return { real: real.length, unconstrained: unconstrained.length };
}

const h = await init();
let bad = 0;
{ // JoinSplit: a real 2-in transfer (exercises membership on both inputs)
  const tree = new Tree(h);
  const a = makeNote(h, { value: 100n }), b = makeNote(h, { value: 7n });
  const ia = tree.insert(a.commitment), ib = tree.insert(b.commitment);
  const inp = buildInput(h, tree, { ins: [{ note: a, index: ia }, { note: b, index: ib }], outs: [makeNote(h, { value: 50n }), makeNote(h, { value: 57n })], ext: 12345n });
  const s = await scan("joinsplit (2 real inputs)", "build/joinsplit/joinsplit", inp); bad += s.real + s.unconstrained;
  const inp2 = buildInput(h, tree, { ins: [null, null], outs: [makeNote(h, { value: 9n }), dummyNote(h)], publicIn: 9n, ext: 1n });
  const s2 = await scan("joinsplit (shield, 2 dummy inputs)", "build/joinsplit/joinsplit", inp2); bad += s2.real + s2.unconstrained;
}
{ // BatchSwap N=4: swap both ways + refund + padding
  const E = 10n ** 18n, X = 3_000_000n * E, Y = 6_000_000n * E, NOW = 1_800_000_000n;
  const tree = new Tree(h);
  const mk = (o) => { const n = intentNote(h, { destPubKey: rand(), refundPubKey: rand(), destBlinding: rand(), minOut: 0n, deadline: NOW + 3600n, ...o }); n.index = tree.insert(n.commitment); return n; };
  const s0 = mk({ dir: 0, value: 1000n * E }), s1 = mk({ dir: 1, value: 800n * E }), ex = mk({ dir: 0, value: 77n * E, deadline: NOW - 1n });
  const inp = buildBatchInput(h, tree, [{ mode: "swap", note: s0, index: s0.index }, { mode: "swap", note: s1, index: s1.index }, { mode: "refund", note: ex, index: ex.index }], { X, Y, tNow: NOW, N: 4 });
  const s = await scan("batch_swap N=4 (2 swaps, 1 refund, 1 padding)", "build/batch_swap_n4/main_n4", inp); bad += s.real + s.unconstrained;
}
{ // zk-DECO
  const bytes = (str) => [...Buffer.from(str, "latin1")];
  const pack = (arr) => arr.reduce((a, b, i) => a + (BigInt(b) << BigInt(8 * i)), 0n);
  const GOOD = '{"symbol":"XAUUSD","price":2345.678912,"ts":1767225600}';
  const body = bytes(GOOD); while (body.length < 96) body.push(0);
  const salt = rand();
  const inp = { providerId: 1n, queryCommitment: h(pack(bytes("XAUUSD")), salt), price: 2345678912n * 10n ** 12n, observedAt: 1767225600n,
    bodyCommitment: h(...[0, 1, 2, 3].map((j) => pack(body.slice(24 * j, 24 * j + 24)))), body, symbol: bytes("XAUUSD"), salt,
    offSymKey: BigInt(GOOD.indexOf('"symbol":"')), offPriceKey: BigInt(GOOD.indexOf('"price":')), offTsKey: BigInt(GOOD.indexOf('"ts":')), intLen: 4n, fracLen: 6n };
  const s = await scan("zkdeco_oracle", "build/zkdeco_oracle/zkdeco_oracle", inp); bad += s.real + s.unconstrained;
}
console.log(`\nnon-benign single-wire freedoms or unconstrained wires: ${bad}`);
process.exit(0);
