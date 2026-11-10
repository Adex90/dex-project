"use client";

import { useState } from "react";
import { bpsToPct, fmt, parseAmount, shortAddr } from "@/lib/format";
import { useApp } from "@/components/AppState";
import { IconChevron, IconVault } from "@/components/ui/icons";
import { Card, Loading, Notice, PreLaunch, Reveal, Row, Skeleton, TokenIcon } from "@/components/ui/primitives";

const WAD = 10n ** 18n;

/** Signed % change of the share price against its 1.00 start (the vault mints the first shares 1:1). Gross of pending fee. */
export function sinceStartPct(sharePrice: bigint): number {
  return Number(((sharePrice - WAD) * 1_000_000n) / WAD) / 10_000;
}

function PerfPill({ pct }: { pct: number }) {
  const cls = pct > 0 ? "pill-mint" : pct < 0 ? "pill-berry" : "pill-plum";
  return (
    <span className={`${cls} text-sm`} title="Share price against 1.00 at launch, gross of any pending performance fee">
      {`${pct > 0 ? "+" : ""}${pct.toFixed(2)}%`} since start
    </span>
  );
}

/**
 * Strategy vault as an expandable list card (NAV, high-water mark, share price, performance pill). The deposit,
 * redeem and settle actions and their guards are the old panel's, unchanged.
 */
