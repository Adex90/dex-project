"use client";

import { useCallback, useEffect, useId, useMemo, useRef } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Check, Clock, Info, Lock, X } from "lucide-react";

import { AssetTableSkeleton, TABLE_CARD, TickerChip } from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { formatMoney, isZero } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import { cancelledSentence, estimatedUnits, heldLabel, orderKind, triggerPhrase } from "@/lib/orders";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { formatShares } from "@/lib/vault";
import { useAuthStore } from "@/store/auth";
import { useMarketStore } from "@/store/market";
import { useOrdersStore, visibleOrders, type OrdersNotice } from "@/store/orders";
import { useVaultStore } from "@/store/vault";
import type { PendingOrder } from "@/types/market";

/*
 * Open Orders: the client's limit and stop orders still waiting for their
 * price (GET /api/v1/market/orders/pending), at the top of the Vault's
 * holdings, in the Sovereign Gold identity: a terminal-style table on the
 * charcoal card, gold for what is held, a quiet zinc Cancel that turns red.
 *
 * - Each row: the ticker (it opens the asset's page), the kind of order and
 *   what fills it ("Limit buy, at $1,600.00 or lower"), its amount with what
 *   it trades (an estimate for a buy, at its own price; exactly the units a
 *   sell holds), and what it holds, with a lock: a buy's cash, a sell's
 *   units. The header adds up the cash held for orders, from the vault.
 * - Cancel is optimistic (store/orders.ts): the row leaves at once and the
 *   cancellation goes to POST /api/v1/market/orders/{id}/cancel; a failure
 *   brings it back and says why. The released cash or units reach the
 *   balance through the vault's WebSocket, never through arithmetic here.
 * - The list reloads whenever the vault moves (the stream's new version), so
 *   an order placed on another device appears and a filled one disappears.
 * - With no open orders it renders nothing, unless it has something to say
 *   (a cancellation just confirmed). While it loads it shows a placeholder
 *   only if the vault says something is held, so the Vault never jumps for
 *   clients without orders.
 */

/** A confirmation stays this long; an error stays until dismissed or replaced. */
const NOTICE_MS = 6_000;

