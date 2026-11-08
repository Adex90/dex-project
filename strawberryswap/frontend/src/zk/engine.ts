import type { Address, Hex } from "viem";
import { DEPTH, bigintToHex32, randomField } from "./field";
import { buildJoinSplitWitness, type SpendInput } from "./joinsplit";
import {
  deriveKeys, extHashNoAddress, extHashWithAddress, makeIntentNote, makeNote, nullifierOf, openSealedOutput, openSelf, paddingCiphertext,
  sealSelf, toOpening, type IntentOpening, type IntentTerms, type Note, type ShieldedKeys,
} from "./notes";
import { NoteTree } from "./tree";

/** Produces a Groth16 proof (256-byte `abi.encode(a, b, c)`) for the JoinSplit circuit. Implemented in the worker (browser) and in tests (Node). */
export interface Groth16Prover {
  prove(input: Record<string, unknown>, expectedPublic: bigint[]): Promise<Hex>;
}

export interface LeafEvent {
  index: number;
  commitment: bigint;
  encryptedNote: Hex;
  /** Monotonic chain position (block·2^32 + logIndex): lets the engine drop re-delivered events, which leaf indices
   *  alone cannot do once the on-chain tree has rolled over. */
  seq?: bigint;
}

export interface OwnedNoteView {
  commitment: bigint;
  assetId: bigint;
  value: bigint;
  index: number;
  spent: boolean;
  reserved: boolean;
  nullifier: bigint;
}

export type IntentState = "created" | "settled" | "refunded";

export interface IntentView {
  commitment: bigint;
  dir: 0 | 1;
  value: bigint;
  minOut: bigint;
  deadline: bigint;
  index: number;
  state: IntentState;
}

export interface WalletState {
  unlocked: boolean;
  pubKey: bigint | undefined;
  treeSize: number;
  root: bigint;
  notes: OwnedNoteView[];
  intents: IntentView[];
  treeId: number; // ordinal of the current on-chain tree (NoteTree.treeId)
}

/** The exact tuple `DarkPool.depositNote / transferNote / withdrawNote` take as `JoinSplit`. */
export interface JoinSplitCall {
  proof: Hex;
  root: Hex;
  nullifiers: readonly [Hex, Hex];
  commitments: readonly [Hex, Hex];
  encryptedNotes: readonly [Hex, Hex];
}

export interface DepositBundle { assetId: bigint; amount: bigint; js: JoinSplitCall }
export interface IntentBundle { assetId: bigint; js: JoinSplitCall; opening: IntentOpening; intentCommitment: bigint; spentCommitment: bigint }
export interface WithdrawBundle { assetId: bigint; amount: bigint; recipient: Address; js: JoinSplitCall; spentCommitment: bigint }

const TWO_112 = 1n << 112n;

// `tree` is the ordinal of the (rolled-over) on-chain tree that holds the leaf; `index` is the position inside it.
interface Owned { note: Note; index: number; tree: number; spent: boolean }
interface IntentRec { terms: IntentTerms; index: number; tree: number; state: IntentState }

/**
 * The shielded wallet. Everything secret — the derived key, note openings, intent terms — lives in this object, and
 * this object lives in the Web Worker. Only what must be public (proofs, commitments, ciphertexts) and the
 * intent OPENING for the Automator ever leave it.
 */
export class ZkEngine {
  private keys: ShieldedKeys | undefined;
  private tree: NoteTree;
  /** Completed trees (NoteTree rolls over when full); their final roots stay valid on-chain forever. */
  private trees: NoteTree[] = [];
  private lastSeq = -1n;
  private owned = new Map<bigint, Owned>();
  private intents = new Map<bigint, IntentRec>();
  private reserved = new Set<bigint>();

  /** `depth` must equal NoteTree.DEPTH on-chain; tests use smaller trees to exercise rollover. */
  constructor(private readonly prover: Groth16Prover, private readonly depth: number = DEPTH) {
    this.tree = new NoteTree(depth);
  }

  // ------------------------------------------------------------------------------------------ lifecycle
  unlock(signature: Hex): bigint {
    this.lock();
    this.keys = deriveKeys(signature);
    return this.keys.pubKey;
  }

  /** Forget the key, every note and every intent. */
  lock(): void {
    this.keys = undefined;
    this.tree = new NoteTree(this.depth);
    this.trees = [];
    this.lastSeq = -1n;
    this.owned.clear();
    this.intents.clear();
    this.reserved.clear();
  }

  private k(): ShieldedKeys {
    if (!this.keys) throw new Error("shielded wallet is locked");
    return this.keys;
  }

