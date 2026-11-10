"use client";

import { useState } from "react";
import { config } from "@/lib/config";
import { fmt } from "@/lib/format";
import { proofsAvailable } from "@/zk/availability";
import { useApp, useShieldedBalances, useSymbols } from "@/components/AppState";
import { IntentRow, NoteRow } from "@/components/trade/OrdersCard";
import { IconLock, IconPortfolio, IconWallet } from "@/components/ui/icons";
import { Mascot } from "@/components/ui/Mascot";
import { Card, CardHeader, Loading, Reveal, Segmented, Skeleton, TabPanel, TokenIcon } from "@/components/ui/primitives";
import { useFiat } from "@/components/settings/Settings";
import { useWalletModal } from "@/components/wallet/WalletModal";

const WAD = 10n ** 18n;
type Tab = "overview" | "liquidity" | "history";

function Empty({ children }: { children: React.ReactNode }) {
  return <p className="card-inset text-center text-sm text-haze">{children}</p>;
}

/**
 * Total shielded value in the reference currency, from on-chain data only: the pool price converts the non-stable leg into
 * the pool's USD stablecoin, and only USD has a rate (the peg, stated). EUR/MXN say so instead of guessing.
 */
function FiatTotal({ balances }: { balances: [bigint, bigint] }) {
  const { pool } = useApp();
  const fiat = useFiat();
  const s = pool.state;
  let usd: number | undefined;
  let stable: string | undefined;
  if (s && s.spot > 0n) {
    const b0 = Number(balances[0]) / 1e18;
    const b1 = Number(balances[1]) / 1e18;
    const spot = Number(s.spot) / 1e18; // token1 per token0
    if (s.symbol0.toUpperCase().startsWith("USD")) [usd, stable] = [b0 + b1 / spot, s.symbol0];
    else if (s.symbol1.toUpperCase().startsWith("USD")) [usd, stable] = [b0 * spot + b1, s.symbol1];
  }
  const v = fiat(usd, stable);
  return (
    <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2 px-1">
      <span className="text-sm text-lavender">Estimated total</span>
      <span className="text-right">
        {v.text ? <span className="num text-xl font-semibold text-ink">≈ {v.text}</span> : null}
        <span className="ml-2 text-xs text-haze">{v.note}</span>
      </span>
    </div>
  );
}

function ShieldedBalances() {
  const { ready, shielded: w } = useApp();
  const unavailable = !config.darkPool || !proofsAvailable;
  const sym = useSymbols();
  const balances = useShieldedBalances();
  if (w.phase === "locked" || w.phase === "error") {
    return (
      <div className="card-inset flex flex-col items-center gap-3 py-6 text-center">
        <IconLock className="h-6 w-6 text-lavender" />
        <p className="max-w-sm text-sm text-lavender">Shielded balances are encrypted to a key derived on this device. Unlock to scan the chain for your notes.</p>
        <button type="button" className="btn-primary" disabled={unavailable || !ready} onClick={() => void w.unlock()}>
          Unlock shielded wallet
        </button>
      </div>
    );
  }
  if (!w.wallet) {
    return (
      <Loading label="Scanning the chain for your notes" className="grid grid-cols-2 gap-3">
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-24 w-full" />
      </Loading>
    );
  }
  return (
    <Reveal>
      <FiatTotal balances={balances} />
      <div className="grid grid-cols-2 gap-3">
      {([0, 1] as const).map((a) => (
        <div key={a} className="card-inset min-w-0">
          <div className="flex items-center gap-2">
            <TokenIcon symbol={sym(a)} size={28} />
            <span className="font-bold">{sym(a)}</span>
          </div>
          <div className="num mt-2 truncate text-2xl font-semibold">{fmt(balances[a])}</div>
          <div className="text-xs text-haze">shielded · spendable</div>
        </div>
      ))}
      </div>
    </Reveal>
  );
}

function VaultPosition() {
  const { vault: v } = useApp();
  const s = v.state;
  if (!v.configured) return <Empty>Strategy vaults launch with Mainnet.</Empty>;
  if (!s) {
    return (
      <Loading label="Loading vault position">
        <Skeleton className="h-20 w-full" />
      </Loading>
    );
  }
  const value = (s.myShares * s.sharePrice) / WAD;
  const shareBps = s.totalShares > 0n ? (s.myShares * 10_000n) / s.totalShares : 0n;
  return (
    <Reveal className="card-inset grid grid-cols-3 gap-3">
      <div className="min-w-0">
        <div className="eyebrow">Vault shares</div>
        <div className="num truncate text-lg font-semibold">{fmt(s.myShares)}</div>
      </div>
      <div className="min-w-0">
        <div className="eyebrow">Value ≈</div>
        <div className="num truncate text-lg font-semibold">{fmt(value)} <span className="text-xs text-haze">{s.symbol}</span></div>
      </div>
      <div className="min-w-0">
        <div className="eyebrow">Of the fund</div>
        <div className="num truncate text-lg font-semibold">{(Number(shareBps) / 100).toFixed(2)}%</div>
      </div>
    </Reveal>
  );
}

