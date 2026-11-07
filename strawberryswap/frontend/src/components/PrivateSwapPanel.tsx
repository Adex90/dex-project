"use client";

import { useEffect, useMemo, useState } from "react";
import type { Address } from "viem";
import type { useDarkPool } from "@/hooks/useDarkPool";
import { useElapsed } from "@/hooks/useProver";
import { usePrivateSwap } from "@/hooks/usePrivateSwap";
import { useShieldedWallet } from "@/hooks/useShieldedWallet";
import { config } from "@/lib/config";
import { fmt, parseAmount, shortAddr } from "@/lib/format";
import { devKeys, proofsAvailable, zkeyBytes } from "@/zk/availability";
import { Notice, Panel, Stat } from "./Panel";

const when = (unix: bigint): string => new Date(Number(unix) * 1000).toISOString().replace("T", " ").slice(0, 16) + " UTC";

function Bar({ value, total }: { value: number; total: number }) {
  const pct = total > 0 ? Math.min(100, Math.round((value / total) * 100)) : 0;
  return (
    <div role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="mt-2 h-[3px] w-full bg-rule">
      <div className="h-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
    </div>
  );
}

export function PrivateSwapPanel({ account, ready, pool }: { account: Address | undefined; ready: boolean; pool: ReturnType<typeof useDarkPool> }) {
  const w = useShieldedWallet(account);
  const swapper = usePrivateSwap(account, w, pool.state);
  const ps = pool.state;
  const sym = (a: bigint | 0 | 1): string => (ps ? (Number(a) === 0 ? ps.symbol0 : ps.symbol1) : Number(a) === 0 ? "T0" : "T1");

  const [depAsset, setDepAsset] = useState<0 | 1>(0);
  const [depAmount, setDepAmount] = useState("");
  const [dir, setDir] = useState<0 | 1>(0);
  const [amount, setAmount] = useState("");
  const [slippage, setSlippage] = useState("50");
  const [minutes, setMinutes] = useState("30");

  const depAmt = parseAmount(depAmount);
  const swapAmt = parseAmount(amount);
  const slipBps = BigInt(Math.min(Math.max(Number.parseInt(slippage, 10) || 0, 0), 1000));
  const validity = Math.max(Number.parseInt(minutes, 10) || 0, 0) * 60;

  const { requestQuote } = pool;
  useEffect(() => {
    const t = setTimeout(() => void requestQuote(swapAmt, dir === 0), 250);
    return () => clearTimeout(t);
  }, [swapAmt, dir, requestQuote]);

  const status = w.prover.status;
  const proving = swapper.busy && swapper.steps.some((s) => s.state === "active" && s.label.startsWith("Generating"));
  const elapsed = useElapsed(proving);

  const balances = useMemo(() => {
    const acc = [0n, 0n];
    for (const n of w.wallet?.notes ?? []) if (!n.spent && !n.reserved && (n.assetId === 0n || n.assetId === 1n)) acc[Number(n.assetId)] = (acc[Number(n.assetId)] ?? 0n) + n.value;
    return acc;
  }, [w.wallet]);

  const unspent = (w.wallet?.notes ?? []).filter((n) => !n.spent);
  const canAct = ready && w.phase === "ready" && w.inSync !== false && !swapper.busy;
  const unavailable = !config.darkPool || !proofsAvailable;
  const trackedFor = (c: bigint) => swapper.tracked.find((t) => t.commitment === c);

  return (
    <Panel no="3" title="Shielded wallet · private swap" tag="proofs are generated on this device">
      {!proofsAvailable ? (
        <Notice tone="err">This build contains no proving artifacts, so private operations are unavailable. Rebuild after `npm run compile &amp;&amp; npm run setup:dev` in circuits/.</Notice>
      ) : null}
      {proofsAvailable && devKeys ? (
        <Notice tone="warn">Development proving keys (single-party setup). A holder of that setup could forge proofs. Use test funds only.</Notice>
      ) : null}

      {/* ---------------------------------------------------------------- engine */}
      <div className="mt-4">
        <div className="kv">
          <span className="label">Proving engine</span>
          <span className="num text-sm">
            {status.phase === "idle" && "not loaded"}
            {status.phase === "loading" && `${status.label} — ${Math.round(status.loaded / 1e5) / 10} / ${Math.round(status.total / 1e5) / 10} MB`}
            {status.phase === "ready" && "ready (cached, hash-verified)"}
            {status.phase === "error" && <span className="text-neg">{status.message}</span>}
          </span>
        </div>
        {status.phase === "loading" ? <Bar value={status.loaded} total={status.total} /> : null}
        {proofsAvailable && status.phase === "idle" ? (
          <p className="mt-2 text-sm text-muted">The ≈{(zkeyBytes / 1e6 + 3).toFixed(0)} MB proving artifacts download once on first use, are checked against a hash compiled into this page, and are then cached.</p>
        ) : null}
      </div>

      {/* ---------------------------------------------------------------- unlock */}
      {w.phase === "locked" || w.phase === "unlocking" || w.phase === "error" ? (
        <div className="mt-6">
          <p className="text-[15px] text-muted">
            Unlocking asks your wallet to sign one message. The signature stays on this device and derives the key that controls your shielded
            notes inside a Web Worker — your private key and trade parameters are never sent anywhere. Reloading the page forgets it; signing again
            restores everything from the chain.
          </p>
          <button className="btn mt-4" disabled={unavailable || !ready || w.phase === "unlocking"} onClick={() => void w.unlock()}>
            {w.phase === "unlocking" ? "Unlocking…" : "Unlock shielded wallet"}
          </button>
          {w.error ? <Notice tone="err">{w.error}</Notice> : null}
        </div>
      ) : (
        <>
          {/* -------------------------------------------------------------- balances */}
          <div className="mt-6 grid grid-cols-2 gap-x-8 gap-y-5">
            <Stat label={`Shielded ${ps?.symbol0 ?? "T0"}`} value={fmt(balances[0])} sub="spendable" />
            <Stat label={`Shielded ${ps?.symbol1 ?? "T1"}`} value={fmt(balances[1])} sub="spendable" />
          </div>
          <dl className="mt-4 text-[15px]">
            <div className="kv"><dt className="label">Shielded public key</dt><dd className="num">{shortAddr(`0x${(w.wallet?.pubKey ?? 0n).toString(16).padStart(64, "0")}`)}</dd></div>
            <div className="kv"><dt className="label">Notes in tree / scanned</dt><dd className="num">{w.wallet?.treeSize ?? 0}</dd></div>
            <div className="kv">
              <dt className="label">Chain consistency</dt>
              <dd className={w.inSync === false ? "text-neg" : "text-pos"}>{w.inSync === undefined ? "checking…" : w.inSync ? "local tree == on-chain root" : "MISMATCH — resync before acting"}</dd>
            </div>
          </dl>
          <div className="mt-3 flex gap-3">
            <button className="btn-ghost !px-3 !py-1" disabled={swapper.busy} onClick={() => void w.sync()}>Rescan chain</button>
            <button className="btn-ghost !px-3 !py-1" disabled={swapper.busy} onClick={() => void w.lock()}>Lock &amp; forget</button>
          </div>

          {/* -------------------------------------------------------------- forms */}
          <div className="mt-8 grid gap-10 md:grid-cols-2">
            <div className="grid gap-3">
              <h3 className="label">Deposit (public → shielded)</h3>
              <div className="flex gap-2">
                <select className="field !w-28 !text-sm" value={depAsset} onChange={(e) => setDepAsset(Number(e.target.value) as 0 | 1)} aria-label="Deposit asset">
                  <option value={0}>{sym(0)}</option>
                  <option value={1}>{sym(1)}</option>
                </select>
                <input className="field" inputMode="decimal" placeholder="0.0" value={depAmount} onChange={(e) => setDepAmount(e.target.value)} aria-label="Deposit amount" />
              </div>
              <button className="btn" disabled={!canAct || !depAmt} onClick={() => depAmt && void swapper.deposit(depAsset, depAmt)}>Deposit</button>
              <p className="text-xs text-muted">The amount and your address are public at this edge, by definition.</p>
            </div>

            <div className="grid gap-3">
              <h3 className="label">Private swap</h3>
              <div className="flex gap-2">
                <select className="field !w-28 !text-sm" value={dir} onChange={(e) => setDir(Number(e.target.value) as 0 | 1)} aria-label="Asset to sell">
                  <option value={0}>sell {sym(0)}</option>
                  <option value={1}>sell {sym(1)}</option>
                </select>
                <input className="field" inputMode="decimal" placeholder="0.0" value={amount} onChange={(e) => setAmount(e.target.value)} aria-label="Amount to sell" />
              </div>
              <div className="num text-sm text-muted">indicative ≈ {swapAmt ? fmt(pool.quote) : "—"} {sym(dir === 0 ? 1 : 0)} (curve quote; the batch settles at one uniform price)</div>
              <div className="flex flex-wrap items-center gap-4">
                <label className="flex items-center gap-2"><span className="label">Slippage bps</span><input className="field !w-20 !py-1 !text-sm" inputMode="numeric" value={slippage} onChange={(e) => setSlippage(e.target.value)} aria-label="Slippage in basis points" /></label>
                <label className="flex items-center gap-2"><span className="label">Valid for (min)</span><input className="field !w-20 !py-1 !text-sm" inputMode="numeric" value={minutes} onChange={(e) => setMinutes(e.target.value)} aria-label="Validity in minutes" /></label>
              </div>
              <button className="btn" disabled={!canAct || !swapAmt || !config.automatorUrl} onClick={() => swapAmt && void swapper.swap({ dir, amount: swapAmt, slippageBps: slipBps, validitySec: validity })}>
                Create private intent
              </button>
              {!config.automatorUrl ? <p className="text-xs text-warn">No Automator URL in this build.</p> : null}
              <p className="text-xs text-muted">
                Funds move into an <em>intent note</em> that only the batch circuit can spend: it pays you at least your limit before the deadline, or
                refunds you after it. The Automator receives the terms but never a key.
              </p>
            </div>
          </div>

          {/* -------------------------------------------------------------- activity */}
          {swapper.steps.length > 0 ? (
            <div className="mt-8">
              <h3 className="label">Activity</h3>
              <ol className="mt-2 text-[15px]">
                {swapper.steps.map((s) => (
                  <li key={s.id} className="kv">
                    <span className={s.state === "error" ? "text-neg" : s.state === "done" ? "text-pos" : "text-text"}>
                      {s.state === "done" ? "✓" : s.state === "error" ? "✗" : "…"} {s.label}
                    </span>
                    <span className="num text-xs text-muted">{s.state === "active" && s.label.startsWith("Generating") ? `${elapsed} s` : s.detail ?? ""}</span>
                  </li>
                ))}
              </ol>
              <button className="btn-ghost mt-3 !px-3 !py-1" onClick={swapper.clearSteps} disabled={swapper.busy}>Clear</button>
            </div>
          ) : null}

          {/* -------------------------------------------------------------- notes & intents */}
          {unspent.length > 0 ? (
            <div className="mt-8">
              <h3 className="label">Shielded notes</h3>
              <ul className="mt-2 text-[15px]">
                {unspent.map((n) => (
                  <li key={n.commitment.toString()} className="kv">
                    <span className="num">{fmt(n.value)} {sym(n.assetId)} <span className="text-xs text-muted">· leaf {n.index}{n.reserved ? " · in use" : ""}</span></span>
                    <button className="btn-ghost !px-3 !py-0.5 !text-sm" disabled={!canAct || n.reserved} onClick={() => void swapper.withdraw(n.commitment)}>Withdraw</button>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {(w.wallet?.intents.length ?? 0) > 0 ? (
            <div className="mt-8">
              <div className="flex items-baseline justify-between">
                <h3 className="label">Intents</h3>
                <button className="btn-ghost !px-3 !py-0.5 !text-sm" disabled={!canAct} onClick={() => void swapper.resubmit()}>Resubmit pending</button>
              </div>
              <ul className="mt-2 text-[15px]">
                {w.wallet?.intents.map((i) => {
                  const t = trackedFor(i.commitment);
                  return (
                    <li key={i.commitment.toString()} className="kv">
                      <span className="num">{fmt(i.value)} {sym(i.dir)} → ≥ {fmt(i.minOut)} {sym(i.dir === 0 ? 1 : 0)} <span className="text-xs text-muted">· until {when(i.deadline)}</span></span>
                      <span className="label">{i.state === "created" ? (t ? `relay: ${t.state}` : "on-chain · not yet sent to a relay") : i.state}</span>
                    </li>
                  );
                })}
              </ul>
            </div>
          ) : null}
          {w.error ? <Notice tone="err">{w.error}</Notice> : null}
        </>
      )}
    </Panel>
  );
}
