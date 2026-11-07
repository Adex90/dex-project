import { createPublicClient, createWalletClient, custom, http, type EIP1193Provider } from "viem";
import { chain } from "./config";

declare global {
  interface Window {
    ethereum?: EIP1193Provider;
  }
}

/** Read client: plain HTTP JSON-RPC to the configured endpoint (no wallet needed to look at the market). */
export const publicClient = createPublicClient({ chain, transport: http(), batch: { multicall: true } });

/** Write client: only exists in a browser with an injected EIP-1193 wallet. */
export function getWalletClient() {
  if (typeof window === "undefined" || !window.ethereum) return undefined;
  return createWalletClient({ chain, transport: custom(window.ethereum) });
}