  // ------------------------------------------------------------------------------------------ chain sync
  /**
   * Appends NoteInserted events IN ORDER and recovers whatever belongs to this wallet: notes and intents it sealed
   * to itself, and the sealed outputs the Automator produced for its intents. Throws on an index gap — the caller
   * must then resync from the pool's first block. Events must arrive in CHAIN order: after the on-chain tree rolls
   * over (it was full), indices restart at 0 in a fresh tree.
   */
  async addLeaves(leaves: LeafEvent[]): Promise<WalletState> {
    const { pubKey, privKey } = this.k();
    for (const leaf of leaves) {
      if (leaf.seq !== undefined) {
        if (leaf.seq <= this.lastSeq) continue; // re-delivered (overlapping scan)
        this.lastSeq = leaf.seq;
      }
      if (this.tree.size === 2 ** this.tree.depth && leaf.index === 0) {
        this.trees.push(this.tree);
        this.tree = new NoteTree(this.depth);
      }
      if (leaf.index < this.tree.size) continue; // overlap with a previous scan
      if (leaf.index !== this.tree.size) throw new Error(`leaf gap: expected ${this.tree.size}, got ${leaf.index}`);
      const idx = this.tree.insert(leaf.commitment);
      const tree = this.trees.length;

      const own = await openSelf(privKey, leaf.encryptedNote);
      if (own?.kind === "note") {
        const n = makeNote(own.assetId, own.value, pubKey, own.blinding);
        if (n.commitment === leaf.commitment && n.value > 0n) this.owned.set(n.commitment, { note: n, index: idx, tree, spent: false });
      } else if (own?.kind === "intent") {
        const terms: IntentTerms = { dir: own.dir, value: own.value, destPubKey: pubKey, refundPubKey: pubKey, destBlinding: own.destBlinding, minOut: own.minOut, deadline: own.deadline };
        if (makeIntentNote(terms).commitment === leaf.commitment) this.intents.set(leaf.commitment, { terms, index: idx, tree, state: "created" });
      } else {
        for (const rec of this.intents.values()) {
          if (rec.state !== "created") continue;
          const out = await openSealedOutput(rec.terms.destBlinding, leaf.encryptedNote);
          if (!out) continue;
          const n = makeNote(BigInt(out.assetOut), out.value, pubKey, rec.terms.destBlinding);
          if (n.commitment !== leaf.commitment) continue; // a blob that opens but does not match the leaf: ignore, never trust it
          this.owned.set(n.commitment, { note: n, index: idx, tree, spent: false });
          rec.state = out.assetOut === rec.terms.dir ? "refunded" : "settled";
          break;
        }
      }
    }
    return this.state();
  }

  /** The caller (which can reach the chain) reports which of our nullifiers are spent. */
  markSpent(spent: bigint[]): WalletState {
    const set = new Set(spent);
    for (const [c, o] of this.owned) {
      if (set.has(this.nullifier(o))) {
        o.spent = true;
        this.reserved.delete(c);
      }
    }
    return this.state();
  }

  /** Releases a note reserved by a bundle whose transaction was not sent / failed. */
  release(commitment: bigint): void {
    this.reserved.delete(commitment);
  }

  private nullifier(o: Owned): bigint {
    return nullifierOf(o.note, o.index, this.k().privKey);
  }

  // ------------------------------------------------------------------------------------------ transactions
  /** public → note.  `caller` must be the address that will send depositNote (it is bound into the proof). */
  async proveDeposit(args: { assetId: bigint; amount: bigint; caller: Address }): Promise<DepositBundle> {
    const { pubKey, privKey } = this.k();
    if (args.amount <= 0n || args.amount >= TWO_112) throw new Error("amount out of range");
    const note = makeNote(args.assetId, args.amount, pubKey, randomField());
    const padNote = makeNote(args.assetId, 0n, pubKey, randomField());
    const enc0 = await sealSelf(privKey, { kind: "note", assetId: note.assetId, value: note.value, blinding: note.blinding });
    const enc1 = paddingCiphertext();
    const ext = extHashWithAddress(args.caller, enc0, enc1);
    const js = await this.proveJoinSplit({ assetId: args.assetId, ins: [undefined, undefined], outs: [note, padNote], publicIn: args.amount, publicOut: 0n, ext }, [enc0, enc1]);
    return { assetId: args.assetId, amount: args.amount, js };
  }

