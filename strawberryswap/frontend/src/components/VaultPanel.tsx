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
    <Panel title="Strategy vault · hedge fund" tag={s ? `fee ${bpsToPct(s.feeBps)} over HWM` : "fee over HWM"}>
      {!v.configured ? (
        <Notice tone="warn">No vault address in this build. Set NEXT_PUBLIC_VAULT_ADDRESS and rebuild.</Notice>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Stat label="NAV" value={fmt(s?.nav)} sub={s?.symbol} />
            <Stat label="Share price" value={fmt(s?.sharePrice, 18, 6)} tone="ice" />
            <Stat label="High-water mark" value={fmt(s?.highWaterMark, 18, 6)} tone={inProfit ? "acid" : "paper"} />
            <Stat label="Accrued perf. fee" value={fmt(s?.pendingFee)} sub={inProfit ? "profit above HWM" : "0 — below HWM"} tone={inProfit ? "berry" : "paper"} />
          </div>

          <dl className="mt-4 grid gap-1 border-2 border-dashed border-rule p-3 font-mono text-xs text-ash">
            <div className="flex justify-between"><dt>MANAGER (trade-only role)</dt><dd className="text-paper">{shortAddr(s?.manager)}</dd></div>
            <div className="flex justify-between"><dt>MAX / TRADE</dt><dd className="text-paper">{bpsToPct(s?.maxTradeBps)} of NAV</dd></div>
            <div className="flex justify-between"><dt>MAX LOSS / TRADE</dt><dd className="text-paper">{bpsToPct(s?.maxLossBps)}</dd></div>
            <div className="flex justify-between"><dt>MANAGER CAN WITHDRAW?</dt><dd className="text-acid">NO — no such function</dd></div>
          </dl>

          <div className="mt-5 grid gap-5 md:grid-cols-2">
            <div className="grid gap-2">
              <label><span className="label">Deposit · {s?.symbol ?? "asset"} (bal {fmt(s?.myAssetBalance)})</span>
                <input className="field" inputMode="decimal" placeholder="0.0" value={dep} onChange={(e) => setDep(e.target.value)} aria-label="Deposit amount" /></label>
              <div className="num text-xs text-ash">≈ {fmt(previewShares)} shares</div>
              <button className="btn" disabled={!ready || !depAmt || busy || (s ? depAmt > s.myAssetBalance : true)} onClick={() => depAmt && void v.deposit(depAmt)}>Deposit</button>
            </div>
            <div className="grid gap-2">
              <label><span className="label">Redeem · shares (you hold {fmt(s?.myShares)})</span>
                <input className="field" inputMode="decimal" placeholder="0.0" value={red} onChange={(e) => setRed(e.target.value)} aria-label="Shares to redeem" /></label>
              <div className="num text-xs text-ash">≈ {fmt(previewAssets)} {s?.symbol} (needs idle cash)</div>
              <button className="btn-ghost" disabled={!ready || !redAmt || busy || (s ? redAmt > s.myShares : true)} onClick={() => redAmt && void v.redeem(redAmt)}>Redeem</button>
            </div>
          </div>

          <div className="mt-4 flex items-center gap-3">
            <button className="btn-ghost !px-3 !py-1 !text-sm" disabled={!ready || busy} onClick={() => void v.settleFees()}>Settle fee cycle</button>
            <span className="label">anyone may trigger · fee is 0 unless price &gt; HWM</span>
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
