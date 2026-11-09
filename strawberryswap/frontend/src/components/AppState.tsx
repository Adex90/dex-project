"use client";

import { useContext, useMemo, type ReactNode } from "react";
import { AppStateContext } from "@/components/appStateContext";
import { useDarkPool } from "@/hooks/useDarkPool";
import { usePrivateSwap } from "@/hooks/usePrivateSwap";
import { useShieldedWallet } from "@/hooks/useShieldedWallet";
import { useStrategyVault } from "@/hooks/useStrategyVault";
import { useWallet } from "@/hooks/useWallet";

/**
 * Layout-level composition of the existing hooks (called exactly as the old /trade page and panels called them).
 * Living in the (app) layout, it stays mounted while the bottom navigation switches tabs, so an unlocked shielded wallet,
 * its synced notes and an in-flight proof survive moving between Trade, Stocks, Earn and Portfolio.
 */
function useAppStateValue() {
  const wallet = useWallet();
  const ready = !!wallet.account && wallet.chainOk;
  const pool = useDarkPool();
  const shielded = useShieldedWallet(wallet.account);
  const swapper = usePrivateSwap(wallet.account, shielded, pool.state);
  const vault = useStrategyVault(wallet.account);
  return { wallet, ready, pool, shielded, swapper, vault };
}

export type AppState = ReturnType<typeof useAppStateValue>;

export function AppStateProvider({ children }: { children: ReactNode }) {
  const value = useAppStateValue();
  return <AppStateContext.Provider value={value}>{children}</AppStateContext.Provider>;
}

export function useApp(): AppState {
  const s = useContext(AppStateContext);
  if (!s) throw new Error("useApp() must be used inside <AppStateProvider>");
  return s;
}

/** Spendable shielded balance per pool asset: unspent, unreserved notes of asset 0 and 1 (the old panel's sum). */
export function useShieldedBalances(): [bigint, bigint] {
  const { shielded } = useApp();
  return useMemo(() => {
    const acc: [bigint, bigint] = [0n, 0n];
    for (const n of shielded.wallet?.notes ?? []) {
      if (!n.spent && !n.reserved && (n.assetId === 0n || n.assetId === 1n)) acc[Number(n.assetId) as 0 | 1] += n.value;
    }
    return acc;
  }, [shielded.wallet]);
}

/** Symbol of asset 0/1 of the pool, with the same fallbacks the old panels used. */
export function useSymbols() {
  const { pool } = useApp();
  const ps = pool.state;
  const sym = (a: bigint | 0 | 1): string => (ps ? (Number(a) === 0 ? ps.symbol0 : ps.symbol1) : Number(a) === 0 ? "T0" : "T1");
  return sym;
}
