import { randomField } from "./field";
import { makeNote, nullifierOf, type Note } from "./notes";
import { H } from "./poseidon";
import type { NoteTree } from "./tree";

/** A real input: a note we own, its tree position, and the private key that spends it. */
export interface SpendInput {
  note: Note;
  index: number;
  privKey: bigint;
}

export interface JoinSplitSpec {
  assetId: bigint;
  ins: [SpendInput | undefined, SpendInput | undefined]; // undefined = dummy zero-value input
  outs: [Note, Note];
  publicIn: bigint;
  publicOut: bigint;
  ext: bigint;
}

export interface JoinSplitWitness {
  /** snarkjs input.json (decimal strings). Contains private data — never leaves the worker. */
  input: Record<string, unknown>;
  /** The 9 public signals in the order the verifier expects: root, n0, n1, c0, c1, publicIn, publicOut, extDataHash, assetId. */
  publicSignals: bigint[];
  root: bigint;
  nullifiers: [bigint, bigint];
}

const s = (x: bigint): string => x.toString();

/**
 * Builds the witness for circuits/joinsplit. Mirrors circuits/test/lib.mjs `buildInput` (checked by tests).
 * Statement proved: ownership, Merkle membership of non-dummy inputs, nullifiers, 128-bit-range value
 * conservation Σin + publicIn = Σout + publicOut, well-formed outputs of the same asset.
 */
export function buildJoinSplitWitness(tree: NoteTree, spec: JoinSplitSpec): JoinSplitWitness {
  const zeroPath = { elements: new Array<bigint>(tree.depth).fill(0n), bits: new Array<number>(tree.depth).fill(0) };
  const rows = spec.ins.map((i) => {
    if (i) {
      const p = tree.path(i.index);
      return { note: i.note, priv: i.privKey, path: p, index: i.index };
    }
    // dummy input: zero value, random key/blinding, skips the membership check but still yields a unique nullifier
    const priv = randomField();
    return { note: makeNote(spec.assetId, 0n, H(priv), randomField()), priv, path: zeroPath, index: 0 };
  });
  const nullifiers = rows.map((r) => nullifierOf(r.note, r.path === zeroPath ? 0 : r.index, r.priv)) as [bigint, bigint];
  const root = tree.root;
  const input = {
    root: s(root), nullifier: nullifiers.map(s), commitment: spec.outs.map((o) => s(o.commitment)),
    publicIn: s(spec.publicIn), publicOut: s(spec.publicOut), extDataHash: s(spec.ext), assetId: s(spec.assetId),
    inPrivKey: rows.map((r) => s(r.priv)), inValue: rows.map((r) => s(r.note.value)), inBlinding: rows.map((r) => s(r.note.blinding)),
    inPathElements: rows.map((r) => r.path.elements.map(s)), inPathIndices: rows.map((r) => r.path.bits.map(String)),
    outPubKey: spec.outs.map((o) => s(o.pubKey)), outValue: spec.outs.map((o) => s(o.value)), outBlinding: spec.outs.map((o) => s(o.blinding)),
  };
  return {
    input, root, nullifiers,
    publicSignals: [root, nullifiers[0], nullifiers[1], spec.outs[0].commitment, spec.outs[1].commitment, spec.publicIn, spec.publicOut, spec.ext, spec.assetId],
  };
}
