/**
 * Markets: the contract between the trading screens and the backend's market
 * engine (backend/src/mercado, migration 0013). English field names, amounts
 * as exact decimal strings, never floats, the way the vault's is
 * (`types/vault.ts`).
 *
 * - `GET /api/v1/market/quote/{ticker}` → `MarketQuote` (any signed-in session).
 * - `POST /api/v1/market/order` with an `OrderRequest` → `OrderReceipt`
 *   (clients only): 201 for a new order, 200 when the idempotency key repeats
 *   one already filled (the same receipt), 409 when it was used for something
 *   else.
 *
 * Until the price oracle is connected, prices are simulated (`price_source`)
 * and the backend only quotes and fills them in demo mode (otherwise 503
 * `MERCADO_NO_DISPONIBLE`).
 */

import type { DecimalString } from "./vault.ts";

/** One instrument, as its detail view shows it. */
export interface MarketAsset {
  /** "AUG1". */
  ticker: string;
  /** "Tokenized gold, 1 g". */
  name: string;
  /** What it is, in a few words: "Commodity", "Fixed income", "Equity", "Fund". */
  asset_class: string;
  /** Last traded price, MXN per unit. */
  price: DecimalString;
  /** Change against the previous close, percent: "0.84" is +0.84 %. */
  change_24h_pct: DecimalString;
  /** MXN; null where it does not apply (a bill has no market cap). */
  market_cap: DecimalString | null;
  /** MXN traded over the last 24 hours; null when unknown. */
  volume_24h: DecimalString | null;
  /** Annual yield, percent; null when the asset pays none (gold). */
  yield_pct: DecimalString | null;
  /** Decimal places a quantity may have: fractional units (4) or whole units (0). */
  quantity_decimals: number;
}

/** A price at an instant: the chart's point (`BalancePoint` in lib/portfolio.ts). */
export interface PricePoint {
  /** Epoch milliseconds. */
  at: number;
  value: DecimalString;
}

/** `GET /api/v1/market/quote/{ticker}`. */
export interface MarketQuote {
  asset: MarketAsset;
  /**
   * Oldest first, up to a year: every 15 minutes over the last day, hourly
   * over the last week, daily before. The last point is `asset.price` at
   * `as_of`; the point exactly 24 hours earlier is the previous close.
   */
  history: PricePoint[];
  /** When the price was read, ISO 8601: the right edge of every chart range. */
  as_of: string;
  /** "simulated" until the price oracle is connected. */
  price_source: "simulated" | "oracle";
}

export type OrderSide = "buy" | "sell";

/** A market order by amount: buy (or sell) this many pesos' worth. */
export interface OrderRequest {
  ticker: string;
  side: OrderSide;
  /** MXN with at most 2 decimals, as a string ("1250.00"). */
  amount: DecimalString;
  /** 8 to 64 characters [A-Za-z0-9-_.]; one per order, reused on a retry. */
  idempotency_key: string;
}

/**
 * What the backend answers for a placed order. A buy fills as many units as
 * `amount` covers in the asset's increment and charges `quantity × price` to
 * the cent (never more than asked; the rest stays as cash). A sell of what the
 * whole position is worth (the drawer's Max) sells all of it.
 */
export interface OrderReceipt {
  order_id: string;
  /** "filled" at once (market orders); "pending" is reserved for limit orders. */
  status: "filled" | "pending";
  ticker: string;
  side: OrderSide;
  /** Units bought or sold. */
  quantity: DecimalString;
  /** Execution price per unit, MXN. */
  price: DecimalString;
  /** MXN charged (buy) or credited (sell) to the vault. */
  amount: DecimalString;
  /** MXN: always zero. */
  commission: DecimalString;
  /** When it filled, ISO 8601. */
  executed_at: string;
}
