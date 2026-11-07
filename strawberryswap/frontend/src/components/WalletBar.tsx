"use client";

import { chain } from "@/lib/config";
import { shortAddr } from "@/lib/format";
import type { WalletState } from "@/hooks/useWallet";

export function WalletBar({ wallet }: { wallet: WalletState }) {
  return (
    <div className="flex flex-wrap items-center gap-3">
      <span className="label">NET: <span className="text-paper">{chain.name} #{chain.id}</span></span>
      {wallet.account && !wallet.chainOk ? (
        <button className="btn-ghost !px-3 !py-1 !text-sm" onClick={() => void wallet.switchChain()}>Switch network</button>
      ) : null}
      {wallet.account ? (
        <span className="num border-2 border-paper bg-ink px-3 py-1 text-sm" title={wallet.account}>{shortAddr(wallet.account)}</span>
      ) : (
        <button className="btn !px-3 !py-1 !text-sm" onClick={() => void wallet.connect()} disabled={!wallet.hasProvider}>
          {wallet.hasProvider ? "Connect wallet" : "No wallet"}
        </button>
      )}
      {wallet.error ? <span role="alert" className="font-mono text-xs text-berry">{wallet.error}</span> : null}
    </div>
  );
}
