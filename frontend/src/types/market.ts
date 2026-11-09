/**
 * Markets: the contract the trading screens are built against, ahead of the
 * backend (docs/FRONTEND_ROADMAP.md, Phase 1, "contracts before screens").
 * A proposal to review with the backend team, written the way the vault's
 * was (`types/vault.ts`): English field names, amounts as exact decimal
 * strings, never floats.
 */

import type { DecimalString } from "./vault.ts";

/** One instrument, as its detail view shows it. */
export interface MarketAsset {
  /** "AUG1". */
  ticker: string;
  /** "Tokenized gold, 1 g". */
  name: string;
  /** What it is, in a few words: "Commodity", "Government bond", "Real estate fund". */
  asset_class: string;
  /** Last traded price, MXN per unit. */
  price: DecimalString;
  /** Change over the last 24 hours, percent. */
  change_24h_pct: DecimalString;
  /** MXN; null where it does not apply (a commodity has no market cap). */
  market_cap: DecimalString | null;
  /** MXN traded over the last 24 hours; null when unknown. */
  volume_24h: DecimalString | null;
  /** Annual yield, percent; null when the asset pays none (gold). */
  yield_pct: DecimalString | null;
  /** Decimal places a quantity may have: fractional units (4) or whole units (0). */
  quantity_decimals: number;
}

export type OrderSide = "buy" | "sell";

/** A market order by amount: buy (or sell) this many pesos' worth. */
export interface OrderRequest {
  ticker: string;
  side: OrderSide;
  /** MXN, at most 2 decimals. */
  amount: DecimalString;
  /** One per order and reused on a retry, like every money movement in this API. */
  idempotency_key: string;
}

/** What the backend answers for a placed order. */
export interface OrderReceipt {
  order_id: string;
  /** "filled" at once, or "pending" until the market fills it. */
  status: "filled" | "pending";
  /** Units bought or sold. */
  quantity: DecimalString;
  /** Average price per unit, MXN. */
  price: DecimalString;
  /** MXN. */
  amount: DecimalString;
  /** MXN: always "0.00". */
  commission: DecimalString;
}
