"use client";

import { useEffect, useState } from "react";
import { fmt } from "@/lib/format";
import { useApp } from "@/components/AppState";
import { Loading, Skeleton, Sparkline, TokenIcon } from "@/components/ui/primitives";

/**
 * One point per pool poll (every 12 s while the tab is visible), kept for this session only. Nothing is backfilled
 * or interpolated: the line shows exactly what this page has read from the chain since it opened.
 */
function useSessionSeries(sample: number | undefined, tick: unknown, max = 48): number[] {
  const [pts, setPts] = useState<number[]>([]);
  useEffect(() => {
    if (sample === undefined || !Number.isFinite(sample)) return;
    setPts((p) => [...p, sample].slice(-max));
  }, [tick, sample, max]);
  return pts;
}

/** Listed for the RWA desk; prices appear only once a zk-DECO-attested feed is wired in. */
const PENDING_FEEDS = [
  { ticker: "SPY", name: "S&P 500 ETF" },
  { ticker: "NVDA", name: "NVIDIA" },
] as const;

function PoolTicker() {
  const { pool } = useApp();
  const s = pool.state;
  const series = useSessionSeries(s ? Number(s.spot) / 1e18 : undefined, s);
  const first = series.at(0);
  const last = series.at(-1);
  const change = first && last !== undefined ? ((last - first) / first) * 100 : undefined;
  const up = (change ?? 0) >= 0;

  return (
    <article className="card w-[16.5rem] shrink-0 snap-start !p-4" aria-label={s ? `${s.symbol0}/${s.symbol1} market` : "Pool market"}>
      <header className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-2">
          <span className="flex -space-x-2">
            <TokenIcon symbol={s?.symbol0 ?? "BERRY"} size={26} />
            <TokenIcon symbol={s?.symbol1 ?? "USD"} size={26} />
          </span>
          <span className="font-bold">{s ? `${s.symbol0}/${s.symbol1}` : "Dark pool"}</span>
        </span>
        {pool.configured ? (
          <span className="pill-mint">
            <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-mint" aria-hidden />
            live
          </span>
        ) : (
          <span className="pill-plum">offline</span>
        )}
      </header>
      {pool.configured && !s ? (
        <Loading label="Loading pool price" className="mt-3">
          <Skeleton className="h-7 w-32" />
          <Skeleton className="mt-2 h-10 w-full" />
          <Skeleton className="mt-2 h-4 w-24" />
        </Loading>
      ) : (
        <div className="mt-3 animate-fade-in">
          <div className="num text-2xl font-semibold leading-7">{s ? fmt(s.spot, 18, 6) : "—"}</div>
          <Sparkline points={series} pending={!s} className="mt-2 h-10 w-full" />
          <div className="mt-2 flex items-center justify-between text-xs">
            <span className="text-haze">{s ? `${s.symbol1} per ${s.symbol0}` : "No pool in this build"}</span>
            {change !== undefined ? <span className={up ? "pill-mint" : "pill-berry"}>{`${up ? "+" : ""}${change.toFixed(2)}% this session`}</span> : null}
          </div>
        </div>
      )}
    </article>
  );
}

function PendingTicker({ ticker, name }: { ticker: string; name: string }) {
  return (
    <article className="card w-[16.5rem] shrink-0 snap-start !p-4" aria-label={`${ticker}: no price feed in this build`}>
      <header className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-2">
          <TokenIcon symbol={ticker} size={26} />
          <span>
            <span className="block font-bold leading-tight">{ticker}</span>
            <span className="block text-xs text-haze">{name}</span>
          </span>
        </span>
        <span className="pill-plum">feed pending</span>
      </header>
      <div className="mt-3">
        <div className="num text-2xl font-semibold leading-7 text-haze">—</div>
        <Sparkline points={[]} pending className="mt-2 h-10 w-full" />
        <div className="mt-2 text-xs text-haze">Synthetic via the zk-DECO oracle · not live in this build</div>
      </div>
    </article>
  );
}

/** Horizontally scrollable row of market mini-cards (snap scrolling; native scroll on touch). */
export function MarketStrip({ title = "Markets", className = "" }: { title?: string; className?: string }) {
  return (
    <section className={className} aria-labelledby="markets-title">
      <div className="mb-3 flex items-baseline justify-between">
        <h2 id="markets-title" className="text-lg font-bold">{title}</h2>
        <span className="text-xs text-haze">scroll →</span>
      </div>
      <div className="no-scrollbar -mx-4 flex snap-x snap-mandatory gap-3 overflow-x-auto px-4 pb-2 sm:-mx-6 sm:px-6">
        <PoolTicker />
        {PENDING_FEEDS.map((f) => (
          <PendingTicker key={f.ticker} ticker={f.ticker} name={f.name} />
        ))}
      </div>
    </section>
  );
}
