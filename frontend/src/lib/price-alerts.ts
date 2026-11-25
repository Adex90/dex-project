/**
 * Price alerts (migration 0024): notify the client when an asset's price
 * reaches a target, rising or falling. The backend checks every few seconds
 * and records the price that reached it; the vault's WebSocket brings the
 * change, and the dashboard announces it.
 *
 * - The target's side decides the direction: above the current price, it
 *   waits for the price to rise to it; below, to fall to it. A target the
 *   price is at already cannot be set.
 * - Words for every screen: the asset's card, the Vault and the notice. None
 *   uses the singular "alert" (an AML hold's word, which no client screen may
 *   show); the feature is "Price alerts".
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { toUnits } from "./decimal.ts";
import { amountUnits, formatPrice, priceForApi } from "./market.ts";
import { isTicker } from "./market-data.ts";
import { isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type { AlertDirection, AlertStatus, PriceAlert, PriceAlertRequest, PriceAlerts } from "../types/market.ts";
import type { DecimalString } from "../types/vault.ts";

const DIRECTIONS: readonly AlertDirection[] = ["above", "below"];
const STATUSES: readonly AlertStatus[] = ["active", "triggered", "cancelled"];
/** The backend keeps at most 50 active per client plus a month of triggered ones. */
const MAX_ALERTS = 1_000;

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function isNullable<T>(value: unknown, check: (v: unknown) => v is T): value is T | null {
  return value === null || check(value);
}

/**
 * One price alert; null if malformed or incoherent: its target lies on the
 * side of its direction from the price it was set at, and its status agrees
 * with what closed it (a triggered one has its price and time, a cancelled
 * one its time).
 */
export function parsePriceAlert(value: unknown): PriceAlert | null {
  if (!isRecord(value)) return null;
  const { alert_id, ticker, direction, target_price, status, reference_price, triggered_price } = value;
  const { created_at, triggered_at, cancelled_at } = value;
  if (
    !isText(alert_id, 64) ||
    !isTicker(ticker) ||
    !isOneOf(DIRECTIONS, direction) ||
    !isPositive(target_price) ||
    !isOneOf(STATUSES, status) ||
    !isPositive(reference_price) ||
    !isNullable(triggered_price, isPositive) ||
    !isInstant(created_at) ||
    !isNullable(triggered_at, isInstant) ||
    !isNullable(cancelled_at, isInstant)
  ) {
    return null;
  }
  const target = toUnits(target_price);
  const reference = toUnits(reference_price);
  const reached =
    triggered_price === null ||
    (direction === "above" ? toUnits(triggered_price) >= target : toUnits(triggered_price) <= target);
  const coherent =
    (direction === "above" ? target > reference : target < reference) &&
    (status === "triggered") === (triggered_price !== null) &&
    (status === "triggered") === (triggered_at !== null) &&
    (status === "cancelled") === (cancelled_at !== null) &&
    reached;
  if (!coherent) return null;
  return {
    alert_id,
    ticker,
    direction,
    target_price,
    status,
    reference_price,
    triggered_price,
    created_at,
    triggered_at,
    cancelled_at,
  };
}

/** `GET /api/v1/market/alerts`; null if any alert is malformed or repeats. */
export function parsePriceAlerts(value: unknown): PriceAlerts | null {
  if (!isRecord(value) || !Array.isArray(value.alerts) || value.alerts.length > MAX_ALERTS) return null;
  const alerts: PriceAlert[] = [];
  const ids = new Set<string>();
  for (const raw of value.alerts) {
    const alert = parsePriceAlert(raw);
    if (!alert || ids.has(alert.alert_id)) return null;
    ids.add(alert.alert_id);
    alerts.push(alert);
  }
  return { alerts };
}

/**
 * Which way `target` (as typed: "1700.5") lies from the current price: above
 * or below; null while it is empty, zero or exactly the current price.
 */
export function directionFor(target: string, current: DecimalString): AlertDirection | null {
  const units = amountUnits(target);
  if (units === 0n) return null;
  const now = toUnits(current);
  if (units === now) return null;
  return units > now ? "above" : "below";
}

/** "Rises to $1,700.00" or "Falls to $1,500.00". */
export function targetPhrase(alert: Pick<PriceAlert, "direction" | "target_price">): string {
  return `${alert.direction === "above" ? "Rises" : "Falls"} to ${formatPrice(alert.target_price)}`;
}

/**
 * Before it is set, what it will do: "Notify me when AUG1 rises to
 * $1,700.00"; for a target at the current price, why it cannot be set.
 */
export function notifySentence(ticker: string, target: string, current: DecimalString): string | null {
  if (amountUnits(target) === 0n) return null;
  const direction = directionFor(target, current);
  if (direction === null) return `${ticker} is at ${formatPrice(current)} now: choose a price above or below it.`;
  const verb = direction === "above" ? "rises" : "falls";
  return `Notify me when ${ticker} ${verb} to ${formatPrice(priceForApi(target))}`;
}

/** Once reached: "AUG1 rose to $1,701.20, past your $1,700.00 target." */
export function reachedSentence(alert: PriceAlert): string {
  const verb = alert.direction === "above" ? "rose" : "fell";
  const price = alert.triggered_price ?? alert.target_price;
  const exact = toUnits(price) === toUnits(alert.target_price);
  return exact
    ? `${alert.ticker} ${verb} to ${formatPrice(price)}, your target.`
    : `${alert.ticker} ${verb} to ${formatPrice(price)}, past your ${formatPrice(alert.target_price)} target.`;
}

/** The request for a target typed on the asset's card; null when it cannot be set. */
export function buildPriceAlert(
  ticker: string,
  target: string,
  current: DecimalString,
  idempotencyKey: string,
): PriceAlertRequest | null {
  const direction = directionFor(target, current);
  if (direction === null) return null;
  return { ticker, target_price: priceForApi(target), direction, idempotency_key: idempotencyKey };
}

/** The asset's alerts for its card: active ones first (nearest target first), then the triggered, newest first. */
export function alertsFor(alerts: readonly PriceAlert[], ticker: string, current: DecimalString): PriceAlert[] {
  const now = toUnits(current);
  const distance = (a: PriceAlert) => {
    const d = toUnits(a.target_price) - now;
    return d < 0n ? -d : d;
  };
  const mine = alerts.filter((a) => a.ticker === ticker && a.status !== "cancelled");
  const active = mine.filter((a) => a.status === "active").sort((a, b) => (distance(a) < distance(b) ? -1 : 1));
  const triggered = mine
    .filter((a) => a.status === "triggered")
    .sort((a, b) => Date.parse(b.triggered_at ?? "") - Date.parse(a.triggered_at ?? ""));
  return [...active, ...triggered];
}

/** The triggered alerts to announce: those reached after `since` (epoch ms), newest first. */
export function toAnnounce(alerts: readonly PriceAlert[], since: number): PriceAlert[] {
  return alerts
    .filter((a) => a.status === "triggered" && a.triggered_at !== null && Date.parse(a.triggered_at) > since)
    .sort((a, b) => Date.parse(b.triggered_at ?? "") - Date.parse(a.triggered_at ?? ""));
}

/** "2 active · 1 reached": what a card or a row says about an asset's alerts. */
export function alertsCount(alerts: readonly Pick<PriceAlert, "status">[]): string {
  const active = alerts.filter((a) => a.status === "active").length;
  const reached = alerts.filter((a) => a.status === "triggered").length;
  const parts = [active > 0 ? `${active} active` : "", reached > 0 ? `${reached} reached` : ""].filter(Boolean);
  return parts.length === 0 ? "None set" : parts.join(" · ");
}
