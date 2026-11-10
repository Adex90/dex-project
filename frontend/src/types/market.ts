/**
 * Markets: the contract between the trading screens and the backend's market
 * engine (backend/src/mercado, migrations 0013 and 0014). English field names,
 * amounts as exact decimal strings, never floats, the way the vault's is
 * (`types/vault.ts`).
 *
 * - `GET /api/v1/market/assets` → `MarketCatalog` (any signed-in session): the
 *   Discover tab.
 * - `GET /api/v1/market/quote/{ticker}` → `MarketQuote` (any signed-in session).
 * - `POST /api/v1/market/order` with a market `OrderRequest` → `OrderReceipt`
 *   (clients only): 201 for a new order, 200 when the idempotency key repeats
 *   one already filled (the same receipt), 409 when it was used for something
 *   else.
 * - Limit and stop orders and savings plans (roadmap modules 3 and 4) are
 *   contract only: the checkout builds them, but nothing sends them until the
 *   backend executes them (`LIVE_ORDER_TYPES` in lib/market.ts).
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

/** "simulated" until the price oracle is connected. */
export type PriceSource = "simulated" | "oracle";

/** An asset as the catalog lists it: the asset and where its price comes from. */
export interface MarketListing extends MarketAsset {
  price_source: PriceSource;
}

/**
 * `GET /api/v1/market/assets`: every asset that can be quoted and traded now,
 * by ticker; each opens at `/dashboard/asset/{ticker}`. Without demo mode only
 * live prices are listed, and the backend answers 503 `MERCADO_NO_DISPONIBLE`
 * when none is.
 */
export interface MarketCatalog {
  assets: MarketListing[];
  /** When the prices were read, ISO 8601. */
  as_of: string;
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
  price_source: PriceSource;
}

export type OrderSide = "buy" | "sell";

/**
 * How an order executes: at once at the current price (`market`), only at a
 * price or better (`limit`), once the price reaches a trigger (`stop`), or as
 * a recurring purchase (`savings_plan`).
 */
export type OrderType = "market" | "limit" | "stop" | "savings_plan";

/** How often a savings plan buys: every week, every two weeks, every month. */
export type PlanFrequency = "weekly" | "biweekly" | "monthly";

interface OrderFields {
  ticker: string;
  /** MXN with at most 2 decimals, as a string ("1250.00"): how much to buy, or to sell. */
  amount: DecimalString;
  /** 8 to 64 characters [A-Za-z0-9-_.]; one per order, reused on a retry. */
  idempotency_key: string;
}

/** Fills at once at the current price: the type the backend executes today. */
export interface MarketOrderRequest extends OrderFields {
  order_type: "market";
  side: OrderSide;
}

/** Fills only at `limit_price` or better (at most that to buy, at least that to sell); it may never fill. */
export interface LimitOrderRequest extends OrderFields {
  order_type: "limit";
  side: OrderSide;
  /** MXN per unit, 2 to 4 decimals ("1580.00", "9.9439"). */
  limit_price: DecimalString;
}

/**
 * Becomes a market order once the price reaches `stop_price`: rising to it to
 * buy, falling to it to sell (a stop-loss). It fills at the next available
 * price, which may be past the stop.
 */
export interface StopOrderRequest extends OrderFields {
  order_type: "stop";
  side: OrderSide;
  /** MXN per unit, 2 to 4 decimals: above the current price to buy, below it to sell. */
  stop_price: DecimalString;
}

/** Buys `amount` of the asset every period from the vault's cash (dollar-cost averaging). */
export interface SavingsPlanRequest extends OrderFields {
  order_type: "savings_plan";
  /** Plans only buy. */
  side: "buy";
  frequency: PlanFrequency;
}

/**
 * An order, by type. Today's endpoint takes market orders without the
 * `order_type` field (it rejects unknown fields); `api.placeOrder` sends
 * exactly that, and only for market orders.
 */
export type OrderRequest = MarketOrderRequest | LimitOrderRequest | StopOrderRequest | SavingsPlanRequest;

/**
 * What the backend answers for a placed order. A buy fills as many units as
 * `amount` covers in the asset's increment and charges `quantity × price` to
 * the cent (never more than asked; the rest stays as cash). A sell of what the
 * whole position is worth (the drawer's Max) sells all of it.
 */
export interface OrderReceipt {
  order_id: string;
  /** "filled" at once (market orders); "pending" is reserved for limit and stop orders waiting for their price. */
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
