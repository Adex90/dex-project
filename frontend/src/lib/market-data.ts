/**
 * Market data from the API (`types/market.ts`): runtime validation of the
 * catalog, a quote, an order receipt and limit and stop orders, and the small
 * vault-side helpers the asset page needs.
 *
 * Network data is validated before anything renders it (definition of done,
 * docs/FRONTEND_ROADMAP.md): a malformed payload is rejected whole, never
 * shown in part. Pure functions with relative `.ts` imports, so `node --test`
 * runs them directly.
 */

import { fromUnits, toUnits } from "./decimal.ts";
import { isDecimalString, isNonNegative, isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type {
  MarketAsset,
  MarketCatalog,
  MarketListing,
  MarketQuote,
  OrderExecution,
  OrderReceipt,
  OrderSide,
  PendingOrder,
  PendingOrders,
  PendingOrderStatus,
  PricePoint,
  WaitingOrderType,
} from "../types/market.ts";
import type { DecimalString, Holding } from "../types/vault.ts";

/** Like the backend's catalog (`mercado.activos.ticker`): a letter, then up to 11 letters or digits. */
const TICKER_RE = /^[A-Z][A-Z0-9]{0,11}$/;
/** A year of history is about 600 points; anything far beyond that is not a quote. */
const MAX_HISTORY = 5_000;
/** Far more assets than the bank lists; anything beyond that is not a catalog. */
const MAX_CATALOG = 1_000;
const SIDES: readonly OrderSide[] = ["buy", "sell"];
const SOURCES = ["simulated", "oracle"] as const;
const WAITING_TYPES: readonly WaitingOrderType[] = ["limit", "stop"];
const PENDING_STATUSES: readonly PendingOrderStatus[] = ["pending", "filled", "cancelled", "rejected"];
/** The backend's open-order cap per client is 50; anything far beyond that is not a list. */
const MAX_PENDING = 1_000;
/** A rejection reason is an error code: "MONTO_INSUFICIENTE". */
const REASON_RE = /^[A-Z][A-Z0-9_]{0,63}$/;

export function isTicker(value: unknown): value is string {
  return typeof value === "string" && TICKER_RE.test(value);
}

/** The asset's page in the dashboard. */
export function assetPath(ticker: string): string {
  return `/dashboard/asset/${encodeURIComponent(ticker)}`;
}

/** Units of `ticker` held in the vault: its holding's shares, or "0" without one. */
export function positionFor(holdings: readonly Holding[], ticker: string): DecimalString {
  return holdings.find((holding) => holding.ticker === ticker)?.shares ?? "0";
}

/**
 * Units of `ticker` a market sale can sell now: the holding's shares minus
 * those held by pending limit and stop sells, or "0" without one.
 */
export function sellableFor(holdings: readonly Holding[], ticker: string): DecimalString {
  const holding = holdings.find((h) => h.ticker === ticker);
  return holding ? fromUnits(toUnits(holding.shares) - toUnits(holding.held_shares)) : "0";
}

function isNullable<T>(value: unknown, check: (v: unknown) => v is T): value is T | null {
  return value === null || check(value);
}

/** An ISO 8601 instant the browser can read. */
function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function isQuantityDecimals(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 8;
}

export function parseMarketAsset(value: unknown): MarketAsset | null {
  if (!isRecord(value)) return null;
  const { ticker, name, asset_class, price, change_24h_pct, market_cap, volume_24h, yield_pct, quantity_decimals } =
    value;
  if (
    !isTicker(ticker) ||
    !isText(name, 120) ||
    !isText(asset_class, 40) ||
    !isPositive(price) ||
    !isDecimalString(change_24h_pct) ||
    !isNullable(market_cap, isNonNegative) ||
    !isNullable(volume_24h, isNonNegative) ||
    !isNullable(yield_pct, isDecimalString) ||
    !isQuantityDecimals(quantity_decimals)
  ) {
    return null;
  }
  return { ticker, name, asset_class, price, change_24h_pct, market_cap, volume_24h, yield_pct, quantity_decimals };
}

function parsePricePoint(value: unknown): PricePoint | null {
  if (!isRecord(value)) return null;
  const { at, value: price } = value;
  if (typeof at !== "number" || !Number.isSafeInteger(at) || !isPositive(price)) return null;
  return { at, value: price };
}

/** `GET /api/v1/market/quote/{ticker}`; null if anything is missing, mistyped or out of order. */
export function parseMarketQuote(value: unknown): MarketQuote | null {
  if (!isRecord(value)) return null;
  const asset = parseMarketAsset(value.asset);
  const { as_of, price_source } = value;
  if (!asset || !isInstant(as_of) || !isOneOf(SOURCES, price_source)) return null;
  if (!Array.isArray(value.history) || value.history.length === 0 || value.history.length > MAX_HISTORY) return null;
  const history: PricePoint[] = [];
  for (const raw of value.history) {
    const point = parsePricePoint(raw);
    // Oldest first, one point per instant: the chart reads it as a series.
    const previous = history.at(-1);
    if (!point || (previous && point.at <= previous.at)) return null;
    history.push(point);
  }
  return { asset, history, as_of, price_source };
}

/** `GET /api/v1/market/assets`; null if any listing is malformed or a ticker repeats. */
export function parseMarketCatalog(value: unknown): MarketCatalog | null {
  if (!isRecord(value)) return null;
  const { assets, as_of } = value;
  if (!isInstant(as_of) || !Array.isArray(assets) || assets.length > MAX_CATALOG) return null;
  const listings: MarketListing[] = [];
  const tickers = new Set<string>();
  for (const raw of assets) {
    const asset = parseMarketAsset(raw);
    const source = isRecord(raw) ? raw.price_source : undefined;
    if (!asset || !isOneOf(SOURCES, source) || tickers.has(asset.ticker)) return null;
    tickers.add(asset.ticker);
    listings.push({ ...asset, price_source: source });
  }
  return { assets: listings, as_of };
}

/**
 * `POST /api/v1/market/order` for a market order; null if the receipt is
 * malformed. `order_type` may be absent (a backend before 0015) but, if
 * present, must say market: a limit or stop order is a `PendingOrder`.
 */
export function parseOrderReceipt(value: unknown): OrderReceipt | null {
  if (!isRecord(value)) return null;
  const { order_id, order_type = "market", status, ticker, side } = value;
  const execution = parseExecution(value);
  if (
    !execution ||
    order_type !== "market" ||
    status !== "filled" ||
    !isTicker(ticker) ||
    !isOneOf(SIDES, side) ||
    order_id !== execution.order_id
  ) {
    return null;
  }
  const { quantity, price, amount, commission, executed_at } = execution;
  return { order_id: execution.order_id, order_type, status, ticker, side, quantity, price, amount, commission, executed_at };
}

/** The fill fields a receipt and an execution share. */
function parseExecution(value: Record<string, unknown>): OrderExecution | null {
  const { order_id, quantity, price, amount, commission, executed_at } = value;
  if (
    !isText(order_id, 64) ||
    !isPositive(quantity) ||
    !isPositive(price) ||
    !isPositive(amount) ||
    !isNonNegative(commission) ||
    !isInstant(executed_at)
  ) {
    return null;
  }
  return { order_id, quantity, price, amount, commission, executed_at };
}

/**
 * A limit or stop order (placing or cancelling one, or a row of the pending
 * list); null if malformed or incoherent: each type carries only its price,
 * and the status agrees with what closed it (`closed_at`, `execution`,
 * `reject_reason`).
 */
export function parsePendingOrder(value: unknown): PendingOrder | null {
  if (!isRecord(value)) return null;
  const { order_id, order_type, status, ticker, side, amount, limit_price, stop_price, held_cash, held_quantity } =
    value;
  const { created_at, closed_at, reject_reason } = value;
  const execution = value.execution === null ? null : isRecord(value.execution) ? parseExecution(value.execution) : undefined;
  if (
    !isText(order_id, 64) ||
    !isOneOf(WAITING_TYPES, order_type) ||
    !isOneOf(PENDING_STATUSES, status) ||
    !isTicker(ticker) ||
    !isOneOf(SIDES, side) ||
    !isPositive(amount) ||
    !isNullable(limit_price, isPositive) ||
    !isNullable(stop_price, isPositive) ||
    !isNonNegative(held_cash) ||
    !isNonNegative(held_quantity) ||
    !isInstant(created_at) ||
    !isNullable(closed_at, isInstant) ||
    !isNullable(reject_reason, (v): v is string => typeof v === "string" && REASON_RE.test(v)) ||
    execution === undefined
  ) {
    return null;
  }
  const coherent =
    (order_type === "limit") === (limit_price !== null) &&
    (order_type === "stop") === (stop_price !== null) &&
    (status === "pending") === (closed_at === null) &&
    (status === "filled") === (execution !== null) &&
    (status === "rejected") === (reject_reason !== null);
  if (!coherent) return null;
  return {
    order_id,
    order_type,
    status,
    ticker,
    side,
    amount,
    limit_price,
    stop_price,
    held_cash,
    held_quantity,
    created_at,
    closed_at,
    reject_reason,
    execution,
  };
}

/** `GET /api/v1/market/orders/pending`; null if any order is malformed. */
export function parsePendingOrders(value: unknown): PendingOrders | null {
  if (!isRecord(value) || !Array.isArray(value.orders) || value.orders.length > MAX_PENDING) return null;
  const orders: PendingOrder[] = [];
  for (const raw of value.orders) {
    const order = parsePendingOrder(raw);
    if (!order) return null;
    orders.push(order);
  }
  return { orders };
}
