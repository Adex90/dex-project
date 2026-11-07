"use client";

import Link from "next/link";
import { DarkPoolPanel } from "@/components/DarkPoolPanel";
import { VaultPanel } from "@/components/VaultPanel";
import { WalletBar } from "@/components/WalletBar";
import { useWallet } from "@/hooks/useWallet";

export default function TradePage() {
  const wallet = useWallet();
  const ready = !!wallet.account && wallet.chainOk;

  return (
    <>
      <div className="border-b border-rule">
        <div className="mx-auto flex max-w-6xl flex-wrap items-center justify-between gap-4 px-6 py-3">
          <Link href="/" className="font-serif text-lg tracking-wide text-text no-underline" style={{ fontVariantCaps: "small-caps" }}>
            StrawberrySwap
          </Link>
          <WalletBar wallet={wallet} />
        </div>
      </div>

      <main className="mx-auto max-w-6xl px-6 pb-24 pt-12">
        <header className="mb-12 max-w-3xl">
          <p className="label">Institutional Desk</p>
          <h1 className="mt-2 font-serif text-4xl font-normal leading-tight">Trading Terminal</h1>
          <p className="mt-3 italic text-muted">
            A zero-fee dark pool and managed strategy vaults. Every balance is fully reserved; no administrator can mint,
            lend deposits, or withdraw fund assets.
          </p>
        </header>

        {wallet.account && !wallet.chainOk ? (
          <p role="alert" className="mb-8 border-l-2 border-neg pl-3 text-[14px] text-neg">
            Wrong network. Market data remains available; transactions are disabled until the wallet is switched.
          </p>
        ) : null}

        <div className="grid gap-x-16 gap-y-16 lg:grid-cols-2">
          <DarkPoolPanel account={wallet.account} ready={ready} />
          <VaultPanel account={wallet.account} ready={ready} />
        </div>

        <footer className="mt-20 border-t border-rule pt-4 text-[13px] text-muted">
          Unaudited software. Testnet use only. Counterparty is the smart contract, not a firm.
        </footer>
      </main>
    </>
  );
}
