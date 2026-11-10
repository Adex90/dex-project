/**
 * Market data from the API (`types/market.ts`): runtime validation of the
 * catalog, a quote and an order receipt, and the small vault-side helpers the
 * asset page needs.
 *
 * Network data is validated before anything renders it (definition of done,
 * docs/FRONTEND_ROADMAP.md): a malformed payload is rejected whole, never
 * shown in part. Pure functions with relative `.ts` imports, so `node --test`
 * runs them directly.
 */

import { isDecimalString, isNonNegative, isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type {
  MarketAsset,
  MarketCatalog,
  MarketListing,
  MarketQuote,
  OrderReceipt,
  OrderSide,
  PricePoint,
} from "../types/market.ts";
import type { DecimalString, Holding } from "../types/vault.ts";

/** Like the backend's catalog (`mercado.activos.ticker`): a letter, then up to 11 letters or digits. */
const TICKER_RE = /^[A-Z][A-Z0-9]{0,11}$/;
/** A year of history is about 600 points; anything far beyond that is not a quote. */
const MAX_HISTORY = 5_000;
/** Far more assets than the bank lists; anything beyond that is not a catalog. */
const MAX_CATALOG = 1_000;
const SIDES: readonly OrderSide[] = ["buy", "sell"];
const STATUSES = ["filled", "pending"] as const;
const SOURCES = ["simulated", "oracle"] as const;

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

/** `POST /api/v1/market/order`; null if the receipt is malformed. */
export function parseOrderReceipt(value: unknown): OrderReceipt | null {
  if (!isRecord(value)) return null;
  const { order_id, status, ticker, side, quantity, price, amount, commission, executed_at } = value;
  if (
    !isText(order_id, 64) ||
    !isOneOf(STATUSES, status) ||
    !isTicker(ticker) ||
    !isOneOf(SIDES, side) ||
    !isPositive(quantity) ||
    !isPositive(price) ||
    !isPositive(amount) ||
    !isNonNegative(commission) ||
    !isInstant(executed_at)
  ) {
    return null;
  }
  return { order_id, status, ticker, side, quantity, price, amount, commission, executed_at };
}
