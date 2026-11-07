"use client";

import { useState } from "react";
import type { Address } from "viem";
import { useStrategyVault } from "@/hooks/useStrategyVault";
import { bpsToPct, fmt, parseAmount, shortAddr } from "@/lib/format";
import { Notice, Panel, Stat } from "./Panel";

export function VaultPanel({ account, ready }: { account: Address | undefined; ready: boolean }) {
  const v = useStrategyVault(account);
  const s = v.state;
  const [dep, setDep] = useState("");
  const [red, setRed] = useState("");
  const depAmt = parseAmount(dep);
  const redAmt = parseAmount(red);

  // client-side previews mirror the contract's maths (shares = assets × S / NAV); the chain is authoritative
  const previewShares = s && depAmt && s.totalShares > 0n && s.nav > 0n ? (depAmt * s.totalShares) / s.nav : undefined;
  const previewAssets = s && redAmt && s.totalShares > 0n ? (redAmt * s.nav) / s.totalShares : undefined;
  const busy = v.tx.kind === "pending";
  const inProfit = s ? s.sharePrice > s.highWaterMark : false;

  return (
    <Panel no="2" title="Strategy Vault" tag={s ? `Performance fee ${bpsToPct(s.feeBps)} above high-water mark` : "Performance fee above high-water mark"}>
      {!v.configured ? (
        <Notice tone="warn">No vault address in this build. Set NEXT_PUBLIC_VAULT_ADDRESS and rebuild.</Notice>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-x-8 gap-y-5">
            <Stat label="NAV" value={fmt(s?.nav)} sub={s?.symbol} />
            <Stat label="Share price" value={fmt(s?.sharePrice, 18, 6)} tone="info" />
            <Stat label="High-water mark" value={fmt(s?.highWaterMark, 18, 6)} tone={inProfit ? "pos" : "default"} />
            <Stat label="Accrued perf. fee" value={fmt(s?.pendingFee)} sub={inProfit ? "profit above high-water mark" : "none — at or below mark"} tone={inProfit ? "warn" : "default"} />
          </div>

          <dl className="mt-6 text-[15px]">
            <div className="kv"><dt className="label">Manager (trade-only role)</dt><dd className="num">{shortAddr(s?.manager)}</dd></div>
            <div className="kv"><dt className="label">Maximum per trade</dt><dd className="num">{bpsToPct(s?.maxTradeBps)} of NAV</dd></div>
            <div className="kv"><dt className="label">Maximum loss per trade</dt><dd className="num">{bpsToPct(s?.maxLossBps)}</dd></div>
            <div className="kv"><dt className="label">Manager withdrawal</dt><dd className="text-pos">none — no such function exists</dd></div>
          </dl>

          <div className="mt-8 grid gap-8 md:grid-cols-2">
            <div className="grid gap-2">
              <label><span className="label">Deposit · {s?.symbol ?? "asset"} · balance {fmt(s?.myAssetBalance)}</span>
                <input className="field" inputMode="decimal" placeholder="0.0" value={dep} onChange={(e) => setDep(e.target.value)} aria-label="Deposit amount" /></label>
              <div className="num text-xs text-muted">≈ {fmt(previewShares)} shares</div>
              <button className="btn" disabled={!ready || !depAmt || busy || (s ? depAmt > s.myAssetBalance : true)} onClick={() => depAmt && void v.deposit(depAmt)}>Deposit</button>
            </div>
            <div className="grid gap-2">
              <label><span className="label">Redeem · shares · held {fmt(s?.myShares)}</span>
                <input className="field" inputMode="decimal" placeholder="0.0" value={red} onChange={(e) => setRed(e.target.value)} aria-label="Shares to redeem" /></label>
              <div className="num text-xs text-muted">≈ {fmt(previewAssets)} {s?.symbol} (needs idle cash)</div>
              <button className="btn-ghost" disabled={!ready || !redAmt || busy || (s ? redAmt > s.myShares : true)} onClick={() => redAmt && void v.redeem(redAmt)}>Redeem</button>
            </div>
          </div>

          <div className="mt-6 flex flex-wrap items-center gap-4">
            <button className="btn-ghost !px-3 !py-1" disabled={!ready || busy} onClick={() => void v.settleFees()}>Settle fee cycle</button>
            <span className="text-sm text-muted">Callable by anyone. The fee is zero unless the share price exceeds the high-water mark.</span>
          </div>

          {v.tx.kind === "pending" ? <Notice tone="warn">{v.tx.label}… confirm in wallet, then wait for inclusion.</Notice> : null}
          {v.tx.kind === "done" ? <Notice tone="ok">{v.tx.label} confirmed · {shortAddr(v.tx.hash)}</Notice> : null}
          {v.tx.kind === "error" ? <Notice tone="err">{v.tx.message}</Notice> : null}
          {v.error ? <Notice tone="err">Vault read failed: {v.error}</Notice> : null}
        </>
      )}
    </Panel>
  );
}