  /**
   * note → INTENT NOTE (owned by the unspendable batch key) + change. The returned `opening` is what the Automator
   * receives; it contains no key. The intent's terms are also sealed to ourselves on-chain so a lost session can
   * recover (and resubmit) them from the chain alone.
   */
  async proveIntent(args: { dir: 0 | 1; amount: bigint; minOut: bigint; deadline: bigint; inputCommitment?: bigint }): Promise<IntentBundle> {
    const { pubKey, privKey } = this.k();
    if (args.amount <= 0n || args.amount >= TWO_112 || args.minOut < 0n || args.minOut >= TWO_112) throw new Error("amount or minOut out of range");
    const input = this.pickInput(BigInt(args.dir), args.amount, args.inputCommitment);

    const terms: IntentTerms = { dir: args.dir, value: args.amount, destPubKey: pubKey, refundPubKey: pubKey, destBlinding: randomField(), minOut: args.minOut, deadline: args.deadline };
    const intentNote = makeIntentNote(terms);
    const changeValue = input.note.value - args.amount;
    const change = makeNote(BigInt(args.dir), changeValue, pubKey, randomField());
    const enc0 = await sealSelf(privKey, { kind: "intent", dir: args.dir, value: args.amount, destBlinding: terms.destBlinding, minOut: args.minOut, deadline: args.deadline });
    const enc1 = changeValue > 0n ? await sealSelf(privKey, { kind: "note", assetId: change.assetId, value: change.value, blinding: change.blinding }) : paddingCiphertext();
    const ext = extHashNoAddress(enc0, enc1);

    this.reserved.add(input.note.commitment);
    try {
      const spend: SpendInput = { note: input.note, index: input.index, privKey };
      const js = await this.proveJoinSplit({ assetId: BigInt(args.dir), ins: [spend, undefined], outs: [intentNote, change], publicIn: 0n, publicOut: 0n, ext }, [enc0, enc1], input.tree);
      return { assetId: BigInt(args.dir), js, opening: toOpening(terms), intentCommitment: intentNote.commitment, spentCommitment: input.note.commitment };
    } catch (e) {
      this.reserved.delete(input.note.commitment);
      throw e;
    }
  }

  /** note → public. `recipient` is bound into the proof, so a relayer cannot redirect the funds. */
  async proveWithdraw(args: { commitment: bigint; recipient: Address }): Promise<WithdrawBundle> {
    const { pubKey, privKey } = this.k();
    const o = this.owned.get(args.commitment);
    if (!o || o.spent || this.reserved.has(args.commitment)) throw new Error("note is not spendable");
    const z0 = makeNote(o.note.assetId, 0n, pubKey, randomField());
    const z1 = makeNote(o.note.assetId, 0n, pubKey, randomField());
    const enc0 = paddingCiphertext();
    const enc1 = paddingCiphertext();
    const ext = extHashWithAddress(args.recipient, enc0, enc1);
    this.reserved.add(args.commitment);
    try {
      const js = await this.proveJoinSplit({ assetId: o.note.assetId, ins: [{ note: o.note, index: o.index, privKey }, undefined], outs: [z0, z1], publicIn: 0n, publicOut: o.note.value, ext }, [enc0, enc1], o.tree);
      return { assetId: o.note.assetId, amount: o.note.value, recipient: args.recipient, js, spentCommitment: args.commitment };
    } catch (e) {
      this.reserved.delete(args.commitment);
      throw e;
    }
  }

  /** Openings of intents that exist on-chain but are still unsettled — lets the user resubmit after a lost session. */
  pendingOpenings(): { commitment: bigint; opening: IntentOpening; deadline: bigint }[] {
    return [...this.intents].filter(([, r]) => r.state === "created").map(([commitment, r]) => ({ commitment, opening: toOpening(r.terms), deadline: r.terms.deadline }));
  }

  // ------------------------------------------------------------------------------------------ internals
  private pickInput(assetId: bigint, amount: bigint, wanted?: bigint): Owned {
    const usable = [...this.owned.values()].filter((o) => !o.spent && !this.reserved.has(o.note.commitment) && o.note.assetId === assetId && o.note.value >= amount);
    const chosen = wanted !== undefined ? usable.find((o) => o.note.commitment === wanted) : usable.sort((a, b) => (a.note.value < b.note.value ? -1 : 1))[0];
    if (!chosen) throw new Error("no single shielded note covers this amount — deposit more, or merge notes");
    return chosen;
  }

  /** Proves against the tree that holds the spent note (a completed tree's final root is still accepted on-chain). */
  private async proveJoinSplit(spec: Parameters<typeof buildJoinSplitWitness>[1], enc: [Hex, Hex], tree = this.trees.length): Promise<JoinSplitCall> {
    const w = buildJoinSplitWitness(tree < this.trees.length ? (this.trees[tree] as NoteTree) : this.tree, spec);
    const proof = await this.prover.prove(w.input, w.publicSignals);
    return {
      proof, root: bigintToHex32(w.root), nullifiers: [bigintToHex32(w.nullifiers[0]), bigintToHex32(w.nullifiers[1])],
      commitments: [bigintToHex32(spec.outs[0].commitment), bigintToHex32(spec.outs[1].commitment)], encryptedNotes: enc,
    };
  }

  state(): WalletState {
    const keys = this.keys;
    return {
      unlocked: !!keys, pubKey: keys?.pubKey, treeSize: this.tree.size, root: this.tree.root, treeId: this.trees.length,
      notes: keys
        ? [...this.owned.values()].map((o) => ({
            commitment: o.note.commitment, assetId: o.note.assetId, value: o.note.value, index: o.index, spent: o.spent,
            reserved: this.reserved.has(o.note.commitment), nullifier: this.nullifier(o),
          }))
        : [],
      intents: [...this.intents].map(([commitment, r]) => ({ commitment, dir: r.terms.dir, value: r.terms.value, minOut: r.terms.minOut, deadline: r.terms.deadline, index: r.index, state: r.state })),
    };
  }
}
