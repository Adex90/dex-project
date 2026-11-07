"use client";

import { useEffect, useState } from "react";
import type { Address } from "viem";
import { useDarkPool } from "@/hooks/useDarkPool";
import { fmt, parseAmount } from "@/lib/format";
import { config } from "@/lib/config";
import { Notice, Panel, Stat } from "./Panel";

export function DarkPoolPanel({ account, ready }: { account: Address | undefined; ready: boolean }) {
  const pool = useDarkPool(account);
  const [zeroForOne, setZeroForOne] = useState(true);
  const [amount, setAmount] = useState("");
  const [slippage, setSlippage] = useState("50"); // bps
  const s = pool.state;
  const amountIn = parseAmount(amount);
  const slipBps = BigInt(Math.min(Math.max(Number.parseInt(slippage, 10) || 0, 0), 1000));

  const { requestQuote } = pool;
  useEffect(() => {
    const t = setTimeout(() => void requestQuote(amountIn, zeroForOne), 250); // debounce keystrokes
    return () => clearTimeout(t);
  }, [amountIn, zeroForOne, requestQuote]);

  const inSym = s ? (zeroForOne ? s.symbol0 : s.symbol1) : "";
  const outSym = s ? (zeroForOne ? s.symbol1 : s.symbol0) : "";
  const price = s ? s.spot : undefined;
  const canSend = ready && !!amountIn && !!pool.quote && !!config.automatorUrl && pool.status.kind !== "signing";

  return (
    <Panel title="Dark Pool · swap" tag="fee 0.00%">
      {!pool.configured ? (
        <Notice tone="warn">No DarkPool address in this build. Set NEXT_PUBLIC_DARKPOOL_ADDRESS and rebuild.</Notice>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Stat label={`Reserve ${s?.symbol0 ?? "T0"}`} value={fmt(s?.reserve0)} />
            <Stat label={`Reserve ${s?.symbol1 ?? "T1"}`} value={fmt(s?.reserve1)} />
            <Stat label="Spot" value={fmt(price, 18, 6)} sub={s ? `${s.symbol1}/${s.symbol0}` : undefined} tone="ice" />
            <Stat
              label="MEV bounty"
              value={!s ? "—" : s.extractable === undefined ? "oracle stale" : fmt(s.extractable)}
              sub={s ? `${Number(s.extractionBps) / 100}% → Treasury` : undefined}
              tone="acid"
            />
          </div>

          <div className="mt-5 grid gap-3">
            <div className="flex items-end gap-3">
              <label className="flex-1">
                <span className="label">You sell · {inSym || "—"}</span>
                <input className="field" inputMode="decimal" placeholder="0.0" value={amount} onChange={(e) => setAmount(e.target.value)} aria-label="Amount to sell" />
              </label>
              <button className="btn-ghost !px-3 !py-2" onClick={() => setZeroForOne((v) => !v)} aria-label="Flip direction" title="Flip direction">⇅</button>
            </div>
            <div className="border-2 border-rule bg-ink px-3 py-2">
              <div className="label">You receive (quote, before slippage) · {outSym || "—"}</div>
              <div className="num text-lg">{amountIn ? fmt(pool.quote) : "—"}</div>
            </div>
            <label className="flex items-center gap-3">
              <span className="label">Max slippage (bps)</span>
              <input className="field !w-28 !py-1 !text-sm" inputMode="numeric" value={slippage} onChange={(e) => setSlippage(e.target.value)} aria-label="Max slippage in basis points" />
            </label>
            <button className="btn" disabled={!canSend} onClick={() => amountIn && void pool.submitIntent(amountIn, zeroForOne, slipBps)}>
              {pool.status.kind === "signing" ? "Signing…" : "Sign & submit intent"}
            </button>
          </div>

          <Notice tone="warn">
            PROOF LAYER NOT LIVE. This intent is a signed plaintext message to the Automator. Batch settlement hides your
            trade on-chain, but the Automator can see it until the ZK note circuits ship. Do not treat this as private yet.
          </Notice>
          {!config.automatorUrl ? <Notice tone="warn">No Automator URL in this build: quoting works, submission is disabled.</Notice> : null}
          {pool.status.kind === "sent" ? <Notice tone="ok">Intent queued · id {pool.status.id}. It settles with the next batch.</Notice> : null}
          {pool.status.kind === "error" ? <Notice tone="err">{pool.status.message}</Notice> : null}
          {pool.error ? <Notice tone="err">Pool read failed: {pool.error}</Notice> : null}
        </>
      )}
    </Panel>
  );
}
