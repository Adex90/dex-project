"use client";

import { useState } from "react";
import { fmt, parseAmount, shortAddr } from "@/lib/format";
import { devKeys, proofsAvailable } from "@/zk/availability";
import { useApp, useShieldedBalances, useSymbols } from "@/components/AppState";
import { IconLock, IconRefresh, IconShield } from "@/components/ui/icons";
import { Card, CardHeader, Loading, Notice, Reveal, Row, Skeleton, TokenIcon, TokenSelect } from "@/components/ui/primitives";

const PHASE_PILL = {
  locked: <span className="pill-plum">Locked</span>,
  unlocking: <span className="pill-blush">Unlocking</span>,
  syncing: <span className="pill-blush">Syncing</span>,
  ready: <span className="pill-mint">Ready</span>,
  error: <span className="pill-berry">Error</span>,
} as const;

/** The shielded wallet: status, spendable balances, chain consistency, and the shield (public → shielded) form. */
export function WalletCard({ className = "" }: { className?: string }) {
  const { ready, shielded: w, swapper } = useApp();
  const sym = useSymbols();
  const balances = useShieldedBalances();
  const [depAsset, setDepAsset] = useState<0 | 1>(0);
  const [depAmount, setDepAmount] = useState("");
  const depAmt = parseAmount(depAmount);

  const canAct = ready && w.phase === "ready" && w.inSync !== false && !swapper.busy;
  const unlocked = w.phase === "ready" || w.phase === "syncing";
  const firstSync = unlocked && !w.wallet;

  return (
    <Card className={className} aria-labelledby="wallet-title">
      <CardHeader icon={<IconShield />} title={<span id="wallet-title">Shielded wallet</span>} subtitle="Keys and notes stay on this device" right={PHASE_PILL[w.phase]} />

      {!proofsAvailable ? (
        <Notice tone="err" className="mb-3">
          This build contains no proving artifacts, so private operations are unavailable. Rebuild after `npm run compile &amp;&amp; npm run setup:dev` in circuits/.
        </Notice>
      ) : null}
      {proofsAvailable && devKeys ? (
        <Notice tone="warn" className="mb-3">
          Development proving keys (single-party setup). A holder of that setup could forge proofs. Use test funds only.
        </Notice>
      ) : null}

      {!unlocked ? (
        <div className="card-inset flex gap-3">
          <IconLock className="mt-0.5 h-5 w-5 shrink-0 text-lavender" />
          <p className="text-sm text-lavender">
            Unlocking asks your wallet to sign one message. The signature stays on this device and derives the key that controls your shielded notes
            inside a Web Worker. Your private key and trade parameters are never sent anywhere. Reloading the page forgets it; signing again restores
            everything from the chain.
          </p>
        </div>
      ) : (
        <>
          {/* ---------------------------------------------------------------------------------------- balances */}
          {firstSync ? (
            <Loading label="Scanning the chain for your notes" className="grid grid-cols-2 gap-3">
              <Skeleton className="h-[5.25rem] w-full" />
              <Skeleton className="h-[5.25rem] w-full" />
            </Loading>
          ) : (
            <Reveal className="grid grid-cols-2 gap-3">
              {([0, 1] as const).map((a) => (
                <div key={a} className="card-inset min-w-0">
                  <div className="flex items-center gap-2">
                    <TokenIcon symbol={sym(a)} size={22} />
                    <span className="eyebrow">Shielded {sym(a)}</span>
                  </div>
                  <div className="num mt-1 truncate text-xl font-semibold">{fmt(balances[a])}</div>
                  <div className="text-xs text-haze">spendable</div>
                </div>
              ))}
            </Reveal>
          )}

          <div className="mt-3 px-1">
            <Row label="Shielded public key">
              <span className="num">{shortAddr(`0x${(w.wallet?.pubKey ?? 0n).toString(16).padStart(64, "0")}`)}</span>
            </Row>
            <Row label="Notes in tree / scanned">
              <span className="num">{w.wallet?.treeSize ?? 0}</span>
            </Row>
            <Row label="Chain consistency">
              <span className={w.inSync === false ? "text-berry-soft" : "text-mint"}>
                {w.inSync === undefined ? "checking…" : w.inSync ? "local tree == on-chain root" : "MISMATCH — resync before acting"}
              </span>
            </Row>
          </div>
          <div className="mt-2 flex flex-wrap gap-2">
            <button type="button" className="btn-secondary" disabled={swapper.busy} onClick={() => void w.sync()}>
              <IconRefresh className="h-4 w-4" />
              Rescan chain
            </button>
            <button type="button" className="btn-ghost" disabled={swapper.busy} onClick={() => void w.lock()}>
              <IconLock className="h-4 w-4" />
              Lock &amp; forget
            </button>
          </div>

          {/* ---------------------------------------------------------------------------------------- shield */}
          <div className="mt-5 border-t border-lavender/10 pt-5">
            <h3 className="font-bold">Shield tokens</h3>
            <p className="mb-3 text-sm text-lavender">Move public tokens into private notes you control.</p>
            <div className="card-inset flex items-center gap-3">
              <TokenSelect
                value={depAsset}
                options={[
                  { value: 0 as const, symbol: sym(0) },
                  { value: 1 as const, symbol: sym(1) },
                ]}
                onChange={(v) => setDepAsset(v)}
                ariaLabel="Deposit asset"
              />
              <input className="field text-right" inputMode="decimal" placeholder="0.0" value={depAmount} onChange={(e) => setDepAmount(e.target.value)} aria-label="Deposit amount" />
            </div>
            <button type="button" className="btn-primary mt-3 w-full" disabled={!canAct || !depAmt} onClick={() => depAmt && void swapper.deposit(depAsset, depAmt)}>
              <IconShield className="h-5 w-5" />
              Deposit
            </button>
            <p className="mt-2 text-xs text-haze">The amount and your address are public at this edge, by definition.</p>
          </div>
        </>
      )}

      {w.error ? <Notice tone="err" className="mt-3">{w.error}</Notice> : null}
    </Card>
  );
}
