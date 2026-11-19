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
  // Testnet (scripts/env-from-manifest.mjs writes these from contracts/deployments/<chainId>.json).
  testnet: process.env.NEXT_PUBLIC_TESTNET,
  explorer: process.env.NEXT_PUBLIC_EXPLORER_URL,
  disclosure: process.env.NEXT_PUBLIC_DISCLOSURE_ADDRESS,
  tWETH: process.env.NEXT_PUBLIC_TWETH_ADDRESS,
  tUSD: process.env.NEXT_PUBLIC_TUSD_ADDRESS,
};

/** Arbitrum Sepolia and anvil: the chains whose deployments use the dev-key verifiers (contracts/script/DeployTestnet.s.sol). */
export const ARBITRUM_SEPOLIA = 421614;
const TEST_CHAINS: ReadonlySet<number> = new Set([ARBITRUM_SEPOLIA, 31337]);

function addr(v: string | undefined): Address | undefined {
  return v && isAddress(v) ? v : undefined;
}

function httpsUrl(v: string | undefined): string | undefined {
  return v && /^https:\/\/[^\s"'<>]+$/.test(v) ? v.replace(/\/$/, "") : undefined;
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
  /**
   * A testnet build: the trusted-bootstrap banner is shown on every page and cannot be dismissed. A test chain is
   * always a testnet build, whatever NEXT_PUBLIC_TESTNET says; NEXT_PUBLIC_TESTNET=1 marks any other chain as one.
   */
  testnet: TEST_CHAINS.has(chain.id) || env.testnet === "1",
  /** Block explorer (https only), for the disclosure contract and the faucets. */
  explorerUrl: httpsUrl(env.explorer),
  /** contracts/src/testnet/TestnetDisclosure.sol: the same disclosure, on chain, with every address of the deployment. */
  disclosure: addr(env.disclosure),
  /** The faucet tokens (contracts/src/testnet/TestToken.sol): `faucet()` pays 1 tWETH or 5,000 tUSD a day per address. */
  faucets: { tWETH: addr(env.tWETH), tUSD: addr(env.tUSD) },
} as const;
