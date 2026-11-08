import { defineChain, isAddress, type Address } from "viem";

/**
 * Build-time configuration. `NEXT_PUBLIC_*` values are inlined by Next during `next build`, so the
 * published IPFS CID commits to the exact chain and contract addresses this bundle talks to.
 * Each variable must be referenced literally (`process.env.NEXT_PUBLIC_X`) or Next will not inline it.
 */
const env = {
  chainId: process.env.NEXT_PUBLIC_CHAIN_ID,
  chainName: process.env.NEXT_PUBLIC_CHAIN_NAME,
  rpcUrl: process.env.NEXT_PUBLIC_RPC_URL,
  darkPool: process.env.NEXT_PUBLIC_DARKPOOL_ADDRESS,
  vault: process.env.NEXT_PUBLIC_VAULT_ADDRESS,
  automator: process.env.NEXT_PUBLIC_AUTOMATOR_URL,
  deployBlock: process.env.NEXT_PUBLIC_DEPLOY_BLOCK,
  logChunk: process.env.NEXT_PUBLIC_LOG_CHUNK,
};

function addr(v: string | undefined): Address | undefined {
  return v && isAddress(v) ? v : undefined;
}

export const chain = defineChain({
  id: Number(env.chainId ?? 31337),
  name: env.chainName ?? "Local",
  nativeCurrency: { name: "Ether", symbol: "ETH", decimals: 18 },
  rpcUrls: { default: { http: [env.rpcUrl ?? "http://127.0.0.1:8545"] } },
});

export const config = {
  chain,
  darkPool: addr(env.darkPool),
  vault: addr(env.vault),
  automatorUrl: env.automator && env.automator.length > 0 ? env.automator.replace(/\/$/, "") : undefined,
  /** First block to scan for NoteInserted (the pool's deployment block). */
  deployBlock: BigInt(env.deployBlock && /^\d+$/.test(env.deployBlock) ? env.deployBlock : "0"),
  /** Blocks per eth_getLogs request (public RPCs cap this). */
  logChunk: BigInt(env.logChunk && /^\d+$/.test(env.logChunk) ? env.logChunk : "2000"),
  pollMs: 12_000,
} as const;
