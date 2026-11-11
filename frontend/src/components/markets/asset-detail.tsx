"use client";

import { useEffect, useId, useMemo, useRef, useState } from "react";
import { ArrowDownRight, ArrowUpRight, Minus, type LucideIcon } from "lucide-react";

import { RangePicker } from "@/components/charts/range-picker";
import { TrendChart } from "@/components/charts/trend-chart";
import { OrderCheckoutDrawer } from "@/components/markets/order-checkout-drawer";
import { cx } from "@/components/ui/primitives";
import { compareAmounts, formatAmount, formatMoney, toUnits } from "@/lib/decimal";
import { positionValue } from "@/lib/market";
import {
  compactMoney,
  drawable,
  pointsInRange,
  readoutDate,
  type BalancePoint,
  type ChartRangeId,
} from "@/lib/portfolio";
import { formatShares, moneyParts, signedPercent, type Trend } from "@/lib/vault";
import type { MarketAsset, OrderReceipt, OrderRequest, OrderSide } from "@/types/market";
import type { DecimalString } from "@/types/vault";

/*
 * Asset Detail: one instrument, full view, in the Sovereign Gold identity.
 *
 * - Header: the ticker as a gold-edged block, the full name, and the price,
 *   large. A new price flashes green or red for a moment as it ticks.
 * - Chart: the shared TrendChart, a gold line over 1D to All. Reading a point
 *   (pointer or keyboard) moves the headline to that point's price and date.
 * - Key stats: market cap, 24-hour volume, yield, on a hairline grid.
 * - A sticky action bar: Sell (charcoal) and Buy (solid gold), massive, held
 *   above the phone's tab bar and at the bottom of the terminal on desktop.
 *   Either opens the checkout drawer.
 *
 * Presentational: the page owns the data (the quote, its history, the cash
 * and the position) and the order call (`onPlaceOrder`). Amounts are exact
 * decimal strings (types/market.ts).
 */

/** The ranges a price is read over. */
const PRICE_RANGES: readonly ChartRangeId[] = ["1D", "1W", "1M", "1Y", "ALL"];

export interface AssetDetailProps {
  asset: MarketAsset;
  /** Price history, oldest first; each point's `value` is the price. */
  history: readonly BalancePoint[];
  /** When the price was read (epoch ms): the right edge of every range. */
  now: number;
  /** Cash available to invest, MXN. */
  availableCash: DecimalString;
  /** Units held; "0" for none. */
  position: DecimalString;
  /**
   * Units a market sale can sell: the position minus units held by pending
   * limit and stop sells. Defaults to the whole position.
   */
  sellable?: DecimalString;
  /** The price streams in as it trades (shows "Live"). */
  live?: boolean;
  /** Places an order: resolves with the receipt, or rejects with the error to show. */
  onPlaceOrder: (order: OrderRequest) => Promise<OrderReceipt>;
  className?: string;
}

