// Reference implementation of the note model + incremental Merkle tree, byte-compatible with
// contracts/src/privacy/NoteTree.sol. Used by the circuit tests and by the Foundry fixture generator.
import { buildPoseidon } from "circomlibjs";
import { keccak256, toBytes, encodeAbiParameters, parseAbiParameters } from "viem";

export const FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617n;
export const DEPTH = 20;
export const ZERO = BigInt(keccak256(toBytes("StrawberrySwap.NoteTree.zero"))) % FIELD;

let P;
export async function init() {
  P = await buildPoseidon();
  const F = P.F;
  const h = (...xs) => BigInt(F.toString(P(xs.map((x) => F.e(x)))));
  return h;
}

export class Tree {
  constructor(h) {
    this.h = h;
    this.leaves = [];
    this.zeros = [ZERO];
    for (let i = 1; i <= DEPTH; i++) this.zeros.push(h(this.zeros[i - 1], this.zeros[i - 1]));
  }
  get root() { return this.layer(DEPTH)[0]; }
  // all nodes at a given height (0 = leaves), padded lazily with zero subtrees
  layer(level) {
    let cur = this.leaves.slice();
    for (let l = 0; l < level; l++) {
      const next = [];
      for (let i = 0; i < Math.max(1, Math.ceil(cur.length / 2)); i++) {
        next.push(this.h(cur[2 * i] ?? this.zeros[l], cur[2 * i + 1] ?? this.zeros[l]));
      }
      cur = next;
    }
    return cur.length ? cur : [this.zeros[level]];
  }
  insert(leaf) { this.leaves.push(leaf); return this.leaves.length - 1; }
  path(index) {
    const pathElements = [], pathIndices = [];
    let idx = index;
    for (let l = 0; l < DEPTH; l++) {
      const layer = l === 0 ? this.leaves : this.layer(l);
      const sib = idx ^ 1;
      pathElements.push(layer[sib] ?? this.zeros[l]);
      pathIndices.push(idx & 1);
      idx >>= 1;
    }
    return { pathElements, pathIndices };
  }
}

export const rand = () => BigInt("0x" + [...crypto.getRandomValues(new Uint8Array(31))].map((b) => b.toString(16).padStart(2, "0")).join(""));

export function makeNote(h, { assetId = 0n, value, privKey = rand(), blinding = rand() }) {
  const pubKey = h(privKey);
  return { assetId, value, privKey, pubKey, blinding, commitment: h(assetId, value, pubKey, blinding) };
}

export const dummyNote = (h, assetId = 0n) => makeNote(h, { assetId, value: 0n });

// extDataHash exactly as Solidity: uint256(keccak256(abi.encode(addr, bytes, bytes))) % FIELD
export function extDataHash(addr, enc0, enc1) {
  const data = encodeAbiParameters(parseAbiParameters("address, bytes, bytes"), [addr, enc0, enc1]);
  return BigInt(keccak256(data)) % FIELD;
}

/**
 * Build a witness input for JoinSplit.
 * ins: [{note, index?}|null,...] real inputs carry the leaf index in `tree`; null/dummy => zero-value note.
 */
export function buildInput(h, tree, { assetId = 0n, ins, outs, publicIn = 0n, publicOut = 0n, ext = 0n }) {
  const inputs = ins.map((i) => i ?? { note: dummyNote(h, assetId), index: undefined });
  const emptyPath = { pathElements: Array(DEPTH).fill(0n), pathIndices: Array(DEPTH).fill(0) };
  const paths = inputs.map((i) => (i.index === undefined ? emptyPath : tree.path(i.index)));
  const nullifiers = inputs.map((i, k) => {
    const idx = paths[k].pathIndices.reduce((a, b, j) => a + (BigInt(b) << BigInt(j)), 0n);
    return h(i.note.commitment, idx, i.note.privKey);
  });
  return {
    root: tree.root,
    nullifier: nullifiers,
    commitment: outs.map((o) => o.commitment),
    publicIn, publicOut, extDataHash: ext, assetId,
    inPrivKey: inputs.map((i) => i.note.privKey),
    inValue: inputs.map((i) => i.note.value),
    inBlinding: inputs.map((i) => i.note.blinding),
    inPathElements: paths.map((p) => p.pathElements),
    inPathIndices: paths.map((p) => p.pathIndices),
    outPubKey: outs.map((o) => o.pubKey),
    outValue: outs.map((o) => o.value),
    outBlinding: outs.map((o) => o.blinding),
  };
}

export const stringify = (o) => JSON.parse(JSON.stringify(o, (_, v) => (typeof v === "bigint" ? v.toString() : v)));
