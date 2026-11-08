"use client";

import { useCallback, useEffect, useState } from "react";
import { numberToHex, type Address } from "viem";
import { chain } from "@/lib/config";
import { errMsg } from "@/lib/format";

export interface WalletState {
  account: Address | undefined;
  chainOk: boolean;
  hasProvider: boolean;
  error: string | undefined;
  connect: () => Promise<void>;
  switchChain: () => Promise<void>;
}

/** Thin EIP-1193 wallet hook. No analytics, no third-party SDK: the bundle must run from IPFS alone. */
export function useWallet(): WalletState {
  const [account, setAccount] = useState<Address>();
  const [chainId, setChainId] = useState<number>();
  const [error, setError] = useState<string>();
  const [hasProvider, setHasProvider] = useState(false);

  useEffect(() => {
    const eth = window.ethereum;
    setHasProvider(!!eth);
    if (!eth) return;
    const onAccounts = (...args: unknown[]) => setAccount(((args[0] as string[])[0] as Address | undefined) ?? undefined);
    const onChain = (...args: unknown[]) => setChainId(Number.parseInt(args[0] as string, 16));
    void eth.request({ method: "eth_accounts" }).then((a) => onAccounts(a));
    void eth.request({ method: "eth_chainId" }).then((c) => onChain(c));
    eth.on("accountsChanged", onAccounts);
    eth.on("chainChanged", onChain);
    return () => {
      eth.removeListener("accountsChanged", onAccounts);
      eth.removeListener("chainChanged", onChain);
    };
  }, []);

  const connect = useCallback(async () => {
    setError(undefined);
    try {
      if (!window.ethereum) throw new Error("No injected wallet found");
      const a = await window.ethereum.request({ method: "eth_requestAccounts" });
      setAccount(a[0]);
    } catch (e) {
      setError(errMsg(e));
    }
  }, []);

  const switchChain = useCallback(async () => {
    setError(undefined);
    try {
      await window.ethereum?.request({
        method: "wallet_switchEthereumChain",
        params: [{ chainId: numberToHex(chain.id) }],
      });
    } catch (e) {
      setError(errMsg(e));
    }
  }, []);

  return { account, chainOk: chainId === chain.id, hasProvider, error, connect, switchChain };
}
