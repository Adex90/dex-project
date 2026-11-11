/**
 * Markets: the contract between the trading screens and the backend's market
 * engine (backend/src/mercado, migrations 0013 to 0015). English field names,
 * amounts as exact decimal strings, never floats, the way the vault's is
 * (`types/vault.ts`).
 *
 * - `GET /api/v1/market/assets` → `MarketCatalog` (any signed-in session): the
 *   Discover tab.
 * - `GET /api/v1/market/quote/{ticker}` → `MarketQuote` (any signed-in session).
 * - `POST /api/v1/market/order` (clients only): a market `OrderRequest` →
 *   `OrderReceipt`; a limit or stop one → `PendingOrder` (its cash or units
 *   held; filled at once when a limit is already reachable). 201 for a new
 *   order, 200 when the idempotency key repeats the same order (as it stands
 *   now), 409 when it was used for something else.
 * - `GET /api/v1/market/orders/pending` → `PendingOrders`; `POST
 *   /api/v1/market/orders/{id}/cancel` → the cancelled `PendingOrder`, its
 *   cash or units released.
 * - The backend executes limit and stop orders; the checkout sends them once
 *   `LIVE_ORDER_TYPES` (lib/market.ts) lists them. Savings plans (roadmap
 *   module 4) are contract only: the backend answers 422
 *   `TIPO_ORDEN_NO_DISPONIBLE` until their scheduler exists.
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
 * An order, by type. Without `order_type` the endpoint takes a market order,
 * which is what `api.placeOrder` sends; limit and stop orders carry their
 * price (`limit_price` or `stop_price`, never both).
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
  /** Tells it apart from a limit or stop order (`PendingOrder`). */
  order_type: "market";
  /** A market order fills at once, whole. */
  status: "filled";
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

/** A limit or stop order: it waits for its price. */
export type WaitingOrderType = "limit" | "stop";

/** Where a limit or stop order stands. It only moves forward, from `pending` to one of the other three. */
export type PendingOrderStatus = "pending" | "filled" | "cancelled" | "rejected";

/** The market fill of a limit or stop order, at the price that triggered it. */
export interface OrderExecution {
  /** The fill's own id in the market ledger. */
  order_id: string;
  /** Units bought or sold: a sell sells exactly the units it held. */
  quantity: DecimalString;
  /** Execution price per unit, MXN: never worse than a limit; at or past a stop. */
  price: DecimalString;
  /** MXN charged (buy) or credited (sell): `quantity × price` to the cent. */
  amount: DecimalString;
  /** MXN: always zero. */
  commission: DecimalString;
  executed_at: string;
}

/**
 * A limit or stop order: what placing or cancelling one answers, and each row
 * of the pending list. While pending it holds what it would use: a buy, its
 * whole `amount` in cash; a sell, the units `amount` sells at its own price.
 */
export interface PendingOrder {
  order_id: string;
  order_type: WaitingOrderType;
  status: PendingOrderStatus;
  ticker: string;
  side: OrderSide;
  /** MXN to invest (buy) or to sell, as asked. */
  amount: DecimalString;
  /** Set for limit orders only. */
  limit_price: DecimalString | null;
  /** Set for stop orders only. */
  stop_price: DecimalString | null;
  /** Cash held right now (a pending buy); "0" once closed. */
  held_cash: DecimalString;
  /** Units held right now (a pending sell); "0" once closed. */
  held_quantity: DecimalString;
  created_at: string;
  /** When it filled, was cancelled or rejected; null while pending. */
  closed_at: string | null;
  /** The rule that rejected it when it triggered ("MONTO_INSUFICIENTE"); null unless `rejected`. */
  reject_reason: string | null;
  /** The fill; null unless `filled`. */
  execution: OrderExecution | null;
}

/** `GET /api/v1/market/orders/pending`: the orders still waiting, newest first. */
export interface PendingOrders {
  orders: PendingOrder[];
}

/** What `POST /api/v1/market/order` answers, by the order's type. */
export type PlacedOrder = OrderReceipt | PendingOrder;
