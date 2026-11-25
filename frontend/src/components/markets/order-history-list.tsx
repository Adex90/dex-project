"use client";

import { useCallback, useEffect, useId } from "react";
import Link from "next/link";
import { History, Search } from "lucide-react";

import { AssetTableSkeleton, TABLE_CARD, TickerChip } from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { useSignOutOn401 } from "@/hooks/use-sign-out-on-401";
import { describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { formatMoney } from "@/lib/decimal";
import { formatPrice } from "@/lib/market";
import { assetPath } from "@/lib/market-data";
import {
  STATUS_LABELS,
  formatOrderTime,
  hasQuantity,
  historyAmount,
  historyKind,
  historyPrice,
  rejectReasonText,
  waited,
} from "@/lib/order-history";
import { formatShares } from "@/lib/vault";
import { useOrderHistoryStore } from "@/store/order-history";
import { useVaultStore } from "@/store/vault";
import type { HistoryStatus, OrderHistoryEntry } from "@/types/market";

/*
 * Order history (GET /api/v1/market/orders/history), on the Orders page:
 * every order the client has placed that is no longer open, newest first.
 * Market orders, limit and stop orders that filled, savings plan purchases,
 * and limit and stop orders cancelled or rejected.
 *
 * - Each row: the ticker (it opens the asset), the kind of order, a status
 *   badge (Filled in gold, Cancelled in zinc, Rejected in red), when it
 *   closed (and, for one that waited for its price, when it was placed), the
 *   price (a fill's, or what it waited for) and the money (a fill's total
 *   with its units; otherwise the amount asked for). A rejected order says
 *   why in plain English.
 * - Pages of 25 with "Load more". The first page reloads whenever the vault
 *   moves, so an order filled or cancelled appears at the top without losing
 *   the older pages already shown.
 */

/** Vault moves this close together (one action's commits) reload once. */
const RELOAD_MS = 150;

const BADGE: Readonly<Record<HistoryStatus, string>> = {
  filled: "border-synthetic-yellow/50 bg-synthetic-yellow/10 text-synthetic-yellow",
  cancelled: "border-border text-muted",
  rejected: "border-danger/50 bg-danger-soft text-danger",
};

const QUIET_BUTTON = cx(
  "inline-flex h-10 items-center gap-1.5 border border-border px-4",
  "text-[11px] font-semibold tracking-[0.18em] text-muted uppercase transition-colors",
  "hover:border-synthetic-yellow/60 hover:text-synthetic-yellow disabled:cursor-wait disabled:opacity-60",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

export function OrderHistoryList({ token, className }: { token: string; className?: string }) {
  const titleId = useId();
  const orders = useOrderHistoryStore((s) => s.orders);
  const cursor = useOrderHistoryStore((s) => s.cursor);
  const status = useOrderHistoryStore((s) => s.status);
  const loadingMore = useOrderHistoryStore((s) => s.loadingMore);
  const error = useOrderHistoryStore((s) => s.error);
  const vaultMoved = useVaultStore((s) => s.updatedAt);
  const signOutOn401 = useSignOutOn401();

  const fetchPage = useCallback(
    (after: string | null, signal: AbortSignal) => api.orderHistory(token, after, signal).catch(signOutOn401),
    [token, signOutOn401],
  );

  // On mount, and whenever the vault moves: an order filled, cancelled or rejected, here or elsewhere.
  useEffect(() => {
    const timer = setTimeout(() => void useOrderHistoryStore.getState().load(fetchPage), RELOAD_MS);
    return () => clearTimeout(timer);
  }, [fetchPage, vaultMoved]);

  if (orders.length === 0) {
    if (status === "idle" || status === "loading") {
      return <AssetTableSkeleton label="Loading your order history…" className={className} rows={4} />;
    }
    if (status === "error") {
      return (
        <section className={cx(TABLE_CARD, "px-5 py-4 sm:px-6", className)} data-testid="order-history-error">
          <p role="alert" className="text-sm text-danger">
            Could not load your order history. {describeError(error)}
          </p>
          <button
            type="button"
            onClick={() => void useOrderHistoryStore.getState().load(fetchPage)}
            className={cx(QUIET_BUTTON, "mt-3")}
          >
            Retry
          </button>
        </section>
      );
    }
  }

  return (
    <section aria-labelledby={titleId} className={cx(TABLE_CARD, className)} data-testid="order-history">
      <header className="flex min-w-0 items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <History className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h2 id={titleId} className="text-base font-semibold tracking-tight">
            Order history
          </h2>
          <p className="text-xs text-muted">Filled, cancelled and rejected orders, newest first</p>
        </div>
      </header>

      {orders.length === 0 ? (
        <div className="flex flex-col items-start gap-3 px-5 py-6 sm:px-6" data-testid="order-history-empty">
          <p className="text-sm text-ink-2">
            No orders yet. Every order you place shows here once it fills, or if it is cancelled or rejected.
          </p>
          <Link href="/dashboard/discover" className={QUIET_BUTTON}>
            <Search className="size-3.5" aria-hidden />
            Discover assets
          </Link>
        </div>
      ) : (
        <table className="w-full border-collapse text-sm">
          <caption className="sr-only">Your past orders, newest first, with their status, price and amount</caption>
          <thead>
            <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
              <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
                Order
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-left font-medium sm:table-cell">
                Status
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium md:table-cell">
                Price
              </th>
              <th scope="col" className="px-5 py-2.5 text-right font-medium sm:px-6">
                Amount
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border border-t border-border">
            {orders.map((entry) => (
              <HistoryRow key={entry.order_id} entry={entry} />
            ))}
          </tbody>
        </table>
      )}

      {orders.length > 0 ? (
        <footer className="flex flex-wrap items-center justify-between gap-3 border-t border-border px-5 py-3 sm:px-6">
          {error !== null ? (
            <p role="alert" className="text-xs text-danger">
              Could not load more. {describeError(error)}
            </p>
          ) : (
            <p className="text-xs text-muted" data-testid="order-history-shown">
              {cursor === null
                ? `${orders.length} ${orders.length === 1 ? "order" : "orders"}: that's all of them`
                : `Showing the latest ${orders.length}`}
            </p>
          )}
          {cursor !== null ? (
            <button
              type="button"
              onClick={() => void useOrderHistoryStore.getState().loadMore(fetchPage)}
              disabled={loadingMore}
              className={QUIET_BUTTON}
              data-testid="order-history-more"
            >
              {loadingMore ? "Loading…" : "Load more"}
            </button>
          ) : null}
        </footer>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function StatusBadge({ status, className }: { status: HistoryStatus; className?: string }) {
  return (
    <span
      className={cx(
        "inline-flex items-center border px-1.5 py-px text-[10px] font-semibold tracking-[0.16em] whitespace-nowrap uppercase",
        BADGE[status],
        className,
      )}
      data-testid="history-status"
    >
      {STATUS_LABELS[status]}
    </span>
  );
}

function HistoryRow({ entry }: { entry: OrderHistoryEntry }) {
  const kind = historyKind(entry);
  const filled = entry.status === "filled";
  // A limit or stop that filled: its fill price first, then what it waited for.
  const waitedFor =
    filled && entry.limit_price !== null
      ? `Limit ${formatPrice(entry.limit_price)}`
      : filled && entry.stop_price !== null
        ? `Stop ${formatPrice(entry.stop_price)}`
        : null;
  const price = historyPrice(entry);
  const units = filled && hasQuantity(entry) && entry.quantity !== null ? formatShares(entry.quantity, 8) : null;

  return (
    <tr
      data-testid="history-row"
      data-order-id={entry.order_id}
      data-status={entry.status}
      data-order-type={entry.order_type}
      data-ticker={entry.ticker}
    >
      {/* The width the other columns leave (w-full max-w-0): a long line wraps, never widens a phone. */}
      <th scope="row" className="w-full max-w-0 py-3.5 pr-2 pl-5 text-left font-normal sm:px-6">
        <div className="flex min-w-0 items-start gap-3">
          <Link
            href={assetPath(entry.ticker)}
            aria-label={`${entry.ticker}: open the asset`}
            className="shrink-0 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
          >
            <TickerChip ticker={entry.ticker} />
          </Link>
          <span className="min-w-0">
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
              <span className="font-medium text-ink" data-testid="history-kind">
                {kind}
              </span>
              <StatusBadge status={entry.status} className="sm:hidden" />
            </span>
            <span className="block text-xs text-muted tabular-nums" data-testid="history-time">
              {formatOrderTime(entry.closed_at)}
              {waited(entry) ? ` · placed ${formatOrderTime(entry.placed_at)}` : null}
            </span>
            {/* The price has its own column from md up. */}
            {price !== "" ? (
              <span className="block text-xs text-ink-2 tabular-nums md:hidden">
                {price}
                {waitedFor ? ` · ${waitedFor}` : null}
              </span>
            ) : null}
            {entry.status === "rejected" ? (
              <span className="mt-0.5 block text-xs text-danger" data-testid="history-reason">
                {rejectReasonText(entry.reject_reason)}
              </span>
            ) : null}
          </span>
        </div>
      </th>
      <td className="hidden px-3 py-3.5 align-top sm:table-cell">
        <StatusBadge status={entry.status} className="mt-0.5" />
      </td>
      <td className="hidden px-3 py-3.5 text-right align-top whitespace-nowrap md:table-cell">
        <span className="block text-ink-2 tabular-nums">{price === "" ? "—" : price}</span>
        {waitedFor ? <span className="block text-xs text-muted tabular-nums">{waitedFor}</span> : null}
      </td>
      <td className="py-3.5 pr-5 pl-2 text-right align-top whitespace-nowrap sm:px-6">
        <span className="block font-medium text-ink tabular-nums" data-testid="history-amount">
          {formatMoney(historyAmount(entry))}
        </span>
        <span className="block text-xs text-muted tabular-nums">
          {units !== null ? `${units} ${entry.ticker}` : filled ? null : "Not filled"}
        </span>
      </td>
    </tr>
  );
}
