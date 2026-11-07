"use client";

import { DarkPoolPanel } from "@/components/DarkPoolPanel";
import { Ticker } from "@/components/Ticker";
import { VaultPanel } from "@/components/VaultPanel";
import { WalletBar } from "@/components/WalletBar";
import { useWallet } from "@/hooks/useWallet";

export default function TradePage() {
  const wallet = useWallet();
  const ready = !!wallet.account && wallet.chainOk;

  return (
    <>
      <Ticker />
      <main className="mx-auto max-w-6xl px-4 pb-24 pt-6">
        <header className="mb-8 flex flex-wrap items-end justify-between gap-4">
          <div>
            <p className="label">StrawberrySwap · Terminal 01 · Institutional desk</p>
            <h1 className="mt-1 font-slab text-5xl uppercase leading-none tracking-tight md:text-7xl">
              <span className="ransom bg-paper text-ink">Trade</span>{" "}
              <span className="ransom -rotate-2 bg-berry text-ink">with</span>{" "}
              <span className="ransom rotate-1 border-2 border-paper text-paper">no fee</span>
            </h1>
          </div>
          <WalletBar wallet={wallet} />
        </header>

        {wallet.account && !wallet.chainOk ? (
          <p role="alert" className="mb-6 border-2 border-berry bg-ink px-3 py-2 font-mono text-sm text-berry">
            Wrong network. Reads still work; writes are disabled until you switch.
          </p>
        ) : null}

        <div className="grid gap-10 lg:grid-cols-2">
          <DarkPoolPanel account={wallet.account} ready={ready} />
          <VaultPanel account={wallet.account} ready={ready} />
        </div>

        <footer className="mt-14 border-t-2 border-dashed border-rule pt-4 font-mono text-[11px] uppercase tracking-widest text-ash">
          Unaudited software · testnet only · you are trading against contracts, not a company.
        </footer>
      </main>
    </>
  );
}
