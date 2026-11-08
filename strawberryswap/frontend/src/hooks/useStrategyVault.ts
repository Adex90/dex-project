"use client";

import { useCallback, useState } from "react";
import type { Address } from "viem";
import { erc20Abi, vaultAbi } from "@/lib/abi";
import { publicClient, getWalletClient } from "@/lib/clients";
import { config } from "@/lib/config";
import { errMsg } from "@/lib/format";
import { usePolling } from "./usePolling";

export interface VaultState {
  asset: Address;
  symbol: string;
  manager: Address;
  managerDid: `0x${string}`;
  nav: bigint;
  totalShares: bigint;
  sharePrice: bigint;
  highWaterMark: bigint;
  pendingFee: bigint;
  feeBps: bigint;
  maxTradeBps: bigint;
  maxLossBps: bigint;
  myShares: bigint;
  myAssetBalance: bigint;
  myAllowance: bigint;
}

async function loadVault(vault: Address, account: Address | undefined): Promise<VaultState> {
  const c = { address: vault, abi: vaultAbi } as const;
  const asset = await publicClient.readContract({ ...c, functionName: "asset" });
  const [manager, managerDid, nav, totalShares, sharePrice, highWaterMark, pendingFee, feeBps, maxTradeBps, maxLossBps, symbol] =
    await Promise.all([
      publicClient.readContract({ ...c, functionName: "manager" }),
      publicClient.readContract({ ...c, functionName: "managerDid" }),
      publicClient.readContract({ ...c, functionName: "totalAssets" }),
      publicClient.readContract({ ...c, functionName: "totalShares" }),
      publicClient.readContract({ ...c, functionName: "sharePrice" }),
      publicClient.readContract({ ...c, functionName: "highWaterMark" }),
      publicClient.readContract({ ...c, functionName: "pendingFee" }),
      publicClient.readContract({ ...c, functionName: "PERFORMANCE_FEE_BPS" }),
      publicClient.readContract({ ...c, functionName: "MAX_TRADE_BPS" }),
      publicClient.readContract({ ...c, functionName: "MAX_TRADE_LOSS_BPS" }),
      publicClient.readContract({ address: asset, abi: erc20Abi, functionName: "symbol" }),
    ]);
  let myShares = 0n;
  let myAssetBalance = 0n;
  let myAllowance = 0n;
  if (account) {
    [myShares, myAssetBalance, myAllowance] = await Promise.all([
      publicClient.readContract({ ...c, functionName: "sharesOf", args: [account] }),
      publicClient.readContract({ address: asset, abi: erc20Abi, functionName: "balanceOf", args: [account] }),
      publicClient.readContract({ address: asset, abi: erc20Abi, functionName: "allowance", args: [account, vault] }),
    ]);
  }
  return { asset, symbol, manager, managerDid, nav, totalShares, sharePrice, highWaterMark, pendingFee, feeBps, maxTradeBps, maxLossBps, myShares, myAssetBalance, myAllowance };
}

export type TxStatus =
  | { kind: "idle" }
  | { kind: "pending"; label: string }
  | { kind: "done"; label: string; hash: `0x${string}` }
  | { kind: "error"; message: string };

/**
 * StrategyVault reads + the three investor actions. There is deliberately NO manager-side write here:
 * `executeTrade` is called by the manager's own infrastructure, never from this public UI.
 */
export function useStrategyVault(account: Address | undefined) {
  const vault = config.vault;
  const { data: state, error, refresh } = usePolling(() => loadVault(vault!, account), config.pollMs, !!vault);
  const [tx, setTx] = useState<TxStatus>({ kind: "idle" });

  const run = useCallback(
    async (label: string, send: (w: NonNullable<ReturnType<typeof getWalletClient>>, a: Address) => Promise<`0x${string}`>) => {
      try {
        const wallet = getWalletClient();
        if (!wallet || !account || !vault) throw new Error("Connect a wallet first");
        setTx({ kind: "pending", label });
        const hash = await send(wallet, account);
        await publicClient.waitForTransactionReceipt({ hash });
        setTx({ kind: "done", label, hash });
        await refresh();
      } catch (e) {
        setTx({ kind: "error", message: errMsg(e) });
      }
    },
    [account, vault, refresh],
  );

  /** approve (only if needed) then deposit. Two transactions, sequential. */
  const deposit = useCallback(
    (assets: bigint) =>
      run("deposit", async (w, a) => {
        if (!state || !vault) throw new Error("Vault not loaded");
        if (state.myAllowance < assets) {
          const h = await w.writeContract({ account: a, address: state.asset, abi: erc20Abi, functionName: "approve", args: [vault, assets] });
          await publicClient.waitForTransactionReceipt({ hash: h });
        }
        return w.writeContract({ account: a, address: vault, abi: vaultAbi, functionName: "deposit", args: [assets, a] });
      }),
    [run, state, vault],
  );

  const redeem = useCallback(
    (shares: bigint) =>
      run("redeem", (w, a) => w.writeContract({ account: a, address: vault!, abi: vaultAbi, functionName: "redeem", args: [shares, a] })),
    [run, vault],
  );

  const settleFees = useCallback(
    () => run("settle fees", (w, a) => w.writeContract({ account: a, address: vault!, abi: vaultAbi, functionName: "settleFees" })),
    [run, vault],
  );

  return { configured: !!vault, state, error, refresh, tx, deposit, redeem, settleFees };
}
