/**
 * Order history (`GET /api/v1/market/orders/history`): every order the
 * client placed that is not open any more, newest first. Fills from the
 * market ledger (market orders, limit and stop orders that triggered,
 * savings plan purchases) and limit or stop orders closed without filling
 * (cancelled, or rejected by a rule when they triggered).
 *
 * Validation of each page, and the words the Orders page uses: the kind of
 * order, its status, its price, and why one was rejected in plain English
 * (the reasons a client can act on; any other in one neutral sentence).
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { toUnits } from "./decimal.ts";
import { formatPrice } from "./market.ts";
import { isTicker } from "./market-data.ts";
import { isNonNegative, isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type { HistoryStatus, OrderHistory, OrderHistoryEntry, OrderSide, OrderType } from "../types/market.ts";

const STATUSES: readonly HistoryStatus[] = ["filled", "cancelled", "rejected"];
const TYPES: readonly OrderType[] = ["market", "limit", "stop", "savings_plan"];
const SIDES: readonly OrderSide[] = ["buy", "sell"];
const REASON_RE = /^[A-Z][A-Z0-9_]{0,63}$/;
/** The backend's pages hold at most 100. */
const MAX_PAGE = 100;

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function isNullable<T>(value: unknown, check: (v: unknown) => v is T): value is T | null {
  return value === null || check(value);
}

/**
 * One order of the history; null if malformed or incoherent: a fill has its
 * quantity, price and total, nothing else does; only a rejected one has a
 * reason; each type carries its own price (and a plan's purchase its plan);
 * it closed when or after it was placed.
 */
export function parseHistoryEntry(value: unknown): OrderHistoryEntry | null {
  if (!isRecord(value)) return null;
  const { order_id, status, order_type, side, ticker, amount, limit_price, stop_price } = value;
  const { quantity, price, total, commission, reject_reason, placed_at, closed_at, source_order_id, plan_id } = value;
  if (
    !isText(order_id, 64) ||
    !isOneOf(STATUSES, status) ||
    !isOneOf(TYPES, order_type) ||
    !isOneOf(SIDES, side) ||
    !isTicker(ticker) ||
    !isPositive(amount) ||
    !isNullable(limit_price, isPositive) ||
    !isNullable(stop_price, isPositive) ||
    !isNullable(quantity, isPositive) ||
    !isNullable(price, isPositive) ||
    !isNullable(total, isPositive) ||
    !isNullable(commission, isNonNegative) ||
    !isNullable(reject_reason, (v): v is string => typeof v === "string" && REASON_RE.test(v)) ||
    !isInstant(placed_at) ||
    !isInstant(closed_at) ||
    !isNullable(source_order_id, (v): v is string => isText(v, 64)) ||
    !isNullable(plan_id, (v): v is string => isText(v, 64))
  ) {
    return null;
  }
  const filled = status === "filled";
  const coherent =
    filled === (quantity !== null) &&
    filled === (price !== null) &&
    filled === (total !== null) &&
    filled === (commission !== null) &&
    (status === "rejected") === (reject_reason !== null) &&
    (order_type === "limit") === (limit_price !== null) &&
    (order_type === "stop") === (stop_price !== null) &&
    (order_type === "savings_plan") === (plan_id !== null) &&
    (source_order_id === null || (filled && (order_type === "limit" || order_type === "stop"))) &&
    // Only limit and stop orders close without filling; a plan only buys.
    (filled || order_type === "limit" || order_type === "stop") &&
    (order_type !== "savings_plan" || side === "buy") &&
    Date.parse(closed_at) >= Date.parse(placed_at);
  if (!coherent) return null;
  return {
    order_id,
    status,
    order_type,
    side,
    ticker,
    amount,
    limit_price,
    stop_price,
    quantity,
    price,
    total,
    commission,
    reject_reason,
    placed_at,
    closed_at,
    source_order_id,
    plan_id,
  };
}

/** A page of `GET /api/v1/market/orders/history`; null if any order is malformed or repeats. */
export function parseOrderHistory(value: unknown): OrderHistory | null {
  if (!isRecord(value) || !Array.isArray(value.orders) || value.orders.length > MAX_PAGE) return null;
  const { next_cursor } = value;
  if (!isNullable(next_cursor, (v): v is string => isText(v, 200))) return null;
  const orders: OrderHistoryEntry[] = [];
  const ids = new Set<string>();
  for (const raw of value.orders) {
    const entry = parseHistoryEntry(raw);
    if (!entry || ids.has(entry.order_id)) return null;
    ids.add(entry.order_id);
    orders.push(entry);
  }
  return { orders, next_cursor };
}