export function VaultCard() {
  const { vault: v, ready } = useApp();
  const s = v.state;
  const [open, setOpen] = useState(true);
  const [dep, setDep] = useState("");
  const [red, setRed] = useState("");
  const depAmt = parseAmount(dep);
  const redAmt = parseAmount(red);

  // client-side previews mirror the contract's maths (shares = assets × S / NAV); the chain is authoritative
  const previewShares = s && depAmt && s.totalShares > 0n && s.nav > 0n ? (depAmt * s.totalShares) / s.nav : undefined;
  const previewAssets = s && redAmt && s.totalShares > 0n ? (redAmt * s.nav) / s.totalShares : undefined;
  const busy = v.tx.kind === "pending";
  const inProfit = s ? s.sharePrice > s.highWaterMark : false;
  const loading = v.configured && !s && !v.error;

  if (!v.configured) {
    return (
      <Card>
        <PreLaunch title="Strategy vaults launch with Mainnet">
          Deposits, the share price and the high-water mark appear here once the vault contract is deployed.
        </PreLaunch>
      </Card>
    );
  }

  return (
    <Card className="!p-0 overflow-hidden" aria-labelledby="vault-title">
      {/* ------------------------------------------------------------------------------------------- list row */}
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-controls="vault-details"
        className="flex w-full flex-col gap-4 p-5 text-left transition hover:bg-plum-700/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-fuchsia sm:p-6 md:flex-row md:items-center"
      >
        <span className="flex min-w-0 flex-1 items-center gap-3">
          <span className="relative">
            <span className="grid h-12 w-12 place-items-center rounded-2xl bg-plum-700 text-lavender shadow-inset">
              <IconVault />
            </span>
            {s ? (
              <span className="absolute -bottom-1 -right-1">
                <TokenIcon symbol={s.symbol} size={20} />
              </span>
            ) : null}
          </span>
          <span className="min-w-0">
            <span id="vault-title" className="block truncate text-lg font-bold">Strategy Vault {s ? `· ${s.symbol}` : ""}</span>
            <span className="block h-6">{loading ? <Skeleton className="mt-1 h-5 w-32" /> : s ? <PerfPill pct={sinceStartPct(s.sharePrice)} /> : null}</span>
          </span>
        </span>
        <span className="grid grid-cols-3 gap-3 md:w-[26rem]">
          {[
            ["NAV", s ? `${fmt(s.nav)}` : undefined, s?.symbol],
            ["High-water mark", s ? fmt(s.highWaterMark, 18, 6) : undefined, "per share"],
            ["Share price", s ? fmt(s.sharePrice, 18, 6) : undefined, inProfit ? "above the mark" : "at or below the mark"],
          ].map(([label, value, sub]) => (
            <span key={label} className="min-w-0">
              <span className="eyebrow block truncate">{label}</span>
              <span className="block h-7">{loading ? <Skeleton className="mt-1 h-5 w-20" /> : <span className="num block truncate text-base font-semibold leading-7 animate-fade-in">{value ?? "—"}</span>}</span>
              <span className="block h-4 truncate text-[11px] text-haze">{loading ? null : sub}</span>
            </span>
          ))}
        </span>
        <IconChevron className={`hidden h-5 w-5 shrink-0 text-lavender transition-transform duration-300 md:block ${open ? "rotate-180" : ""}`} />
      </button>

      {/* ------------------------------------------------------------------------------------------- details */}
      {open ? (
        <div id="vault-details" className="animate-fade-in border-t border-lavender/10 p-5 sm:p-6">
          {loading ? (
            // Same footprint as the loaded details (3 rows per column, the two forms, the settle row): no layout shift.
            <Loading label="Loading vault">
              <div className="grid gap-x-8 gap-y-2.5 md:grid-cols-2">
                {[0, 1, 2, 3, 4, 5].map((i) => (
                  <Skeleton key={i} className={`h-6 ${i % 2 ? "w-3/4" : "w-full"}`} />
                ))}
              </div>
              <div className="mt-5 grid gap-4 md:grid-cols-2">
                <Skeleton className="h-[10.75rem] w-full" />
                <Skeleton className="h-[10.75rem] w-full" />
              </div>
              <Skeleton className="mt-4 h-[4.25rem] w-full sm:h-[3.25rem] md:h-8 md:w-1/2" />
            </Loading>
          ) : (
            <Reveal>
              <div className="grid gap-x-8 md:grid-cols-2">
                <div>
                  <Row label="Manager (trade-only role)"><span className="num">{shortAddr(s?.manager)}</span></Row>
                  <Row label="Maximum per trade"><span className="num">{bpsToPct(s?.maxTradeBps)} of NAV</span></Row>
                  <Row label="Maximum loss per trade"><span className="num">{bpsToPct(s?.maxLossBps)}</span></Row>
                </div>
                <div>
                  <Row label="Performance fee"><span className="num">{bpsToPct(s?.feeBps)} above the mark</span></Row>
                  <Row label="Accrued perf. fee">
                    <span className={`num ${inProfit ? "text-blush" : ""}`}>{fmt(s?.pendingFee)} {inProfit ? "· accruing" : "· none due"}</span>
                  </Row>
                  <Row label="Manager withdrawal"><span className="text-mint">none: no such function</span></Row>
                </div>
              </div>

              <div className="mt-5 grid gap-4 md:grid-cols-2">
                <div className="card-inset">
                  <label className="block">
                    <span className="flex justify-between text-sm">
                      <span className="font-semibold">Deposit {s?.symbol ?? "asset"}</span>
                      <span className="text-haze">balance <span className="num text-lavender">{fmt(s?.myAssetBalance)}</span></span>
                    </span>
                    <input className="field mt-2" inputMode="decimal" placeholder="0.0" value={dep} onChange={(e) => setDep(e.target.value)} aria-label="Deposit amount" />
                  </label>
                  <div className="num mt-1 text-xs text-haze">≈ {fmt(previewShares)} shares</div>
                  <button type="button" className="btn-primary mt-3 w-full" disabled={!ready || !depAmt || busy || (s ? depAmt > s.myAssetBalance : true)} onClick={() => depAmt && void v.deposit(depAmt)}>
                    Deposit
                  </button>
                </div>
                <div className="card-inset">
                  <label className="block">
                    <span className="flex justify-between text-sm">
                      <span className="font-semibold">Redeem shares</span>
                      <span className="text-haze">held <span className="num text-lavender">{fmt(s?.myShares)}</span></span>
                    </span>
                    <input className="field mt-2" inputMode="decimal" placeholder="0.0" value={red} onChange={(e) => setRed(e.target.value)} aria-label="Shares to redeem" />
                  </label>
                  <div className="num mt-1 text-xs text-haze">≈ {fmt(previewAssets)} {s?.symbol} (needs idle cash)</div>
                  <button type="button" className="btn-secondary mt-3 w-full !py-3 !text-base" disabled={!ready || !redAmt || busy || (s ? redAmt > s.myShares : true)} onClick={() => redAmt && void v.redeem(redAmt)}>
                    Redeem
                  </button>
                </div>
              </div>

              <div className="mt-4 flex flex-wrap items-center gap-3">
                <button type="button" className="btn-ghost" disabled={!ready || busy} onClick={() => void v.settleFees()}>
                  Settle fee cycle
                </button>
                <span className="text-xs text-haze">Callable by anyone. The fee is zero unless the share price exceeds the high-water mark.</span>
              </div>
            </Reveal>
          )}

          {v.tx.kind === "pending" ? <Notice tone="warn" className="mt-3">{v.tx.label}… confirm in wallet, then wait for inclusion.</Notice> : null}
          {v.tx.kind === "done" ? <Notice tone="ok" className="mt-3">{v.tx.label} confirmed · {shortAddr(v.tx.hash)}</Notice> : null}
          {v.tx.kind === "error" ? <Notice tone="err" className="mt-3">{v.tx.message}</Notice> : null}
        </div>
      ) : null}
      {v.error ? <Notice tone="err" className="m-5 mt-0">Vault read failed: {v.error}</Notice> : null}
    </Card>
  );
}