export function AssetDetail({
  asset,
  history,
  now,
  availableCash,
  position,
  sellable = position,
  live = false,
  onPlaceOrder,
  className,
}: AssetDetailProps) {
  const titleId = useId();
  const statsId = useId();
  const [range, setRange] = useState<ChartRangeId>("1M");
  const [active, setActive] = useState<number | null>(null);
  const [checkout, setCheckout] = useState<OrderSide | null>(null);
  const tick = usePriceTick(asset.price);

  const shown = useMemo(() => drawable(pointsInRange(history, range, now)), [history, range, now]);
  const focus = active !== null ? (shown[active] ?? null) : null;
  const headline = moneyParts(focus ? focus.value : asset.price);
  const change = signedPercent(asset.change_24h_pct);
  const trend = TREND[change.trend];
  const holds = toUnits(position) > 0n;
  const worth = holds ? positionValue(position, asset.price) : "0";
  // Units held by pending sell orders are not for sale at market.
  const canSell = toUnits(sellable) > 0n;

  return (
    <article aria-labelledby={titleId} className={className} data-testid="asset-detail">
      <section className="border border-border bg-surface p-5 sm:p-7">
        <header>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <span className="inline-flex h-8 items-center border border-synthetic-yellow/60 bg-synthetic-yellow/10 px-2.5 font-mono text-sm font-bold tracking-wider text-synthetic-yellow">
              {asset.ticker}
            </span>
            <span className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
              {asset.asset_class} · MXN
            </span>
            {live ? (
              <span className="inline-flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
                <span className="relative flex size-2" aria-hidden>
                  <span className="absolute inline-flex size-full rounded-full bg-synthetic-yellow opacity-60 motion-safe:animate-ping" />
                  <span className="relative inline-flex size-2 rounded-full bg-synthetic-yellow" />
                </span>
                Live
              </span>
            ) : null}
          </div>
          <h2 id={titleId} className="mt-3 text-2xl font-semibold tracking-tight text-ink sm:text-3xl">
            {asset.name}
          </h2>

          {/* Screen readers get the price and its change once; the styled headline is visual only. */}
          <p className="sr-only" aria-live="polite">
            {`${formatMoney(asset.price)} MXN, ${change.text} in 24 hours`}
          </p>
          <div aria-hidden className="mt-5">
            <p
              className={cx(
                "text-[clamp(2.5rem,9vw,4.5rem)] leading-none font-semibold tracking-tighter tabular-nums transition-colors duration-700",
                focus ? "text-ink" : tick === "up" ? "text-gain" : tick === "down" ? "text-loss" : "text-ink",
              )}
              data-testid="asset-price"
              data-tick={tick ?? undefined}
            >
              {headline.negative ? "−" : null}
              <span className="mr-[0.06em] text-[0.5em] font-medium text-synthetic-yellow">$</span>
              {headline.integer}
              <span className="text-[0.5em] text-muted">.{headline.fraction}</span>
            </p>
            {focus ? (
              <p className="mt-2 text-sm text-muted tabular-nums">{readoutDate(focus.at)}</p>
            ) : (
              <p className={cx("mt-2 inline-flex items-center gap-1.5 text-sm font-semibold tabular-nums", trend.className)}>
                <trend.Icon className="size-4" aria-hidden />
                {change.text}
                <span className="font-normal text-muted">24h</span>
              </p>
            )}
          </div>
        </header>

        <div className="mt-6 flex justify-end">
          <RangePicker
            ranges={PRICE_RANGES}
            value={range}
            onChange={(id) => {
              setRange(id);
              setActive(null);
            }}
          />
        </div>
        <TrendChart
          className="mt-4"
          points={shown}
          curve="linear"
          label={`${asset.name} price chart. Use the arrow keys to read each point.`}
          active={active}
          onActiveChange={setActive}
          describe={(point) => `${formatMoney(point.value)} MXN, ${readoutDate(point.at)}`}
          emptyText={{ title: "No price history yet", body: "The line starts with the first trade." }}
          heightClassName="h-60 sm:h-80"
          lineTestId="asset-line"
        />
      </section>

      <section aria-labelledby={statsId} className="mt-4 sm:mt-6">
        <h3 id={statsId} className="mb-3 text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
          Key stats
        </h3>
        {/* Hairline grid: a 1 px gap over the border color draws the dividers. */}
        <dl className="grid grid-cols-1 gap-px border border-border bg-border sm:grid-cols-3" data-testid="asset-stats">
          <KeyStat label="Market cap" value={asset.market_cap} />
          <KeyStat label="24h volume" value={asset.volume_24h} />
          <div className="bg-surface px-5 py-4">
            <dt className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">Yield</dt>
            <dd className="mt-2 text-xl font-semibold tracking-tight text-ink tabular-nums">
              {asset.yield_pct === null ? "None" : `${formatAmount(asset.yield_pct, 2)}%`}
              <span className="ml-2 text-xs font-medium text-muted">{asset.yield_pct === null ? "" : "a year"}</span>
            </dd>
          </div>
        </dl>
      </section>

      {holds ? (
        <section className="mt-4 flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 border border-border bg-surface px-5 py-4 sm:mt-6">
          <h3 className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">Your position</h3>
          <p className="text-sm tabular-nums">
            <span className="font-semibold text-ink">
              {formatShares(position, asset.quantity_decimals)} {asset.ticker}
            </span>
            <span className="text-muted"> · {formatMoney(worth)}</span>
          </p>
        </section>
      ) : null}

      {/*
        The action bar sticks to the bottom of the screen while the page scrolls:
        above the phone's tab bar (4rem plus the home indicator), at the very
        bottom on desktop. It spans the content column edge to edge.
      */}
      <div
        className="sticky bottom-[calc(4rem+env(safe-area-inset-bottom))] z-20 -mx-4 mt-6 border-t border-border bg-bg/95 px-4 py-3 backdrop-blur-md sm:-mx-6 sm:px-6 lg:bottom-0"
        data-testid="asset-actions"
      >
        <div className="grid grid-cols-2 gap-3">
          <button
            type="button"
            onClick={() => setCheckout("sell")}
            disabled={!canSell}
            title={
              canSell
                ? undefined
                : holds
                  ? `Your ${asset.ticker} is held by pending sell orders.`
                  : `You don't hold any ${asset.ticker}.`
            }
            className={cx(
              "h-14 border border-border-strong bg-surface-2 text-base font-bold tracking-[0.24em] text-white uppercase transition-colors sm:h-16",
              "hover:bg-surface-hover focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
              "disabled:cursor-not-allowed disabled:opacity-40",
            )}
          >
            Sell
          </button>
          <button
            type="button"
            onClick={() => setCheckout("buy")}
            className={cx(
              "h-14 bg-synthetic-yellow text-base font-bold tracking-[0.24em] text-black uppercase transition-colors sm:h-16",
              "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
              "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none",
            )}
            data-testid="asset-buy"
          >
            Buy
          </button>
        </div>
      </div>

      <OrderCheckoutDrawer
        open={checkout !== null}
        side={checkout ?? "buy"}
        onClose={() => setCheckout(null)}
        asset={asset}
        cash={availableCash}
        held={sellable}
        onConfirm={onPlaceOrder}
      />
    </article>
  );
}

