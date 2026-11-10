"use client";

import { useId } from "react";
import Link from "next/link";
import { Coins, Search } from "lucide-react";

import {
  AssetTableSkeleton,
  DailyChange,
  ROW,
  ROW_HEADER,
  RowLink,
  TABLE_CARD,
  TickerChip,
  useRowNavigation,
} from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { addUnits, formatMoney } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import { formatShares } from "@/lib/vault";
import type { Holding } from "@/types/vault";

/*
 * Asset List: the vault's holdings as a terminal-style table on a charcoal
 * card ("Sovereign Gold", globals.css).
 *
 * - Each row opens its asset's page (/dashboard/asset/{ticker}), where the
 *   client buys and sells; the rows, the ticker chip and the daily change are
 *   the Discover catalog's too (components/markets/asset-table.tsx).
 * - Phones get two columns (asset with its shares, value with its daily
 *   change); wider screens get shares and price as their own columns.
 * - With nothing held, it points to Discover, where a first buy starts.
 *
 * Presentational apart from `useId` and the router. Feed it `holdings` from
 * `useVaultStore`; amounts are decimal strings, formatted exactly.
 */

export interface AssetListProps {
  holdings: readonly Holding[];
  className?: string;
}

export function AssetList({ holdings, className }: AssetListProps) {
  const titleId = useId();
  const total = holdings.length > 0 ? addUnits(...holdings.map((h) => h.total_value)) : null;

  return (
    <section aria-labelledby={titleId} className={cx(TABLE_CARD, className)} data-testid="asset-list">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
          >
            <Coins className="size-[18px]" />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">
              Assets
            </h2>
            <p className="text-xs text-muted">
              {holdings.length === 0
                ? "No positions"
                : `${holdings.length} ${holdings.length === 1 ? "position" : "positions"}`}
            </p>
          </div>
        </div>
        {total !== null ? (
          <div className="text-right">
            <p className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">Market value</p>
            <p className="text-lg font-medium tracking-tight text-ink tabular-nums" data-testid="asset-total">
              {formatMoney(total)}
            </p>
          </div>
        ) : null}
      </header>

      {holdings.length === 0 ? (
        <EmptyAssets />
      ) : (
        <table className="w-full border-collapse text-sm">
          <caption className="sr-only">Assets in your vault, with their price and daily change</caption>
          <thead>
            <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
              <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
                Asset
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium md:table-cell">
                Shares
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium sm:table-cell">
                Price
              </th>
              <th scope="col" className="px-5 py-2.5 text-right font-medium sm:px-6">
                Value
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border border-t border-border">
            {holdings.map((holding) => (
              <AssetRow key={holding.ticker} holding={holding} />
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function AssetRow({ holding }: { holding: Holding }) {
  const href = assetPath(holding.ticker);
  const open = useRowNavigation(href);

  return (
    <tr className={ROW} data-testid="asset-row" data-ticker={holding.ticker} onClick={open}>
      <th scope="row" className={ROW_HEADER}>
        <div className="flex min-w-0 items-center gap-3">
          <TickerChip ticker={holding.ticker} />
          <span className="min-w-0">
            <RowLink href={href} testId="asset-row-link">
              {holding.name}
            </RowLink>
            {/* Shares have their own column from md up. */}
            <span className="block text-xs text-muted tabular-nums md:hidden">
              {formatShares(holding.shares)} shares
              <span className="sm:hidden"> · {formatMoney(holding.current_price)}</span>
            </span>
          </span>
        </div>
      </th>
      <td className="hidden px-3 py-3.5 text-right text-ink-2 tabular-nums md:table-cell">
        {formatShares(holding.shares)}
      </td>
      <td className="hidden px-3 py-3.5 text-right text-ink-2 tabular-nums sm:table-cell">
        {formatMoney(holding.current_price)}
      </td>
      <td className="px-5 py-3.5 text-right sm:px-6">
        <span className="block font-medium text-ink tabular-nums">{formatMoney(holding.total_value)}</span>
        <DailyChange pct={holding.daily_change_pct} suffix="today" className="mt-0.5" />
      </td>
    </tr>
  );
}

function EmptyAssets() {
  return (
    <div className="flex flex-col items-center px-6 py-12 text-center" data-testid="asset-list-empty">
      <span
        aria-hidden
        className="grid size-12 place-items-center rounded-2xl border border-dashed border-synthetic-yellow/40 text-synthetic-yellow"
      >
        <Coins className="size-5" />
      </span>
      <p className="mt-4 text-sm font-medium text-ink">No assets yet</p>
      <p className="mt-1 max-w-sm text-sm text-muted">
        Assets you hold will be listed here with their price and daily change.
      </p>
      <Link
        href="/dashboard/discover"
        className={cx(
          "mt-5 inline-flex h-10 items-center gap-2 border border-synthetic-yellow/60 px-4",
          "text-xs font-semibold tracking-[0.18em] text-synthetic-yellow uppercase transition-colors hover:bg-synthetic-yellow/10",
          "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
        )}
        data-testid="asset-list-discover"
      >
        <Search className="size-4" aria-hidden />
        Discover assets
      </Link>
    </div>
  );
}

/** Placeholder with the same footprint, for while the vault loads. */
export function AssetListSkeleton({ className, rows = 3 }: { className?: string; rows?: number }) {
  return <AssetTableSkeleton label="Loading your assets…" className={className} rows={rows} />;
}