/** The Orders page: open orders and the history, linked from the Vault. */
export const ORDERS_PATH = "/dashboard/orders";

export type OrdersTab = "open" | "history";

/** The Orders page on one of its tabs: "/dashboard/orders?tab=history". */
export function ordersPath(tab: OrdersTab): string {
  return tab === "open" ? ORDERS_PATH : `${ORDERS_PATH}?tab=${tab}`;
}

/** The tab a `?tab=` value names; the open orders for anything else. */
export function ordersTab(value: string | null): OrdersTab {
  return value === "history" ? "history" : "open";
}

/** Pages joined in order, each order once (a page loaded twice adds nothing). */
export function appendPage(shown: readonly OrderHistoryEntry[], page: readonly OrderHistoryEntry[]) {
  const seen = new Set(shown.map((e) => e.order_id));
  return [...shown, ...page.filter((e) => !seen.has(e.order_id))];
}

/** "Market buy", "Limit sell", "Stop buy", "Stop-loss", "Savings plan purchase". */
export function historyKind(entry: Pick<OrderHistoryEntry, "order_type" | "side">): string {
  const buy = entry.side === "buy";
  switch (entry.order_type) {
    case "market":
      return buy ? "Market buy" : "Market sale";
    case "limit":
      return buy ? "Limit buy" : "Limit sell";
    case "stop":
      return buy ? "Stop buy" : "Stop-loss";
    case "savings_plan":
      return "Savings plan purchase";
  }
}

export const STATUS_LABELS: Readonly<Record<HistoryStatus, string>> = {
  filled: "Filled",
  cancelled: "Cancelled",
  rejected: "Rejected",
};

/**
 * Why an order was rejected when it triggered, in plain English: the reasons
 * a client can act on in their words, anything else in one neutral sentence
 * that says nothing about why.
 */
const REASONS: Readonly<Record<string, string>> = {
  SALDO_INSUFICIENTE: "Not enough available cash when it triggered.",
  MONTO_INSUFICIENTE: "The amount no longer bought one unit at the price when it triggered.",
  POSICION_INSUFICIENTE: "Not enough units to sell when it triggered.",
  KYC_NO_VERIFICADO: "Your identity verification was pending when it triggered.",
};
const QUIET_REASON = "The bank could not fill it when it triggered. Nothing was charged.";

export function rejectReasonText(code: string | null): string {
  return code !== null && Object.hasOwn(REASONS, code) ? REASONS[code]! : QUIET_REASON;
}

/**
 * The price that matters for the row: a fill's execution price ("at
 * $1,590.00"), else what it waited for ("Limit $1,600.00", "Stop $4,000.00").
 */
export function historyPrice(entry: OrderHistoryEntry): string {
  if (entry.price !== null) return `at ${formatPrice(entry.price)}`;
  if (entry.limit_price !== null) return `Limit ${formatPrice(entry.limit_price)}`;
  if (entry.stop_price !== null) return `Stop ${formatPrice(entry.stop_price)}`;
  return "";
}

/** The money for the row: a fill's total (what was charged or credited), else the amount asked for. */
export function historyAmount(entry: OrderHistoryEntry): string {
  return entry.total ?? entry.amount;
}

/** Whether a fill ran at a different moment than it was placed (a limit, a stop): the row says when it was placed too. */
export function waited(entry: Pick<OrderHistoryEntry, "placed_at" | "closed_at">): boolean {
  return Date.parse(entry.closed_at) - Date.parse(entry.placed_at) >= 1_000;
}

const DATE_PARTS = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/Mexico_City",
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
  hour12: true,
});

/** "Nov 24 · 12:03 PM" in Mexico City; with the year when it is not `now`'s ("Dec 30, 2025 · 9:15 AM"). */
export function formatOrderTime(iso: string, now = Date.now()): string {
  const parts = (ms: number) => {
    const out: Record<string, string> = {};
    for (const part of DATE_PARTS.formatToParts(ms)) out[part.type] = part.value;
    return out;
  };
  const p = parts(Date.parse(iso));
  const year = p.year !== parts(now).year ? `, ${p.year}` : "";
  return `${p.month} ${p.day}${year} · ${p.hour}:${p.minute} ${p.dayPeriod}`;
}

/** Whether the units of a fill are worth showing (a whole-unit asset's "10" or gold's "0.31"). */
export function hasQuantity(entry: Pick<OrderHistoryEntry, "quantity">): boolean {
  return entry.quantity !== null && toUnits(entry.quantity) > 0n;
}
