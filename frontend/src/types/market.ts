/**
 * Markets: the contract between the trading screens and the backend's market
 * engine (backend/src/mercado, migrations 0013, 0015 and 0023). English field
 * names, amounts as exact decimal strings, never floats, the way the vault's
 * is (`types/vault.ts`).
 *
 * - `GET /api/v1/market/assets` → `MarketCatalog` (any signed-in session): the
 *   Discover tab.
 * - `GET /api/v1/market/quote/{ticker}` → `MarketQuote` (any signed-in session).
 * - `POST /api/v1/market/order` (clients only): a market `OrderRequest` →
 *   `OrderReceipt`; a limit or stop one → `PendingOrder` (its cash or units
 *   held; filled at once when a limit is already reachable); a savings plan
 *   → `SavingsPlan`, with its first purchase already made (or skipped). 201
 *   for a new order, 200 when the idempotency key repeats the same order (as
 *   it stands now), 409 when it was used for something else.
 * - `GET /api/v1/market/orders/pending` → `PendingOrders`; `POST
 *   /api/v1/market/orders/{id}/cancel` → the cancelled `PendingOrder`, its
 *   cash or units released.
 * - `GET /api/v1/market/savings-plans` → `SavingsPlans` (active and paused);
 *   `POST /api/v1/market/savings-plans/{id}/pause`, `/resume` and `/cancel`
 *   → the `SavingsPlan` as it stands (repeating one answers the same plan; a
 *   cancelled plan is 409 `PLAN_CANCELADO` to anything but cancel).
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

/**
 * Buys `amount` of the asset every period from the vault's cash
 * (dollar-cost averaging): the first purchase at once, then at 9:00 a.m.
 * Mexico City time every 7 or 14 days, or on the same day of each month.
 */
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

/** Where a savings plan stands: a paused one buys nothing until resumed; cancelled is final. */
export type SavingsPlanStatus = "active" | "paused" | "cancelled";

/** What happened to a plan's last due purchase: bought, or skipped by a rule (not enough cash...). */
export type PlanPurchaseResult = "executed" | "skipped";

/**
 * A savings plan: what creating, pausing, resuming or cancelling one answers,
 * and each row of the plans list. Every purchase is a market order like any
 * other, charged to the vault's cash at the price of the day; a purchase the
 * cash cannot cover is skipped and the plan carries on at its next date.
 */
export interface SavingsPlan {
  plan_id: string;
  /** Tells it apart from a market receipt or a limit or stop order. */
  order_type: "savings_plan";
  status: SavingsPlanStatus;
  ticker: string;
  /** Plans only buy. */
  side: "buy";
  /** MXN per purchase. */
  amount: DecimalString;
  frequency: PlanFrequency;
  /** The day it was created in Mexico City (YYYY-MM-DD): its dates count from it. */
  anchor_date: string;
  /** The next purchase, ISO 8601; null unless active. */
  next_execution_at: string | null;
  /** The next purchase and the two after it, ISO 8601; empty unless active. */
  upcoming: string[];
  /** When the last due purchase was processed (bought or skipped); null before the first. */
  last_execution_at: string | null;
  last_result: PlanPurchaseResult | null;
  /** The rule that stopped the last purchase ("SALDO_INSUFICIENTE"); null unless it was skipped. */
  failure_reason: string | null;
  /** The most recent purchase made, even when a later one was skipped; null before the first. */
  last_purchase: OrderExecution | null;
  /** Purchases made and skipped since it was created. */
  purchases: number;
  skipped: number;
  /** MXN charged by all its purchases. */
  invested: DecimalString;
  /** Units bought by all its purchases. */
  quantity: DecimalString;
  created_at: string;
  cancelled_at: string | null;
}

/** `GET /api/v1/market/savings-plans`: the active and paused plans, newest first. */
export interface SavingsPlans {
  plans: SavingsPlan[];
}

/** The plan actions, as `POST /api/v1/market/savings-plans/{id}/{action}` names them. */
export type PlanAction = "pause" | "resume" | "cancel";

/** What `POST /api/v1/market/order` answers, by the order's type. */
export type PlacedOrder = OrderReceipt | PendingOrder | SavingsPlan;

/* ------------------------------------------------------------------------ */
/* Watchlist, price alerts and order history (migration 0024)               */
/* ------------------------------------------------------------------------ */

/** An asset the client follows, held or not, with its catalog listing. */
export interface WatchlistItem {
  ticker: string;
  /** When it was added, ISO 8601. */
  added_at: string;
  asset: MarketListing;
}

/** `GET /api/v1/market/watchlist`: newest first. */
export interface Watchlist {
  items: WatchlistItem[];
}

/** `POST /api/v1/market/watchlist/{ticker}/toggle`: how it stands now. */
export interface WatchState {
  ticker: string;
  watched: boolean;
  /** Since when; null when not watched. */
  added_at: string | null;
}

/** Which way the price must move to reach the target. */
export type AlertDirection = "above" | "below";

/** An active alert waits; a triggered one reached its target once; cancelled is final. */
export type AlertStatus = "active" | "triggered" | "cancelled";

/**
 * A price alert: notify the client when the price reaches `target_price`,
 * rising (`above`) or falling (`below`). The backend checks every few seconds
 * and records the price that reached it.
 */
export interface PriceAlert {
  alert_id: string;
  ticker: string;
  direction: AlertDirection;
  target_price: DecimalString;
  status: AlertStatus;
  /** The price when it was set. */
  reference_price: DecimalString;
  /** The price that reached the target; null unless triggered. */
  triggered_price: DecimalString | null;
  created_at: string;
  triggered_at: string | null;
  cancelled_at: string | null;
}

/** `GET /api/v1/market/alerts`: the active ones and those triggered in the last 30 days, newest first. */
export interface PriceAlerts {
  alerts: PriceAlert[];
}

/** `POST /api/v1/market/alerts`. */
export interface PriceAlertRequest {
  ticker: string;
  /** MXN per unit, up to 4 decimals, as a string. */
  target_price: DecimalString;
  direction: AlertDirection;
  /** 8 to 64 characters [A-Za-z0-9-_.]; reused on a retry. */
  idempotency_key: string;
}

/** How an order ended: filled, cancelled by the client, or rejected by a rule when it triggered. */
export type HistoryStatus = "filled" | "cancelled" | "rejected";

/** One order of `GET /api/v1/market/orders/history`. */
export interface OrderHistoryEntry {
  /** The fill's id in the market ledger, or the limit or stop order's own. */
  order_id: string;
  status: HistoryStatus;
  order_type: OrderType;
  side: OrderSide;
  ticker: string;
  /** MXN asked for. */
  amount: DecimalString;
  limit_price: DecimalString | null;
  stop_price: DecimalString | null;
  /** Filled: units, execution price, cash charged or credited, commission; null otherwise. */
  quantity: DecimalString | null;
  price: DecimalString | null;
  total: DecimalString | null;
  commission: DecimalString | null;
  /** Rejected: the rule's code ("SALDO_INSUFICIENTE"); null otherwise. */
  reject_reason: string | null;
  placed_at: string;
  closed_at: string;
  /** A fill of a limit or stop order: that order's id. */
  source_order_id: string | null;
  /** A savings plan's purchase: the plan's id. */
  plan_id: string | null;
}

/** `GET /api/v1/market/orders/history`: newest first, a page at a time. */
export interface OrderHistory {
  orders: OrderHistoryEntry[];
  /** For the next page (`?cursor=`); null on the last one. */
  next_cursor: string | null;
}
