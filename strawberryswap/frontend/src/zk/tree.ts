import { DEPTH, TREE_ZERO } from "./field";
import { H } from "./poseidon";

/**
 * Local mirror of contracts/src/privacy/NoteTree.sol: incremental binary tree, Poseidon(2), depth 20, zero value
 * keccak("StrawberrySwap.NoteTree.zero") mod p. Used to build Merkle paths for the JoinSplit circuit.
 */
export class NoteTree {
  private readonly zeros: bigint[];
  private readonly layers: bigint[][];
  private readonly byCommitment = new Map<bigint, number>();

  constructor(readonly depth: number = DEPTH) {
    this.zeros = [TREE_ZERO];
    for (let l = 1; l <= depth; l++) {
      const z = this.zeros[l - 1] as bigint;
      this.zeros.push(H(z, z));
    }
    this.layers = Array.from({ length: depth + 1 }, () => []);
  }

  get size(): number {
    return (this.layers[0] as bigint[]).length;
  }

  get root(): bigint {
    return (this.layers[this.depth] as bigint[])[0] ?? (this.zeros[this.depth] as bigint);
  }

  private node(level: number, i: number): bigint {
    return (this.layers[level] as bigint[])[i] ?? (this.zeros[level] as bigint);
  }

  /** Appends a leaf, returns its index. */
  insert(leaf: bigint): number {
    const idx = this.size;
    if (idx >= 2 ** this.depth) throw new Error("note tree is full");
    (this.layers[0] as bigint[]).push(leaf);
    if (!this.byCommitment.has(leaf)) this.byCommitment.set(leaf, idx);
    let cur = idx;
    for (let l = 0; l < this.depth; l++) {
      const h = H(this.node(l, cur & ~1), this.node(l, cur | 1));
      const parent = cur >> 1;
      (this.layers[l + 1] as bigint[])[parent] = h;
      cur = parent;
    }
    return idx;
  }

  indexOf(commitment: bigint): number | undefined {
    return this.byCommitment.get(commitment);
  }

  /** Siblings and index bits (bit l = 1 ⇒ the running node is a RIGHT child at level l), as the circuit expects. */
  path(index: number): { elements: bigint[]; bits: number[] } {
    if (index < 0 || index >= this.size) throw new Error(`leaf index ${index} out of range`);
    const elements: bigint[] = [];
    const bits: number[] = [];
    let cur = index;
    for (let l = 0; l < this.depth; l++) {
      elements.push(this.node(l, cur ^ 1));
      bits.push(cur & 1);
      cur >>= 1;
    }
    return { elements, bits };
  }
}
