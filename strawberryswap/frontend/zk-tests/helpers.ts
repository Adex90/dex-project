import { toBytes32 } from "../src/zk/field";
import type { Groth16Prover, LeafEvent } from "../src/zk/engine";
import type { Hex } from "viem";

export const SIG_A = ("0x" + "11".repeat(65)) as Hex;
export const SIG_B = ("0x" + "22".repeat(65)) as Hex;
export const ALICE = "0x00000000000000000000000000000000000A11cE" as const;
export const U = 10n ** 18n;

/** Records the public signals it is asked to prove; returns a dummy proof (logic tests only). */
export class RecordingProver implements Groth16Prover {
  calls: { input: Record<string, unknown>; pub: bigint[] }[] = [];
  async prove(input: Record<string, unknown>, pub: bigint[]): Promise<Hex> {
    this.calls.push({ input, pub });
    return ("0x" + "00".repeat(256)) as Hex;
  }
}

/** A chain's NoteInserted stream: commitments are inserted in order, exactly as the contract does. */
export class FakeChain {
  leaves: LeafEvent[] = [];
  constructor(private readonly depth = 20) {}
  push(commitment: bigint, encryptedNote: Hex): LeafEvent {
    // indices restart on rollover; seq is the chain position (as fetchLeaves derives it from block + log index)
    const ev = { index: this.leaves.length % 2 ** this.depth, commitment, encryptedNote, seq: BigInt(this.leaves.length) };
    this.leaves.push(ev);
    return ev;
  }
  pushPair(commitments: readonly [Hex, Hex], enc: readonly [Hex, Hex]): void {
    this.push(BigInt(commitments[0]), enc[0]);
    this.push(BigInt(commitments[1]), enc[1]);
  }
}

/** Same format as Go `intent.Seal`: AES-256-GCM, key = SHA-256("StrawberrySwap.outnote.v1" ‖ blinding32), nonce ‖ ct(asset ‖ value) ‖ tag. */
export async function sealLikeAutomator(destBlinding: bigint, assetOut: 0 | 1, value: bigint): Promise<Hex> {
  const key = await crypto.subtle.importKey(
    "raw", await crypto.subtle.digest("SHA-256", new Uint8Array([...new TextEncoder().encode("StrawberrySwap.outnote.v1"), ...toBytes32(destBlinding)])), "AES-GCM", false, ["encrypt"],
  );
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const pt = new Uint8Array([assetOut, ...toBytes32(value)]);
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, pt));
  return ("0x" + [...nonce, ...ct].map((b) => b.toString(16).padStart(2, "0")).join("")) as Hex;
}