/* ------------------------------------------------------------------------ */

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
  flat: { Icon: Minus, className: "text-muted" },
};

/** A market figure, compact ("$1.2B") with the exact amount on hover; "—" when it does not apply. */
function KeyStat({ label, value }: { label: string; value: DecimalString | null }) {
  return (
    <div className="bg-surface px-5 py-4">
      <dt className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">{label}</dt>
      <dd
        className="mt-2 text-xl font-semibold tracking-tight text-ink tabular-nums"
        title={value === null ? undefined : `${formatMoney(value)} MXN`}
      >
        {value === null ? "—" : compactMoney(Number(value))}
      </dd>
    </div>
  );
}

/** "up" or "down" for a moment after the price changes; null otherwise. */
function usePriceTick(price: DecimalString): "up" | "down" | null {
  const previous = useRef(price);
  const [tick, setTick] = useState<{ direction: "up" | "down"; at: number } | null>(null);

  useEffect(() => {
    const before = previous.current;
    previous.current = price;
    if (compareAmounts(price, before) === 0) return;
    const direction = compareAmounts(price, before) > 0 ? "up" : "down";
    const at = Date.now();
    // Deferred: the flash belongs to the next frame, not to this render.
    const start = setTimeout(() => setTick({ direction, at }), 0);
    const end = setTimeout(() => setTick((t) => (t?.at === at ? null : t)), 800);
    return () => {
      clearTimeout(start);
      clearTimeout(end);
    };
  }, [price]);

  return tick?.direction ?? null;
}

/** Placeholder with the same footprint, while the quote or the vault loads. */
export function AssetDetailSkeleton({ className }: { className?: string }) {
  return (
    <div className={className} role="status" data-testid="asset-detail-skeleton">
      <span className="sr-only">Loading the asset…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="border border-border bg-surface p-5 sm:p-7">
          <div className="flex items-center gap-3">
            <div className="h-8 w-16 bg-surface-2" />
            <div className="h-3 w-32 bg-surface-2" />
          </div>
          <div className="mt-4 h-7 w-56 bg-surface-2" />
          <div className="mt-6 h-14 w-64 bg-surface-2 sm:h-16" />
          <div className="mt-3 h-4 w-24 bg-surface-2" />
          <div className="mt-6 ml-auto h-9 w-60 bg-surface-2" />
          <div className="mt-4 h-60 bg-surface-2 sm:h-80" />
        </div>
        <div className="mt-4 grid grid-cols-1 gap-px border border-border bg-border sm:mt-6 sm:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <div key={i} className="bg-surface px-5 py-4">
              <div className="h-3 w-20 bg-surface-2" />
              <div className="mt-3 h-6 w-28 bg-surface-2" />
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
