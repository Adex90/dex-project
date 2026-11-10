"use client";

import { useState } from "react";
import { MarketChart } from "./MarketChart";
import { changeOver, usePriceSeries, type PriceSeries } from "./priceHistory";
import { Loading, Skeleton, Sparkline, TokenIcon } from "@/components/ui/primitives";

/** Listed for the RWA desk; prices appear only once a zk-DECO-attested feed is wired in. */
const PENDING_FEEDS = [
  { ticker: "SPY", name: "S&P 500 ETF" },
  { ticker: "NVDA", name: "NVIDIA" },
] as const;

const fmt6 = (p: number) => p.toFixed(6);

const cardCls = (selected: boolean) =>
  `card w-[16.5rem] shrink-0 snap-start !p-4 text-left transition duration-200 hover:-translate-y-0.5 hover:border-fuchsia/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia ${
    selected ? "!border-fuchsia shadow-glow-fuchsia" : ""
  }`;

function PoolTicker({ series, selected, onSelect }: { series: PriceSeries; selected: boolean; onSelect: () => void }) {
  const pts = series.points;
  const last = pts.at(-1);
  const change = changeOver(pts, 86_400, Math.floor(Date.now() / 1000));
  const up = (change?.pct ?? 0) >= 0;
  const pair = series.symbol0 && series.symbol1 ? `${series.symbol0}/${series.symbol1}` : "Dark pool";
  const loading = series.configured && pts.length === 0 && series.status === "loading";

  return (
    <button type="button" aria-pressed={selected} onClick={onSelect} className={cardCls(selected)}>
      <span className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-2">
          <span className="flex -space-x-2">
            <TokenIcon symbol={series.symbol0 ?? "BERRY"} size={26} />
            <TokenIcon symbol={series.symbol1 ?? "USD"} size={26} />
          </span>
          <span className="font-bold">{pair}</span>
        </span>
        {series.configured ? (
          <span className="pill-mint">
            <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-mint" aria-hidden />
            live
          </span>
        ) : (
          <span className="pill-plum">pre-launch</span>
        )}
      </span>
      {loading ? (
        <Loading label="Loading pool price" className="mt-3">
          <Skeleton className="h-7 w-32" />
          <Skeleton className="mt-2 h-10 w-full" />
          <Skeleton className="mt-2 h-5 w-24" />
        </Loading>
      ) : (
        <span className="mt-3 block animate-fade-in">
          <span className="num block text-2xl font-semibold leading-7">{last ? fmt6(last.price) : "—"}</span>
          <Sparkline points={pts.slice(-60).map((p) => p.price)} pending={!last} className="mt-2 h-10 w-full" />
          <span className="mt-2 flex h-5 items-center justify-between text-xs">
            <span className="text-haze">{series.symbol1 ? `${series.symbol1} per ${series.symbol0}` : "Launches with Mainnet"}</span>
            {change ? <span className={up ? "pill-mint" : "pill-berry"}>{`${up ? "+" : ""}${change.pct.toFixed(2)}% ${change.full ? "24h" : "range"}`}</span> : null}
          </span>
        </span>
      )}
    </button>
  );
}

function PendingTicker({ ticker, name, selected, onSelect }: { ticker: string; name: string; selected: boolean; onSelect: () => void }) {
  return (
    <button type="button" aria-pressed={selected} onClick={onSelect} className={cardCls(selected)} aria-label={`${ticker}: no price feed in this build`}>
      <span className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-2">
          <TokenIcon symbol={ticker} size={26} />
          <span>
            <span className="block font-bold leading-tight">{ticker}</span>
            <span className="block text-xs text-haze">{name}</span>
          </span>
        </span>
        <span className="pill-plum">feed pending</span>
      </span>
      <span className="mt-3 block">
        <span className="num block text-2xl font-semibold leading-7 text-haze">—</span>
        <Sparkline points={[]} pending className="mt-2 h-10 w-full" />
        <span className="mt-2 block h-5 text-xs text-haze">Synthetic via the zk-DECO oracle · not live in this build</span>
      </span>
    </button>
  );
}

/**
 * Markets: a horizontally scrollable watchlist (snap scrolling) whose selected entry opens in the interactive chart
 * below. The pool's history and live price feed both views from one series.
 */
export function MarketsSection({ title = "Markets", className = "" }: { title?: string; className?: string }) {
  const series = usePriceSeries();
  const [market, setMarket] = useState<string>("pool");
  const pending = PENDING_FEEDS.find((f) => f.ticker === market);
  return (
    <section className={className} aria-labelledby="markets-title">
      <div className="mb-3 flex items-baseline justify-between">
        <h2 id="markets-title" className="text-lg font-bold">{title}</h2>
        <span className="text-xs text-haze">select to chart · scroll →</span>
      </div>
      <div className="no-scrollbar -mx-4 flex snap-x snap-mandatory gap-3 overflow-x-auto px-4 pb-2 pt-1 sm:-mx-6 sm:px-6">
        <PoolTicker series={series} selected={market === "pool"} onSelect={() => setMarket("pool")} />
        {PENDING_FEEDS.map((f) => (
          <PendingTicker key={f.ticker} ticker={f.ticker} name={f.name} selected={market === f.ticker} onSelect={() => setMarket(f.ticker)} />
        ))}
      </div>
      <MarketChart series={series} market={market} pendingName={pending ? `${pending.ticker} · ${pending.name}` : undefined} />
    </section>
  );
}
