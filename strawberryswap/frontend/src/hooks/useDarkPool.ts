"use client";

import { useCallback, useEffect, useState } from "react";
import type { Address } from "viem";
import { darkPoolAbi, erc20Abi } from "@/lib/abi";
import { publicClient } from "@/lib/clients";
import { config } from "@/lib/config";
import { usePolling } from "./usePolling";

export interface PoolState {
  token0: Address;
  token1: Address;
  symbol0: string;
  symbol1: string;
  reserve0: bigint;
  reserve1: bigint;
  virtual0: bigint;
  virtual1: bigint;
  spot: bigint; // token1 per token0, 1e18
  k: bigint;
  extractable: bigint | undefined; // undefined when the zk oracle is stale (view reverts)
  feeBps: bigint;
  extractionBps: bigint;
}

async function loadPool(pool: Address): Promise<PoolState> {
  const c = { address: pool, abi: darkPoolAbi } as const;
  const [token0, token1, reserve0, reserve1, virtual0, virtual1, spot, k, feeBps, extractionBps] = await Promise.all([
    publicClient.readContract({ ...c, functionName: "token0" }),
    publicClient.readContract({ ...c, functionName: "token1" }),
    publicClient.readContract({ ...c, functionName: "reserve0" }),
    publicClient.readContract({ ...c, functionName: "reserve1" }),
    publicClient.readContract({ ...c, functionName: "virtual0" }),
    publicClient.readContract({ ...c, functionName: "virtual1" }),
    publicClient.readContract({ ...c, functionName: "spotPrice" }),
    publicClient.readContract({ ...c, functionName: "invariantK" }),
    publicClient.readContract({ ...c, functionName: "RETAIL_FEE_BPS" }),
    publicClient.readContract({ ...c, functionName: "EXTRACTION_BPS" }),
  ]);
  const [symbol0, symbol1] = await Promise.all([
    publicClient.readContract({ address: token0, abi: erc20Abi, functionName: "symbol" }),
    publicClient.readContract({ address: token1, abi: erc20Abi, functionName: "symbol" }),
  ]);
  // extractableValue() reverts while the zk oracle is stale: treat as "unknown", not as an error.
  const extractable = await publicClient
    .readContract({ ...c, functionName: "extractableValue" })
    .catch(() => undefined);
  return { token0, token1, symbol0, symbol1, reserve0, reserve1, virtual0, virtual1, spot, k, extractable, feeBps, extractionBps };
}

/**
 * DarkPool market state + indicative quote. Swaps are never sent to the pool directly: they are shielded intents
 * created and submitted by usePrivateSwap and settled in batches (DarkPool.settleBatch), which is what keeps
 * individual size and counterparty off-chain and out of the mempool.
 */
export function useDarkPool() {
  const pool = config.darkPool;
  const { data: state, error, refresh } = usePolling(() => loadPool(pool!), config.pollMs, !!pool);
  const [quote, setQuote] = useState<bigint>();

  const requestQuote = useCallback(
    async (amountIn: bigint | undefined, zeroForOne: boolean) => {
      if (!pool || !amountIn) return setQuote(undefined);
      try {
        setQuote(await publicClient.readContract({ address: pool, abi: darkPoolAbi, functionName: "getAmountOut", args: [amountIn, zeroForOne] }));
      } catch {
        setQuote(undefined);
      }
    },
    [pool],
  );

  // Clear a stale quote when the pool disappears (config change)
  useEffect(() => {
    if (!pool) setQuote(undefined);
  }, [pool]);

  return { configured: !!pool, state, error, refresh, quote, requestQuote };
}
