"use client";

import { useId, useMemo } from "react";
import Link from "next/link";
import { BellRing, Star } from "lucide-react";

import {
  DailyChange,
  ROW,
  ROW_HEADER,
  RowLink,
  TABLE_CARD,
  TickerChip,
  useRowNavigation,
} from "@/components/markets/asset-table";
import { WatchError, WatchToggle } from "@/components/markets/watch-toggle";
import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import { alertsFor, targetPhrase } from "@/lib/price-alerts";
import { notHeld } from "@/lib/watchlist";
import { usePriceAlertsStore } from "@/store/price-alerts";
import { useVaultStore } from "@/store/vault";
import { useWatchlistStore } from "@/store/watchlist";
import type { PriceAlert, WatchlistItem } from "@/types/market";

/*
 * Watchlist, on the Vault: the assets the client follows and does not hold,
 * newest first, in the holdings' terminal style (asset-table.tsx). Each row
 * opens its asset; the star unfollows it without opening it.
 *
 * - An asset bought leaves the list (it is in the holdings); sold, it comes
 *   back while it is still followed.
 * - Under the name, the nearest active price target, if there is one.
 * - The dashboard keeps the list current (components/markets/following-sync.tsx).
 *   With nothing to show, or until the vault and the list have loaded, it
 *   renders nothing, so the Vault never jumps for clients without one.
 */

export function WatchlistSection({ className }: { className?: string }) {
  const titleId = useId();
  const items = useWatchlistStore((s) => s.items);
  const pending = useWatchlistStore((s) => s.pending);
  const holdings = useVaultStore((s) => s.holdings);
  const vaultLoaded = useVaultStore((s) => s.balance !== null);
  const alerts = usePriceAlertsStore((s) => s.alerts);
  // An unfollow in flight leaves at once; a failure brings the row back with its reason.
  const shown = useMemo(
    () => notHeld(items, holdings).filter((item) => pending[item.ticker] !== false),
    [items, holdings, pending],
  );

  if (!vaultLoaded || shown.length === 0) return null;

  return (
    <section aria-labelledby={titleId} className={cx(TABLE_CARD, className)} data-testid="vault-watchlist">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
          >
            <Star className="size-[18px] fill-current" strokeWidth={1.5} />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">
              Watchlist
            </h2>
            <p className="text-xs text-muted" data-testid="vault-watchlist-count">
              {shown.length} {shown.length === 1 ? "asset" : "assets"} you&apos;re watching
            </p>
          </div>
        </div>
        <Link
          href="/dashboard/discover"
          className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase transition-colors hover:text-synthetic-yellow focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
        >
          Find more
        </Link>
      </header>

      <table className="w-full border-collapse text-sm">
        <caption className="sr-only">Assets you follow and do not hold, with their price and 24-hour change</caption>
        <thead>
          <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
            <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
              Asset
            </th>
            <th scope="col" className="py-2.5 pr-1 pl-3 text-right font-medium">
              Price
            </th>
            <th scope="col" className="py-2.5 pr-3 pl-0 font-medium sm:pr-4">
              <span className="sr-only">Watchlist</span>
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border border-t border-border">
          {shown.map((item) => (
            <WatchRow key={item.ticker} item={item} alerts={alerts} />
          ))}
        </tbody>
      </table>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function WatchRow({ item, alerts }: { item: WatchlistItem; alerts: readonly PriceAlert[] }) {
  const href = assetPath(item.ticker);
  const open = useRowNavigation(href);
  const { asset } = item;
  const active = useMemo(
    () => alertsFor(alerts, item.ticker, asset.price).filter((alert) => alert.status === "active"),
    [alerts, item.ticker, asset.price],
  );
  const nearest = active[0];

  return (
    <tr className={ROW} data-testid="watchlist-row" data-ticker={item.ticker} onClick={open}>
      <th scope="row" className={ROW_HEADER}>
        <div className="flex min-w-0 items-center gap-3">
          <TickerChip ticker={item.ticker} />
          <span className="min-w-0">
            <RowLink href={href} testId="watchlist-row-link">
              {asset.name}
            </RowLink>
            {nearest ? (
              <span
                className="flex min-w-0 items-center gap-1 text-xs text-synthetic-yellow tabular-nums"
                data-testid="watchlist-row-target"
              >
                <BellRing className="size-3 shrink-0" aria-hidden />
                <span className="truncate">
                  {targetPhrase(nearest)}
                  {active.length > 1 ? ` · ${active.length - 1} more` : null}
                </span>
              </span>
            ) : (
              <span className="block truncate text-xs text-muted">{asset.asset_class}</span>
            )}
            <WatchError ticker={item.ticker} className="mt-0.5 whitespace-normal" />
          </span>
        </div>
      </th>
      <td className="py-3.5 pr-1 pl-3 text-right">
        <span className="block font-medium text-ink tabular-nums">{formatMoney(asset.price)}</span>
        <DailyChange pct={asset.change_24h_pct} suffix="in 24 hours" className="mt-0.5" />
      </td>
      <td className="w-px py-2 pr-3 pl-0 sm:pr-4">
        <WatchToggle ticker={item.ticker} asset={asset} />
      </td>
    </tr>
  );
}
