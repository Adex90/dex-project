"use client";

import { useId } from "react";
import type { LucideIcon } from "lucide-react";

import {
  DailyChange,
  ROW,
  ROW_HEADER,
  RowLink,
  TABLE_CARD,
  TickerChip,
  useRowNavigation,
} from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { formatAmount, formatMoney } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import { compactMoney } from "@/lib/portfolio";
import type { MarketListing } from "@/types/market";

/*
 * Market List: the Discover tab's catalog, in the style of the vault's
 * holdings (components/markets/asset-table.tsx). Each row opens its asset's
 * page.
 *
 * - Phones get two columns: the asset with its class (and yield, when it
 *   pays one), the price with its 24-hour change. From md up the 24-hour
 *   volume has its own column, and from lg up the market cap.
 * - Market figures are compact ("$84.3M"), with the exact amount on hover.
 */

export interface MarketListProps {
  assets: readonly MarketListing[];
  title: string;
  /** Under the title: what the list is ("Most traded in the last 24 hours"). */
  summary: string;
  icon: LucideIcon;
  className?: string;
}

export function MarketList({ assets, title, summary, icon: Icon, className }: MarketListProps) {
  const titleId = useId();

  return (
    <section aria-labelledby={titleId} className={cx(TABLE_CARD, className)} data-testid="market-list">
      <header className="flex min-w-0 items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <Icon className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h2 id={titleId} className="text-base font-semibold tracking-tight">
            {title}
          </h2>
          <p className="truncate text-xs text-muted" data-testid="market-list-summary">
            {summary}
          </p>
        </div>
      </header>

      <table className="w-full border-collapse text-sm">
        <caption className="sr-only">Assets you can trade, with their price and 24-hour change</caption>
        <thead>
          <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
            <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
              Asset
            </th>
            <th scope="col" className="hidden px-3 py-2.5 text-right font-medium lg:table-cell">
              Market cap
            </th>
            <th scope="col" className="hidden px-3 py-2.5 text-right font-medium md:table-cell">
              24h volume
            </th>
            <th scope="col" className="px-5 py-2.5 text-right font-medium sm:px-6">
              Price
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border border-t border-border">
          {assets.map((asset) => (
            <MarketRow key={asset.ticker} asset={asset} />
          ))}
        </tbody>
      </table>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function MarketRow({ asset }: { asset: MarketListing }) {
  const href = assetPath(asset.ticker);
  const open = useRowNavigation(href);

  return (
    <tr className={ROW} data-testid="market-row" data-ticker={asset.ticker} onClick={open}>
      <th scope="row" className={ROW_HEADER}>
        <div className="flex min-w-0 items-center gap-3">
          <TickerChip ticker={asset.ticker} />
          <span className="min-w-0">
            <RowLink href={href} testId="market-row-link">
              {asset.name}
            </RowLink>
            <span className="block truncate text-xs text-muted">
              {asset.asset_class}
              {asset.yield_pct === null ? null : ` · ${formatAmount(asset.yield_pct, 2)}% a year`}
            </span>
          </span>
        </div>
      </th>
      <td className="hidden px-3 py-3.5 text-right text-ink-2 tabular-nums lg:table-cell">
        <Figure value={asset.market_cap} />
      </td>
      <td className="hidden px-3 py-3.5 text-right text-ink-2 tabular-nums md:table-cell">
        <Figure value={asset.volume_24h} />
      </td>
      <td className="px-5 py-3.5 text-right sm:px-6">
        <span className="block font-medium text-ink tabular-nums">{formatMoney(asset.price)}</span>
        <DailyChange pct={asset.change_24h_pct} suffix="in 24 hours" className="mt-0.5" />
      </td>
    </tr>
  );
}

/** A market figure, compact with the exact amount on hover; a dash where it does not apply (a bill's market cap). */
function Figure({ value }: { value: string | null }) {
  if (value === null) {
    return (
      <>
        <span aria-hidden className="text-muted">
          —
        </span>
        <span className="sr-only">Not applicable</span>
      </>
    );
  }
  // Display only: the compact label rounds anyway, and the title carries the exact amount.
  return <span title={formatMoney(value)}>{compactMoney(Number(value))}</span>;
}
