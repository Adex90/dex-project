// Reference implementation of the intent-note model and the batch clearing maths. The circuit, the Go Automator
// (bots/automator) and the Foundry fixtures are all checked against THIS file (and against each other).
import { keccak256, toBytes } from "viem";
import { FIELD, DEPTH, dummyNote } from "./lib.mjs";

const lab = (s) => BigInt(keccak256(toBytes(s))) % FIELD;
export const BATCH_PUBKEY = lab("StrawberrySwap.batch.pubkey");
export const BATCH_NK = lab("StrawberrySwap.batch.nullifierKey");
export const WAD = 10n ** 18n;

/** Intent note: a note owned by the unspendable BATCH_PUBKEY whose blinding commits to the swap terms. */
export function intentNote(h, { dir, value, destPubKey, refundPubKey, destBlinding, minOut, deadline }) {
  const intentBlinding = h(destPubKey, refundPubKey, destBlinding, minOut, deadline);
  return {
    dir, value, destPubKey, refundPubKey, destBlinding, minOut, deadline,
    // as an ordinary note (what the user's JoinSplit must create):
    assetId: BigInt(dir), pubKey: BATCH_PUBKEY, blinding: intentBlinding,
    commitment: h(BigInt(dir), value, BATCH_PUBKEY, intentBlinding),
  };
}

export const nullifierOf = (h, commitment, leafIndex) => h(commitment, BigInt(leafIndex), BATCH_NK);

/** Pure clearing maths. swaps: [{dir, value}] -> price, payouts, nets. Mirrors the circuit and the Go batcher. */
export function clear(X, Y, swaps, adv = {}) {
  let A0 = 0n, A1 = 0n;
  for (const s of swaps) (s.dir === 0 ? (A0 += s.value) : (A1 += s.value));
  const D = X + A0, Nn = Y + A1;
  let pLo = (WAD * Nn) / D, pRem = (WAD * Nn) % D;
  // ADVERSARIAL knobs (tests only): a malicious prover who keeps every equation consistent by shifting remainders.
  if (adv.pLoDelta) { pLo -= adv.pLoDelta; pRem += adv.pLoDelta * D; }   // understate the clearing price
  const pHi = pLo + 1n;
  let O0 = 0n, O1 = 0n;
  const out = swaps.map((s, i) => {
    const [num, den] = s.dir === 0 ? [pLo, WAD] : [WAD, pHi];
    let o = (s.value * num) / den, rem = (s.value * num) % den;
    if (adv.skim === i) { o -= 1n; rem += den; }                           // skim 1 wei, remainder 'fixed up'
    s.dir === 0 ? (O1 += o) : (O0 += o);
    return { o, rem };
  });
  const d0 = A0 - O0, d1 = A1 - O1;
  return {
    A0, A1, O0, O1, pLo, pRem, pHi, out,
    netIn0: d0 > 0n ? d0 : 0n, netOut0: d0 < 0n ? -d0 : 0n,
    netIn1: d1 > 0n ? d1 : 0n, netOut1: d1 < 0n ? -d1 : 0n,
  };
}

/**
 * Build the circuit input for N slots. items: [{mode:'swap'|'refund', note: intentNote, index}] — active slots first.
 */
export function buildBatchInput(h, tree, items, { X, Y, tNow, N, adv = {} }) {
  if (items.length > N) throw new Error("too many items");
  const swaps = items.filter((i) => i.mode === "swap").map((i) => ({ dir: i.note.dir, value: i.note.value }));
  const c = clear(X, Y, swaps, adv);
  let si = 0;
  const slots = items.map((it) => {
    const n = it.note;
    const idx = BigInt(it.index);
    const path = tree.path(it.index);
    const nf = nullifierOf(h, n.commitment, it.index);
    const isSwap = it.mode === "swap";
    const { o, rem } = isSwap ? c.out[si++] : { o: 0n, rem: 0n };
    const outAsset = isSwap ? BigInt(1 - n.dir) : BigInt(n.dir);
    const outValue = isSwap ? o : n.value;
    const outPub = isSwap ? n.destPubKey : n.refundPubKey;
    return {
      isSwap: isSwap ? 1n : 0n, isRefund: isSwap ? 0n : 1n, dir: BigInt(n.dir), value: n.value, outValue: o, rem,
      destPubKey: n.destPubKey, refundPubKey: n.refundPubKey, destBlinding: n.destBlinding, minOut: n.minOut, deadline: n.deadline,
      pathElements: path.pathElements, pathIndices: path.pathIndices.map(BigInt),
      nullifier: nf, commitment: h(outAsset, outValue, outPub, n.destBlinding),
    };
  });
  const zeroPath = { pathElements: Array(DEPTH).fill(0n), pathIndices: Array(DEPTH).fill(0n) };
  while (slots.length < N) slots.push({ isSwap: 0n, isRefund: 0n, dir: 0n, value: 0n, outValue: 0n, rem: 0n, destPubKey: 0n, refundPubKey: 0n, destBlinding: 0n, minOut: 0n, deadline: 0n, ...zeroPath, nullifier: 0n, commitment: 0n });
  const col = (k) => slots.map((s) => s[k]);
  return {
    root: tree.root, tNow: BigInt(tNow), X, Y, netIn0: c.netIn0, netIn1: c.netIn1, netOut0: c.netOut0, netOut1: c.netOut1,
    nullifier: col("nullifier"), commitment: col("commitment"),
    isSwap: col("isSwap"), isRefund: col("isRefund"), dir: col("dir"), value: col("value"), outValue: col("outValue"), rem: col("rem"),
    destPubKey: col("destPubKey"), refundPubKey: col("refundPubKey"), destBlinding: col("destBlinding"), minOut: col("minOut"), deadline: col("deadline"),
    pathElements: col("pathElements"), pathIndices: col("pathIndices"),
    pLo: c.pLo, pRem: c.pRem,
  };
}