export function OpenOrdersList({ token, className }: { token: string; className?: string }) {
  const titleId = useId();
  const router = useRouter();
  const all = useOrdersStore((s) => s.orders);
  const cancelling = useOrdersStore((s) => s.cancelling);
  const status = useOrdersStore((s) => s.status);
  const error = useOrdersStore((s) => s.error);
  const notice = useOrdersStore((s) => s.notice);
  const heldCash = useVaultStore((s) => s.balance?.held_cash ?? "0");
  const holdings = useVaultStore((s) => s.holdings);
  const vaultMoved = useVaultStore((s) => s.updatedAt);
  const catalog = useMarketStore((s) => s.catalog);
  const catalogStatus = useMarketStore((s) => s.status);
  const orders = useMemo(() => visibleOrders({ orders: all, cancelling }), [all, cancelling]);

  // A 401 ends the session, as on every dashboard page.
  const endSession = useRef(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  });
  const signOutOn401 = useCallback((error: unknown): never => {
    if (error instanceof ApiError && error.isUnauthorized) endSession.current();
    throw error;
  }, []);

  const load = useCallback(
    () => useOrdersStore.getState().load((signal) => api.pendingOrders(token, signal).catch(signOutOn401)),
    [token, signOutOn401],
  );

  // On mount, and whenever the vault moves: placed, filled, cancelled or rejected elsewhere.
  useEffect(() => {
    void load();
  }, [load, vaultMoved]);

  // Each asset's quantity increment, so a buy's estimate rounds like its fill (Discover may have loaded it already).
  const hasOrders = orders.length > 0;
  useEffect(() => {
    if (hasOrders && catalogStatus === "idle") {
      void useMarketStore.getState().load((signal) => api.marketAssets(token, signal));
    }
  }, [hasOrders, catalogStatus, token]);

  useEffect(() => {
    if (notice === null || notice.kind === "failed") return;
    const timer = setTimeout(() => useOrdersStore.getState().dismissNotice(), NOTICE_MS);
    return () => clearTimeout(timer);
  }, [notice]);

  const cancel = useCallback(
    (order: PendingOrder) =>
      void useOrdersStore
        .getState()
        .cancel(order.order_id, (id) => api.cancelOrder(token, id).catch(signOutOn401)),
    [token, signOutOn401],
  );

  const decimalsFor = (ticker: string) =>
    catalog?.assets.find((asset) => asset.ticker === ticker)?.quantity_decimals ?? 4;

  // Something is held for orders, by the vault's own account: worth a placeholder while the list loads.
  const holds = !isZero(heldCash) || holdings.some((holding) => !isZero(holding.held_shares));

  if (orders.length === 0 && notice === null) {
    if ((status === "idle" || status === "loading") && holds) {
      return <AssetTableSkeleton label="Loading your open orders…" className={className} rows={1} />;
    }
    if (status === "error" && holds) {
      return (
        <section className={cx(TABLE_CARD, "px-5 py-4 sm:px-6", className)} data-testid="open-orders-error">
          <p role="alert" className="text-sm text-danger">
            Could not load your open orders. {describeError(error)}
          </p>
          <button type="button" onClick={() => void load()} className={cx(QUIET_BUTTON, "mt-3")}>
            Retry
          </button>
        </section>
      );
    }
    return null;
  }

  return (
    <section
      id="open-orders"
      aria-labelledby={titleId}
      className={cx(TABLE_CARD, "scroll-mt-24", className)}
      data-testid="open-orders"
    >
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
          >
            <Clock className="size-[18px]" />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">
              Open orders
            </h2>
            <p className="text-xs text-muted" data-testid="open-orders-count">
              {orders.length === 0
                ? "No open orders"
                : `${orders.length} waiting for ${orders.length === 1 ? "its" : "their"} price`}
            </p>
          </div>
        </div>
        {!isZero(heldCash) ? (
          <div className="text-right">
            <p className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">Cash held</p>
            <p
              className="inline-flex items-center gap-1.5 text-lg font-medium tracking-tight text-ink tabular-nums"
              data-testid="open-orders-held"
            >
              <Lock className="size-4 text-synthetic-yellow" aria-hidden />
              {formatMoney(heldCash)}
            </p>
          </div>
        ) : null}
      </header>

      {notice ? <Notice notice={notice} /> : null}

      {orders.length > 0 ? (
        <table className="w-full border-collapse text-sm">
          <caption className="sr-only">Your open limit and stop orders, with what each one holds</caption>
          <thead>
            <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
              <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
                Order
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium sm:table-cell">
                Amount
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium sm:table-cell">
                Held
              </th>
              <th scope="col" className="px-5 py-2.5 text-right font-medium sm:px-6">
                <span className="sr-only">Cancel</span>
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border border-t border-border">
            {orders.map((order) => (
              <OrderRow key={order.order_id} order={order} decimals={decimalsFor(order.ticker)} onCancel={cancel} />
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

const QUIET_BUTTON = cx(
  "inline-flex h-10 items-center gap-1.5 border border-border px-3",
  "text-[11px] font-semibold tracking-[0.18em] text-muted uppercase transition-colors",
  "hover:border-border-strong hover:text-ink",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

function OrderRow({
  order,
  decimals,
  onCancel,
}: {
  order: PendingOrder;
  decimals: number;
  onCancel: (order: PendingOrder) => void;
}) {
  const kind = orderKind(order);
  const when = triggerPhrase(order);
  const units = estimatedUnits(order, decimals);
  // A buy's quantity is an estimate at its own price; a sell holds exactly the units it sells.
  const trades =
    units === null ? null : `${order.side === "buy" ? "≈ " : ""}${formatShares(units, decimals)} ${order.ticker}`;
  const held = heldLabel(order);

  return (
    <tr data-testid="open-order" data-order-id={order.order_id} data-ticker={order.ticker}>
      {/* The width the other columns leave (w-full max-w-0): a long line wraps or truncates, never widens a phone. */}
      <th scope="row" className="w-full max-w-0 py-3.5 pr-2 pl-5 text-left font-normal sm:px-6">
        {/* On a phone the details stack under the kind: the chip stays at the top. */}
        <div className="flex min-w-0 items-start gap-3 sm:items-center">
          <Link
            href={assetPath(order.ticker)}
            aria-label={`${order.ticker}: open the asset`}
            className="shrink-0 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
          >
            <TickerChip ticker={order.ticker} />
          </Link>
          <span className="min-w-0">
            <span className="block truncate font-medium text-ink">{kind}</span>
            <span className="block text-xs text-muted tabular-nums" data-testid="open-order-trigger">
              {when}
            </span>
            {/* The amount and what it holds have their own columns from sm up. */}
            <span className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs tabular-nums sm:hidden">
              <span className="whitespace-nowrap text-ink-2">{formatMoney(order.amount)}</span>
              <span className="inline-flex items-center gap-1 whitespace-nowrap text-ink-2">
                <Lock className="size-3 text-synthetic-yellow" aria-hidden />
                Held {held}
              </span>
            </span>
          </span>
        </div>
      </th>
      <td className="hidden px-3 py-3.5 text-right whitespace-nowrap sm:table-cell">
        <span className="block font-medium text-ink tabular-nums">{formatMoney(order.amount)}</span>
        {trades ? <span className="block text-xs text-muted tabular-nums">{trades}</span> : null}
      </td>
      <td className="hidden px-3 py-3.5 text-right whitespace-nowrap sm:table-cell" data-testid="open-order-held">
        <span className="inline-flex items-center gap-1.5 font-medium text-ink tabular-nums">
          <Lock className="size-3.5 text-synthetic-yellow" aria-hidden />
          {held}
        </span>
        <span className="block text-xs text-muted">until it fills</span>
      </td>
      <td className="py-3.5 pr-5 pl-1 text-right align-top sm:px-6 sm:align-middle">
        <button
          type="button"
          onClick={() => onCancel(order)}
          aria-label={`Cancel the ${kind.toLowerCase()} of ${order.ticker} ${when}`}
          className={cx(
            "inline-flex h-10 items-center gap-1.5 border border-border px-2.5 sm:px-3",
            "text-[11px] font-semibold tracking-[0.14em] text-muted uppercase transition-colors sm:tracking-[0.18em]",
            "hover:border-danger/60 hover:bg-danger-soft hover:text-danger",
            "focus-visible:ring-2 focus-visible:ring-danger focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
          )}
          data-testid="open-order-cancel"
        >
          <X className="size-3.5" aria-hidden />
          Cancel
        </button>
      </td>
    </tr>
  );
}

/** What the last cancellation did. Announced politely; an error persists until dismissed. */
function Notice({ notice }: { notice: OrdersNotice }) {
  const failed = notice.kind === "failed";
  const Icon = notice.kind === "cancelled" ? Check : failed ? X : Info;
  const text =
    notice.kind === "cancelled"
      ? cancelledSentence(notice.order)
      : notice.kind === "gone"
        ? `The ${orderKind(notice.order).toLowerCase()} of ${notice.order.ticker} is no longer open: it filled or was rejected.`
        : `Could not cancel the ${orderKind(notice.order).toLowerCase()} of ${notice.order.ticker}. ${describeError(notice.error)}`;
  return (
    <div
      className={cx(
        "flex items-start gap-2.5 border-b px-5 py-3 text-sm sm:px-6",
        failed ? "border-danger/40 bg-danger-soft text-danger" : "border-border bg-bg text-ink-2",
      )}
      data-testid="open-orders-notice"
      data-kind={notice.kind}
    >
      <Icon className={cx("mt-0.5 size-4 shrink-0", failed ? "text-danger" : "text-synthetic-yellow")} aria-hidden />
      <p role={failed ? "alert" : "status"} className="min-w-0 flex-1">
        {text}
      </p>
      <button
        type="button"
        onClick={() => useOrdersStore.getState().dismissNotice()}
        aria-label="Dismiss"
        className="grid size-6 shrink-0 place-items-center text-muted transition-colors hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
      >
        <X className="size-3.5" aria-hidden />
      </button>
    </div>
  );
}
