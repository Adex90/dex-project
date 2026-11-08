"use client";

import { useEffect, useState } from "react";
import { formatUnits } from "viem";
import { useElapsed } from "@/hooks/useProver";
import { config } from "@/lib/config";
import { bpsToPct, fmt, parseAmount } from "@/lib/format";
import { proofsAvailable, zkeyBytes } from "@/zk/availability";
import { useApp, useShieldedBalances, useSymbols } from "@/components/AppState";
import { IconFlip, IconSettings, IconShield, IconZap } from "@/components/ui/icons";
import { Card, Notice, Row, Skeleton, TokenSelect } from "@/components/ui/primitives";

const SLIPPAGE_PRESETS = [10, 50, 100] as const; // bps

/**
 * § 1 Dark-pool swap. Same state, same hook calls and the same request/validation rules as the old private-swap form:
 * the debounced curve quote, slippage clamped to 0–1000 bps, validity in minutes, `swapper.swap(...)` on submit.
 */
export function SwapCard({ className = "" }: { className?: string }) {
  const { wallet, ready, pool, shielded: w, swapper } = useApp();
  const sym = useSymbols();
  const balances = useShieldedBalances();
  const ps = pool.state;

  const [dir, setDir] = useState<0 | 1>(0);
  const [amount, setAmount] = useState("");
  const [slippage, setSlippage] = useState("50");
  const [minutes, setMinutes] = useState("30");
  const [showSettings, setShowSettings] = useState(false);
  const [answered, setAnswered] = useState<string>();

  const swapAmt = parseAmount(amount);
  const slipBps = BigInt(Math.min(Math.max(Number.parseInt(slippage, 10) || 0, 0), 1000));
  const validity = Math.max(Number.parseInt(minutes, 10) || 0, 0) * 60;

  // Debounced indicative quote, exactly as before; `answered` only tells the UI which request the shown quote belongs to.
  const { requestQuote } = pool;
  const quoteKey = `${swapAmt ?? ""}|${dir}`;
  useEffect(() => {
    const key = `${swapAmt ?? ""}|${dir}`;
    const t = setTimeout(() => void requestQuote(swapAmt, dir === 0).finally(() => setAnswered(key)), 250);
    return () => clearTimeout(t);
  }, [swapAmt, dir, requestQuote]);
  const quoting = !!swapAmt && !!ps && answered !== quoteKey;

  const canAct = ready && w.phase === "ready" && w.inSync !== false && !swapper.busy;
  const unavailable = !config.darkPool || !proofsAvailable;
  const unlocked = w.phase === "ready" || w.phase === "syncing";
  const other: 0 | 1 = dir === 0 ? 1 : 0;
  const tokenOptions = [
    { value: 0 as const, symbol: sym(0) },
    { value: 1 as const, symbol: sym(1) },
  ];
  const minOut = swapAmt && pool.quote !== undefined ? (pool.quote * (10_000n - slipBps)) / 10_000n : undefined;

  // One primary action that always names the next required step.
  type Cta = { label: string; onClick?: () => void; disabled?: boolean };
  let cta: Cta;
  if (!wallet.account) cta = { label: wallet.hasProvider ? "Connect Wallet" : "No wallet detected", onClick: () => void wallet.connect(), disabled: !wallet.hasProvider };
  else if (!wallet.chainOk) cta = { label: "Switch network", onClick: () => void wallet.switchChain() };
  else if (unavailable) cta = { label: "Private swaps unavailable in this build", disabled: true };
  else if (w.phase === "locked" || w.phase === "error") cta = { label: "Unlock shielded wallet", onClick: () => void w.unlock() };
  else if (w.phase === "unlocking") cta = { label: "Unlocking…", disabled: true };
  else if (w.phase === "syncing") cta = { label: "Syncing with the chain…", disabled: true };
  else if (w.inSync === false) cta = { label: "Resync before swapping", onClick: () => void w.sync() };
  else if (swapper.busy) cta = { label: "Proving on this device…", disabled: true };
  else if (!swapAmt) cta = { label: "Enter an amount", disabled: true };
  else if (swapAmt > balances[dir]) cta = { label: `Insufficient shielded ${sym(dir)}`, disabled: true };
  else if (!config.automatorUrl) cta = { label: "No relay configured in this build", disabled: true };
  else cta = { label: "Shield & Swap (ZK)", onClick: () => void swapper.swap({ dir, amount: swapAmt, slippageBps: slipBps, validitySec: validity }) };
  const live = cta.label === "Shield & Swap (ZK)" && canAct;

  return (
    <Card className={className} aria-labelledby="swap-title">
      <header className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 id="swap-title" className="text-2xl font-extrabold tracking-tight">Swap</h2>
          <p className="text-sm text-lavender">Private · settled in batches at one uniform price</p>
        </div>
        <div className="flex items-center gap-2">
          <span className="pill-mint">{ps ? bpsToPct(ps.feeBps) : "0.00%"} fee</span>
          <button
            type="button"
            className={`grid h-10 w-10 place-items-center rounded-full transition hover:bg-plum-700 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia ${showSettings ? "bg-plum-700 text-fuchsia" : "text-lavender"}`}
            aria-expanded={showSettings}
            aria-controls="swap-settings"
            aria-label="Swap settings"
            onClick={() => setShowSettings((s) => !s)}
          >
            <IconSettings />
          </button>
        </div>
      </header>

      {showSettings ? (
        <div id="swap-settings" className="card-inset mb-4 animate-fade-in space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="text-sm font-semibold">Max slippage</span>
            <div className="flex items-center gap-1.5">
              {SLIPPAGE_PRESETS.map((p) => (
                <button
                  key={p}
                  type="button"
                  aria-pressed={slippage === String(p)}
                  onClick={() => setSlippage(String(p))}
                  className={`rounded-full px-3 py-1 text-sm font-semibold transition ${slippage === String(p) ? "bg-fuchsia-deep text-white" : "bg-plum-700/80 text-lavender hover:text-ink"}`}
                >
                  {p / 100}%
                </button>
              ))}
              <label className="flex items-center gap-1.5">
                <input className="field-sm !w-20 text-right" inputMode="numeric" value={slippage} onChange={(e) => setSlippage(e.target.value)} aria-label="Slippage in basis points" />
                <span className="text-xs text-haze">bps</span>
              </label>
            </div>
          </div>
          <div className="flex items-center justify-between gap-2">
            <span className="text-sm font-semibold">Valid for</span>
            <label className="flex items-center gap-1.5">
              <input className="field-sm !w-20 text-right" inputMode="numeric" value={minutes} onChange={(e) => setMinutes(e.target.value)} aria-label="Validity in minutes" />
              <span className="text-xs text-haze">min</span>
            </label>
          </div>
          {slipBps > 100n ? (
            <Notice tone="warn">High slippage: the batch may pay you up to {bpsToPct(slipBps)} less than the curve quote.</Notice>
          ) : null}
        </div>
      ) : null}

      {/* -------------------------------------------------------------------------------------------- you sell */}
      <div className="relative">
        <div className="card-inset">
          <div className="mb-2 flex items-center justify-between text-sm">
            <span className="font-semibold text-lavender">You sell</span>
            <span className="text-haze">
              Shielded <span className="num text-lavender">{unlocked ? fmt(balances[dir]) : "—"}</span>
              {unlocked && balances[dir] > 0n ? (
                <button type="button" className="ml-2 rounded-full bg-fuchsia/15 px-2 py-0.5 text-xs font-bold text-fuchsia hover:bg-fuchsia/25" onClick={() => setAmount(formatUnits(balances[dir], 18))}>
                  MAX
                </button>
              ) : null}
            </span>
          </div>
          <div className="flex items-center gap-3">
            <TokenSelect value={dir} options={tokenOptions} onChange={(v) => setDir(v)} ariaLabel="Asset to sell" />
            <input className="field text-right" inputMode="decimal" placeholder="0.0" value={amount} onChange={(e) => setAmount(e.target.value)} aria-label="Amount to sell" />
          </div>
        </div>

        {/* flip */}
        <div className="relative z-10 -my-3 flex justify-center">
          <button
            type="button"
            onClick={() => setDir((d) => (d === 0 ? 1 : 0))}
            aria-label="Flip assets"
            className="grid h-11 w-11 place-items-center rounded-2xl border-4 border-plum-800 bg-gradient-to-br from-plum-600 to-plum-700 text-fuchsia shadow-glow-fuchsia transition duration-300 hover:rotate-180 hover:scale-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia"
          >
            <IconFlip />
          </button>
        </div>

        {/* ------------------------------------------------------------------------------------------ you receive */}
        <div className="card-inset">
          <div className="mb-2 flex items-center justify-between text-sm">
            <span className="font-semibold text-lavender">You receive</span>
            <span className="text-haze">indicative curve quote</span>
          </div>
          <div className="flex items-center gap-3">
            <TokenSelect value={other} options={tokenOptions} onChange={(v) => setDir(v === 0 ? 1 : 0)} ariaLabel="Asset to buy" />
            <div className="flex h-9 min-w-0 flex-1 items-center justify-end">
              {quoting ? (
                <Skeleton className="h-7 w-32" />
              ) : (
                <output aria-label="Indicative amount received" className="num block truncate text-right text-2xl text-ink animate-fade-in">
                  {swapAmt ? fmt(pool.quote) : <span className="text-haze">0.0</span>}
                </output>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* ---------------------------------------------------------------------------------------------- details */}
      <div className="mt-3 px-1">
        <Row label="Rate">
          {ps ? (
            <span className="num">1 {ps.symbol0} ≈ {fmt(ps.spot, 18, 6)} {ps.symbol1}</span>
          ) : pool.configured && !pool.error ? (
            <Skeleton className="ml-auto h-4 w-36" />
          ) : (
            "—"
          )}
        </Row>
        <Row label="Minimum received (est.)">
          <span className="num">{minOut !== undefined ? `${fmt(minOut)} ${sym(other)}` : "—"}</span>
        </Row>
        <Row label="Max slippage · valid for">
          <span className={slipBps > 100n ? "text-blush" : ""}>{bpsToPct(slipBps)} · {Math.round(validity / 60)} min</span>
        </Row>
      </div>

      <ZkStatus />

      <button
        type="button"
        className={`btn-primary mt-4 w-full py-4 text-xl ${live ? "animate-cta-glow" : ""}`}
        onClick={cta.onClick}
        disabled={cta.disabled || !cta.onClick}
      >
        {cta.label === "Shield & Swap (ZK)" ? <IconZap className="h-5 w-5" /> : null}
        {cta.label}
      </button>
      <p className="mt-3 text-center text-xs leading-relaxed text-haze">
        Funds move into an intent note only the batch circuit can spend: it pays at least your minimum before the deadline, or refunds you after
        it. The relay receives the terms, never a key.
      </p>
    </Card>
  );
}

/**
 * The proving engine as a nested mini-card: artifact download (real byte progress), readiness, and every step of the
 * current action, with a strawberry-red striped bar while a proof is generated on this device.
 */
function ZkStatus() {
  const { shielded: w, swapper } = useApp();
  const status = w.prover.status;
  const proving = swapper.busy && swapper.steps.some((s) => s.state === "active" && s.label.startsWith("Generating"));
  const elapsed = useElapsed(proving);
  const pct = status.phase === "loading" && status.total > 1 ? Math.min(100, Math.round((status.loaded / status.total) * 100)) : 0;
  const last = swapper.steps.at(-1);

  return (
    <div className="card-inset mt-4">
      {/* Screen readers hear each step once, not the ticking seconds counter. */}
      <p className="sr-only" aria-live="polite">
        {last ? `${last.state === "done" ? "Done" : last.state === "error" ? "Failed" : "In progress"}: ${last.label}` : ""}
      </p>
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-center gap-2 text-sm font-semibold">
          <IconShield className="h-4 w-4 text-fuchsia" />
          ZK prover
        </span>
        <span className="text-xs">
          {status.phase === "idle" ? <span className="pill-plum">not loaded</span> : null}
          {status.phase === "loading" ? <span className="pill-blush">loading</span> : null}
          {status.phase === "ready" ? <span className="pill-mint">ready · hash-verified</span> : null}
          {status.phase === "error" ? <span className="pill-berry">error</span> : null}
        </span>
      </div>

      {status.phase === "idle" && proofsAvailable ? (
        <p className="mt-2 text-xs text-haze">
          ≈{(zkeyBytes / 1e6 + 3).toFixed(0)} MB of proving artifacts download once on first use, are checked against a hash compiled into this page, then
          cached. Proofs are generated here, on your device.
        </p>
      ) : null}
      {status.phase === "error" ? <p className="mt-2 text-xs text-berry-soft">{status.message}</p> : null}

      {status.phase === "loading" ? (
        <div className="mt-3">
          <div className="mb-1 flex justify-between text-xs text-lavender">
            <span>{status.label}</span>
            <span className="num">
              {Math.round(status.loaded / 1e5) / 10} / {Math.round(status.total / 1e5) / 10} MB
            </span>
          </div>
          <div role="progressbar" aria-label="Downloading proving artifacts" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className="h-2.5 overflow-hidden rounded-full bg-plum-700">
            {status.total > 1 ? <div className="stripes h-full rounded-full transition-[width] duration-300" style={{ width: `${pct}%` }} /> : <div className="stripes h-full w-1/3 animate-indeterminate rounded-full" />}
          </div>
        </div>
      ) : null}

      {proving ? (
        <div className="mt-3">
          <div className="mb-1 flex justify-between text-xs text-lavender">
            <span>Generating proof on this device</span>
            <span className="num">{elapsed} s</span>
          </div>
          <div role="progressbar" aria-label="Generating proof" aria-valuetext={`${elapsed} seconds elapsed`} className="h-2.5 overflow-hidden rounded-full bg-plum-700">
            <div className="stripes h-full w-full rounded-full" />
          </div>
        </div>
      ) : null}

      {swapper.steps.length > 0 ? (
        <div className="mt-3 border-t border-lavender/10 pt-2">
          <ol className="space-y-0.5">
            {swapper.steps.map((s) => (
              <li key={s.id} className="flex items-baseline justify-between gap-3 text-sm">
                <span className={s.state === "error" ? "text-berry-soft" : s.state === "done" ? "text-mint" : "text-ink"}>
                  {s.state === "done" ? "✓" : s.state === "error" ? "✗" : "…"} {s.label}
                </span>
                <span className="num shrink-0 text-xs text-haze">{s.state === "active" && s.label.startsWith("Generating") ? `${elapsed} s` : s.detail ?? ""}</span>
              </li>
            ))}
          </ol>
          <button type="button" className="btn-ghost -ml-3 mt-1 !py-1 text-xs" onClick={swapper.clearSteps} disabled={swapper.busy}>
            Clear activity
          </button>
        </div>
      ) : null}
    </div>
  );
}
