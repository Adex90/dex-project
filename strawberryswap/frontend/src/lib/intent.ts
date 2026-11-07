import type { Address, Hex } from "viem";

/**
 * Swap INTENT — what a trader hands to an Automator for batch settlement (DarkPool.settleBatch).
 *
 * ⚠ LEGACY FORMAT — NOT WHAT THE AUTOMATOR NOW ACCEPTS. Phase 6's `bots/automator` takes the OPENING of an
 * on-chain INTENT NOTE (see docs/BATCH_SETTLEMENT.md), created with a client-side JoinSplit proof. Producing that
 * in the browser (wasm prover + note management) is not wired yet, so submissions made with this file will be
 * rejected with 400 by a Phase-6 node. Kept so the quoting UI keeps working.
 *
 * ⚠ PROOF LAYER NOT LIVE. The final design wraps this in (a) an encryption to the Automator batch key and
 * (b) a join-split note spend with a ZK proof, so the Automator never learns the trader's address. Those
 * circuits are a later phase. Until then an intent is a plain EIP-712 signature: it authenticates the
 * trader but is NOT private. The UI says so; do not remove that warning before the circuits ship.
 */
export interface SwapIntent {
  pool: Address;
  zeroForOne: boolean;
  amountIn: bigint;
  minAmountOut: bigint;
  deadline: bigint; // unix seconds
  nonce: bigint;
}

export const intentTypes = {
  SwapIntent: [
    { name: "pool", type: "address" },
    { name: "zeroForOne", type: "bool" },
    { name: "amountIn", type: "uint256" },
    { name: "minAmountOut", type: "uint256" },
    { name: "deadline", type: "uint256" },
    { name: "nonce", type: "uint256" },
  ],
} as const;

export function intentDomain(chainId: number, pool: Address) {
  return { name: "StrawberrySwap Intent", version: "1", chainId, verifyingContract: pool } as const;
}

/** Slippage in bps applied to the quoted output; 0 fee is charged by the protocol itself. */
export function minOut(quote: bigint, slippageBps: bigint): bigint {
  return (quote * (10_000n - slippageBps)) / 10_000n;
}

export function randomNonce(): bigint {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return BigInt("0x" + Array.from(b, (x) => x.toString(16).padStart(2, "0")).join(""));
}

export async function postIntent(
  automatorUrl: string,
  intent: SwapIntent,
  signature: Hex,
  signal?: AbortSignal,
): Promise<{ id: string }> {
  const res = await fetch(`${automatorUrl}/v1/intents`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    // bigint -> decimal string: JSON has no bigint
    body: JSON.stringify({ intent, signature }, (_, v) => (typeof v === "bigint" ? v.toString() : v)),
    signal,
  });
  if (!res.ok) throw new Error(`Automator rejected intent (${res.status})`);
  return (await res.json()) as { id: string };
}