/** Wallet overview: Overview (balances), Liquidity (vault share + private notes), History (intents + this session). */
export function PortfolioCard() {
  const { wallet, shielded: w, swapper } = useApp();
  const { openWallet } = useWalletModal();
  const [tab, setTab] = useState<Tab>("overview");
  const notes = (w.wallet?.notes ?? []).filter((n) => !n.spent);
  const intents = w.wallet?.intents ?? [];

  if (!wallet.account) {
    return (
      <Card className="flex flex-col items-center gap-4 py-10 text-center">
        <Mascot className="h-24 w-24" float />
        <div>
          <h2 className="text-xl font-bold">Connect a wallet to see your portfolio</h2>
          <p className="mt-1 text-sm text-lavender">Shielded balances are decrypted on this device only.</p>
        </div>
        <button type="button" className="btn-primary" onClick={openWallet}>
          <IconWallet className="h-5 w-5" />
          Connect Wallet
        </button>
      </Card>
    );
  }

  return (
    <Card aria-labelledby="pf-title">
      <CardHeader icon={<IconPortfolio />} title={<span id="pf-title">Your portfolio</span>} subtitle="Private balances are computed here, never sent anywhere" />
      <Segmented
        idPrefix="pf"
        label="Portfolio view"
        value={tab}
        onChange={setTab}
        options={[
          { value: "overview", label: "Overview" },
          { value: "liquidity", label: "Liquidity" },
          { value: "history", label: "History" },
        ]}
        className="mb-5 w-full [&>button]:flex-1 sm:w-auto sm:[&>button]:flex-none"
      />

      <TabPanel idPrefix="pf" value="overview" active={tab === "overview"}>
        <div className="space-y-5">
          <section>
            <h3 className="eyebrow mb-2">Shielded balances</h3>
            <ShieldedBalances />
          </section>
          <section>
            <h3 className="eyebrow mb-2">Strategy vault</h3>
            <VaultPosition />
          </section>
        </div>
      </TabPanel>

      <TabPanel idPrefix="pf" value="liquidity" active={tab === "liquidity"}>
        <div className="space-y-5">
          <section>
            <h3 className="eyebrow mb-2">Vault liquidity</h3>
            <VaultPosition />
          </section>
          <section>
            <h3 className="eyebrow mb-2">Private notes</h3>
            {w.phase === "locked" || w.phase === "error" ? (
              <Empty>Unlock the shielded wallet (Overview) to list your notes.</Empty>
            ) : !w.wallet ? (
              <Loading label="Loading notes" className="space-y-2">
                <Skeleton className="h-12 w-full" />
                <Skeleton className="h-12 w-full" />
              </Loading>
            ) : notes.length > 0 ? (
              <ul className="space-y-2 animate-fade-in">
                {notes.map((n) => (
                  <NoteRow key={n.commitment.toString()} n={n} />
                ))}
              </ul>
            ) : (
              <Empty>No notes yet. Shield tokens on the Trade tab.</Empty>
            )}
          </section>
        </div>
      </TabPanel>

      <TabPanel idPrefix="pf" value="history" active={tab === "history"}>
        <div className="space-y-5">
          <section>
            <h3 className="eyebrow mb-2">Private swaps</h3>
            {w.phase === "locked" || w.phase === "error" ? (
              <Empty>Unlock the shielded wallet (Overview) to load your intents.</Empty>
            ) : !w.wallet ? (
              <Loading label="Loading intents">
                <Skeleton className="h-12 w-full" />
              </Loading>
            ) : intents.length > 0 ? (
              <ul className="space-y-2 animate-fade-in">
                {intents.map((i) => (
                  <IntentRow key={i.commitment.toString()} i={i} />
                ))}
              </ul>
            ) : (
              <Empty>No private swaps yet.</Empty>
            )}
          </section>
          <section>
            <h3 className="eyebrow mb-2">This session</h3>
            {swapper.steps.length > 0 ? (
              <ol className="card-inset space-y-0.5 text-sm">
                {swapper.steps.map((s) => (
                  <li key={s.id} className={s.state === "error" ? "text-berry-soft" : s.state === "done" ? "text-mint" : "text-ink"}>
                    {s.state === "done" ? "✓" : s.state === "error" ? "✗" : "…"} {s.label}
                    {s.detail ? <span className="num ml-2 text-xs text-haze">{s.detail}</span> : null}
                  </li>
                ))}
              </ol>
            ) : (
              <Empty>Nothing yet. Deposits, swaps and withdrawals you make here show up step by step.</Empty>
            )}
          </section>
        </div>
      </TabPanel>
    </Card>
  );
}
