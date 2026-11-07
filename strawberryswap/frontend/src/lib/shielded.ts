import { parseAbiItem, type Address, type Hex } from "viem";
import { darkPoolAbi } from "./abi";
import { publicClient } from "./clients";
import { config } from "./config";
import { bigintToHex32 } from "../zk/field";
import type { JoinSplitCall, LeafEvent } from "../zk/engine";

const noteInserted = parseAbiItem("event NoteInserted(uint32 indexed leafIndex, bytes32 commitment, bytes encryptedNote)");

export interface ChainHead {
  number: bigint;
  timestamp: bigint;
}

export async function chainHead(): Promise<ChainHead> {
  const b = await publicClient.getBlock({ blockTag: "latest" });
  return { number: b.number, timestamp: b.timestamp };
}

/** NoteInserted events in [from, to], oldest first, scanned in `logChunk`-block windows. */
export async function fetchLeaves(pool: Address, from: bigint, to: bigint, onChunk?: (scannedTo: bigint) => void): Promise<LeafEvent[]> {
  const out: LeafEvent[] = [];
  for (let start = from; start <= to; start += config.logChunk) {
    const end = start + config.logChunk - 1n < to ? start + config.logChunk - 1n : to;
    const logs = await publicClient.getLogs({ address: pool, event: noteInserted, fromBlock: start, toBlock: end });
    for (const l of logs) {
      const { leafIndex, commitment, encryptedNote } = l.args;
      if (leafIndex === undefined || commitment === undefined || encryptedNote === undefined) throw new Error("malformed NoteInserted log");
      out.push({ index: Number(leafIndex), commitment: BigInt(commitment), encryptedNote });
    }
    onChunk?.(end);
  }
  return out.sort((a, b) => a.index - b.index);
}

export async function poolRoot(pool: Address): Promise<bigint> {
  return BigInt(await publicClient.readContract({ address: pool, abi: darkPoolAbi, functionName: "currentRoot" }));
}

/** Which of the wallet's nullifiers are already spent on-chain. */
export async function spentAmong(pool: Address, nullifiers: bigint[]): Promise<bigint[]> {
  const flags = await Promise.all(nullifiers.map((n) => publicClient.readContract({ address: pool, abi: darkPoolAbi, functionName: "nullifierSpent", args: [bigintToHex32(n)] })));
  return nullifiers.filter((_, i) => flags[i]);
}

/** Engine bundle → the `JoinSplit` struct argument (same field names as DarkPool.JoinSplit). */
export function toJoinSplitArg(js: JoinSplitCall): { proof: Hex; root: Hex; nullifiers: readonly [Hex, Hex]; commitments: readonly [Hex, Hex]; encryptedNotes: readonly [Hex, Hex] } {
  return { proof: js.proof, root: js.root, nullifiers: js.nullifiers, commitments: js.commitments, encryptedNotes: js.encryptedNotes };
}
