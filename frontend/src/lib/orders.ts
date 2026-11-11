/**
 * Limit and stop orders as the screens describe them (`PendingOrder`,
 * types/market.ts): what kind of order it is, what fills it, what it holds
 * and roughly what it trades. The checkout's receipt and the Vault's Open
 * orders list say the same things in the same words.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { formatMoney, toUnits } from "./decimal.ts";
import { estimateQuantity, formatPrice } from "./market.ts";
import { formatShares } from "./vault.ts";
import type { PendingOrder } from "../types/market.ts";
import type { DecimalString } from "../types/vault.ts";

type Terms = Pick<PendingOrder, "order_type" | "side" | "limit_price" | "stop_price">;

/** "Limit buy", "Limit sell", "Stop buy" or "Stop-loss" (a stop that sells). */
export function orderKind(order: Pick<PendingOrder, "order_type" | "side">): string {
  if (order.order_type === "limit") return order.side === "buy" ? "Limit buy" : "Limit sell";
  return order.side === "buy" ? "Stop buy" : "Stop-loss";
}

/** The price the order waits for: its limit, or its stop. */
export function targetPrice(order: Terms): DecimalString {
  return (order.order_type === "limit" ? order.limit_price : order.stop_price) ?? "0";
}

/**
 * When it fills, after the verb: "at $1,600.00 or lower" (limit buy), "at
 * $1,700.00 or higher" (limit sell), "when the price rises to $1,650.00"
 * (stop buy), "when the price falls to $1,550.00" (stop-loss).
 */
export function triggerPhrase(order: Terms): string {
  const price = formatPrice(targetPrice(order));
  if (order.order_type === "limit") return `at ${price} or ${order.side === "buy" ? "lower" : "higher"}`;
  return `when the price ${order.side === "buy" ? "rises" : "falls"} to ${price}`;
}

/** What it holds while it waits: a buy, its cash ("$1,250.00"); a sell, its units ("0.2941 AUG1"). */
export function heldLabel(order: Pick<PendingOrder, "side" | "ticker" | "held_cash" | "held_quantity">): string {
  return order.side === "buy"
    ? formatMoney(order.held_cash)
    : `${formatShares(order.held_quantity)} ${order.ticker}`;
}

/**
 * Roughly what it trades: a buy, the units its amount buys at its own price
 * (rounded down to `decimals` places, as the backend fills it); a sell,
 * exactly the units it holds. Null for a buy without a positive price.
 */
export function estimatedUnits(
  order: Terms & Pick<PendingOrder, "amount" | "held_quantity">,
  decimals = 4,
): DecimalString | null {
  if (order.side === "sell") return order.held_quantity;
  return estimateQuantity(toUnits(order.amount), targetPrice(order), decimals);
}

/**
 * The sentence that confirms a cancellation, from the order as it was while
 * pending: what was cancelled and what is free again.
 */
export function cancelledSentence(order: PendingOrder): string {
  const what = `${orderKind(order)} of ${order.ticker} cancelled.`;
  return order.side === "buy"
    ? `${what} ${formatMoney(order.held_cash)} is back in your cash.`
    : `${what} Your ${formatShares(order.held_quantity)} ${order.ticker} are free to sell again.`;
}
