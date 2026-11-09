import { useId } from "react";
import { ArrowDownRight, ArrowUpRight, Coins, Minus, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { addUnits, formatMoney } from "@/lib/decimal";
import { formatShares, signedPercent, type Trend } from "@/lib/vault";
import type { Holding } from "@/types/vault";

/*
 * Asset List: the vault's holdings as a terminal-style table on a charcoal
 * card ("Sovereign Gold", globals.css).
 *
 * - Rows raise to the lighter charcoal on hover, with a yellow edge and the
 *   ticker in yellow: a reading aid across the columns (rows are not links).
 * - Phones get two columns (asset with its shares, value with its daily
 *   change); wider screens get shares and price as their own columns.
 * - The daily change carries its sign and an arrow as well as green or red.
 *
 * Presentational and hook-free apart from `useId`. Feed it `holdings` from
 * `useVaultStore`; amounts are decimal strings, formatted exactly.
 */

export interface AssetListProps {
  holdings: readonly Holding[];
  className?: string;
}

const CARD = "rounded-2xl border border-border bg-surface";

export function AssetList({ holdings, className }: AssetListProps) {
  const titleId = useId();
  const total = holdings.length > 0 ? addUnits(...holdings.map((h) => h.total_value)) : null;

  return (
    <section aria-labelledby={titleId} className={cx(CARD, className)} data-testid="asset-list">
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

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
  flat: { Icon: Minus, className: "text-muted" },
};

function AssetRow({ holding }: { holding: Holding }) {
  const change = signedPercent(holding.daily_change_pct);
  const trend = TREND[change.trend];

  return (
    <tr className="group transition-colors hover:bg-surface-2" data-testid="asset-row">
      <th
        scope="row"
        className={cx(
          "relative px-5 py-3.5 text-left font-normal sm:px-6",
          // The yellow edge: invisible at rest, drawn on hover.
          "before:absolute before:inset-y-2 before:left-0 before:w-[3px] before:bg-synthetic-yellow",
          "before:opacity-0 before:transition-opacity group-hover:before:opacity-100",
        )}
      >
        <div className="flex min-w-0 items-center gap-3">
          <span
            className={cx(
              "inline-flex h-7 min-w-14 shrink-0 items-center justify-center rounded-md border border-border bg-bg px-2",
              "font-mono text-xs font-semibold tracking-wide text-ink transition-colors",
              "group-hover:border-synthetic-yellow/50 group-hover:text-synthetic-yellow",
            )}
          >
            {holding.ticker}
          </span>
          <span className="min-w-0">
            <span className="block truncate font-medium text-ink">{holding.name}</span>
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
        <span
          className={cx("mt-0.5 inline-flex items-center gap-1 text-xs font-medium tabular-nums", trend.className)}
          data-trend={change.trend}
        >
          <trend.Icon className="size-3.5" aria-hidden />
          {change.text}
          <span className="sr-only"> today</span>
        </span>
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
    </div>
  );
}

/** Placeholder with the same footprint, for while the vault loads. */
export function AssetListSkeleton({ className, rows = 3 }: { className?: string; rows?: number }) {
  return (
    <div className={cx(CARD, className)} role="status">
      <span className="sr-only">Loading your assets…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
          <div className="size-10 rounded-xl bg-surface-2" />
          <div className="space-y-2">
            <div className="h-4 w-20 rounded-sm bg-surface-2" />
            <div className="h-3 w-28 rounded-sm bg-surface-2" />
          </div>
        </div>
        {Array.from({ length: rows }, (_, i) => (
          <div key={i} className="flex items-center justify-between gap-4 border-b border-border px-5 py-4 last:border-b-0 sm:px-6">
            <div className="flex items-center gap-3">
              <div className="h-7 w-14 rounded-md bg-surface-2" />
              <div className="h-4 w-32 rounded-sm bg-surface-2" />
            </div>
            <div className="h-4 w-20 rounded-sm bg-surface-2" />
          </div>
        ))}
      </div>
    </div>
  );
}
