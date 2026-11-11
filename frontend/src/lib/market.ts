/**
 * Order entry: the amount (and, for limit and stop orders, the price) a
 * numpad or a keyboard builds, what it buys, whether it can be placed, and
 * the request it becomes (types/market.ts).
 *
 * Pure functions with no React and no DOM: `node --test` runs them directly
 * (hence the relative `.ts` imports, which Node resolves without the
 * bundler's `@/` alias).
 *
 * An amount is a peso string with at most 2 decimals, built a key at a time
 * ("", "1", "12", "12.", "12.5"). It is parsed exactly (BigInt units of 10^-8,
 * lib/decimal) only to compute and to validate; money never touches floating
 * point.
 */

import { SCALE, formatMoney, fromUnits, toUnits } from "./decimal.ts";
import type { OrderRequest, OrderSide, OrderType, PlanFrequency } from "../types/market.ts";
import type { DecimalString } from "../types/vault.ts";

/** Pesos have cents: at most two decimals in an amount. */
export const AMOUNT_DECIMALS = 2;
/** Limit and stop prices: at most four decimals, as quotes have (CETES at 9.9439). */
export const PRICE_DECIMALS = 4;
/** Integer digits an amount may have: up to 999,999,999. */
export const MAX_AMOUNT_DIGITS = 9;

export type NumpadKey = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "." | "back";

/** The numpad, row by row: 1 2 3 / 4 5 6 / 7 8 9 / . 0 back. */
export const NUMPAD_KEYS: readonly NumpadKey[] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", ".", "0", "back"];

/**
 * The amount after one numpad key. A key that would make it invalid (a
 * second point, one decimal too many, a tenth integer digit) changes nothing.
 * Leading zeros never pile up: "0" then "5" is "5". `decimals` is 2 for
 * amounts, `PRICE_DECIMALS` for prices.
 */
export function pressKey(amount: string, key: NumpadKey, decimals = AMOUNT_DECIMALS): string {
  if (key === "back") return amount.slice(0, -1);
  if (key === ".") return amount.includes(".") ? amount : `${amount === "" ? "0" : amount}.`;
  const [integer = "", fraction] = amount.split(".");
  if (fraction !== undefined) return fraction.length >= decimals ? amount : amount + key;
  if (integer === "0") return key;
  if (integer.length >= MAX_AMOUNT_DIGITS) return amount;
  return amount + key;
}

/**
 * Typed or pasted text as an amount ("$1,250.50" gives "1250.50"), or null
 * when it cannot be one: other characters, a second point, more than
 * `decimals` decimals or nine integer digits. The caller then keeps what it
 * had.
 */
export function sanitizeAmount(raw: string, decimals = AMOUNT_DECIMALS): string | null {
  const text = raw.replace(/[\s,$]/g, "");
  if (!/^\d*(\.\d*)?$/.test(text)) return null;
  const [integerPart = "", fraction] = text.split(".");
  if (fraction !== undefined && fraction.length > decimals) return null;
  const integer = integerPart.replace(/^0+(?=\d)/, "");
  if (integer.length > MAX_AMOUNT_DIGITS) return null;
  if (fraction === undefined) return integer;
  return `${integer === "" ? "0" : integer}.${fraction}`;
}

/** The amount as typed, with thousands separators: "1250.5" gives "1,250.5"; "" gives "0". */
export function formatAmountInput(amount: string): string {
  if (amount === "") return "0";
  const [integer = "", fraction] = amount.split(".");
  const grouped = (integer === "" ? "0" : integer).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return fraction === undefined ? grouped : `${grouped}.${fraction}`;
}

/** The amount in units of 10^-8: "" and "0." are 0. */
export function amountUnits(amount: string): bigint {
  const digits = amount.endsWith(".") ? amount.slice(0, -1) : amount;
  return digits === "" ? 0n : toUnits(digits);
}

/** The amount as the API takes it (2 decimals): "12.5" gives "12.50". */
export function amountForApi(amount: string): DecimalString {
  const units = amountUnits(amount);
  const cents = units / 10n ** BigInt(SCALE - AMOUNT_DECIMALS);
  return `${cents / 100n}.${(cents % 100n).toString().padStart(2, "0")}`;
}

/** A limit or stop price as the API takes it, 2 to 4 decimals: "1580" gives "1580.00", "9.9439" stays. */
export function priceForApi(price: string): DecimalString {
  const steps = amountUnits(price) / 10n ** BigInt(SCALE - PRICE_DECIMALS);
  const base = 10n ** BigInt(PRICE_DECIMALS);
  let fraction = (steps % base).toString().padStart(PRICE_DECIMALS, "0");
  while (fraction.length > AMOUNT_DECIMALS && fraction.endsWith("0")) fraction = fraction.slice(0, -1);
  return `${steps / base}.${fraction}`;
}

/**
 * The most that can be entered: `available` rounded down to the cent (or to
 * `decimals` places), in the entry format ("15000.12345678" gives "15000.12";
 * "15000" stays "15000"); "" when there is nothing.
 */
export function maxAmount(available: DecimalString, decimals = AMOUNT_DECIMALS): string {
  const units = toUnits(available);
  if (units <= 0n) return "";
  const steps = units / 10n ** BigInt(SCALE - decimals);
  const base = 10n ** BigInt(decimals);
  const integer = (steps / base).toString();
  const fraction = decimals === 0 ? "" : (steps % base).toString().padStart(decimals, "0").replace(/0+$/, "");
  return fraction === "" ? integer : `${integer}.${fraction}`;
}

/** A price in the entry format, as "Use current" fills it: "1612.40000000" gives "1612.4". */
export function priceEntry(price: DecimalString): string {
  return maxAmount(price, PRICE_DECIMALS);
}

/** A price for display: to the cent, or to four decimals when it has them ("$9.9439"). */
export function formatPrice(price: DecimalString): string {
  const cents = toUnits(price) % 10n ** BigInt(SCALE - AMOUNT_DECIMALS) === 0n;
  return formatMoney(price, cents ? AMOUNT_DECIMALS : PRICE_DECIMALS);
}

/**
 * Units an amount buys at `price`, rounded down to `decimals` places so an
 * order never asks for more than its money covers: 1,250 at 1,612.40 with 4
 * decimals is "0.7752". Null for a price that is not positive.
 */
export function estimateQuantity(units: bigint, price: DecimalString, decimals: number): DecimalString | null {
  const priceUnits = toUnits(price);
  if (priceUnits <= 0n || units < 0n) return null;
  return increments((units * 10n ** BigInt(decimals)) / priceUnits, decimals);
}

/**
 * Units a sale takes from a holding of `held` at `price`, as the backend
 * fills it (backend/src/mercado/calculo.rs). An amount that reaches the
 * holding's value rounded down to the cent (what "Max" enters) sells all of
 * it, leaving no residue: 2,499.86 of 1.5504 at 1,612.40 sells "1.5504",
 * where the amount alone would cover 1.5503. Any other amount sells what it
 * covers, rounded down like a purchase. Null for a price that is not positive.
 */
export function estimateSale(
  units: bigint,
  held: DecimalString,
  price: DecimalString,
  decimals: number,
): DecimalString | null {
  if (toUnits(price) <= 0n) return null;
  const heldUnits = toUnits(held);
  const worth = toUnits(positionValue(held, price));
  const cent = 10n ** BigInt(SCALE - AMOUNT_DECIMALS);
  if (heldUnits > 0n && units > 0n && units >= worth - (worth % cent)) {
    return increments(heldUnits / 10n ** BigInt(SCALE - decimals), decimals);
  }
  return estimateQuantity(units, price, decimals);
}

/** The asset's smallest quantity: "0.0001" with 4 decimals, "1" with none. */
export function smallestUnit(decimals: number): DecimalString {
  return increments(1n, decimals);
}

/** `count` increments of 10^-decimals, written with `decimals` places. */
function increments(count: bigint, decimals: number): DecimalString {
  if (decimals === 0) return count.toString();
  const base = 10n ** BigInt(decimals);
  return `${count / base}.${(count % base).toString().padStart(decimals, "0")}`;
}

/** What a position is worth at `price`, exact to 10^-8 (rounded down). */
export function positionValue(quantity: DecimalString, price: DecimalString): DecimalString {
  return fromUnits((toUnits(quantity) * toUnits(price)) / 10n ** BigInt(SCALE));
}

/** Why an amount cannot be placed: nothing entered, more than there is, or too little to buy one unit. */
export type OrderProblem = "empty" | "exceeds" | "too_small";

/**
 * Whether an amount can be placed against what is `available` (cash for a
 * buy, the position's value for a sale): null when it can, "empty" for
 * nothing entered yet, "exceeds" for more than is there. `assessOrder` adds
 * "too_small".
 */
export function orderProblem(amount: string, available: DecimalString): OrderProblem | null {
  const units = amountUnits(amount);
  if (units === 0n) return "empty";
  return units > toUnits(available) ? "exceeds" : null;
}

/* ------------------------------------------------------------------------ */
/* Order types                                                              */
/* ------------------------------------------------------------------------ */

/** The types a side offers, in the checkout's order: a savings plan only buys. */
export function orderTypesFor(side: OrderSide): readonly OrderType[] {
  return side === "buy" ? ["market", "limit", "stop", "savings_plan"] : ["market", "limit", "stop"];
}

/**
 * What the backend executes: market orders, and limit and stop orders, which
 * wait for their price with their cash or units held (migration 0015). The
 * checkout lets a client set up a savings plan and see what it would do, but
 * schedules none until the backend does (roadmap module 4).
 */
export const LIVE_ORDER_TYPES: readonly OrderType[] = ["market", "limit", "stop"];

export const PLAN_FREQUENCIES: readonly PlanFrequency[] = ["weekly", "biweekly", "monthly"];

/** Purchases a year: 52 weekly, 26 every two weeks, 12 monthly. */
const PURCHASES_A_YEAR: Record<PlanFrequency, bigint> = { weekly: 52n, biweekly: 26n, monthly: 12n };

/** What a savings plan invests in an average month, to the cent (half up): 500 weekly is "2166.67". */
export function monthlyEquivalent(amount: string, frequency: PlanFrequency): DecimalString {
  const cents = amountUnits(amount) / 10n ** BigInt(SCALE - AMOUNT_DECIMALS);
  // cents × purchases a year / 12, rounded half up: floor((2x + 12) / 24).
  const monthly = (2n * cents * PURCHASES_A_YEAR[frequency] + 12n) / 24n;
  return `${monthly / 100n}.${(monthly % 100n).toString().padStart(2, "0")}`;
}

export type PriceProblem = "empty" | "wrong_side";

/**
 * Whether a limit or stop price can be used: "empty" while nothing (or zero)
 * is entered, "wrong_side" for a stop the price has already reached, which
 * would trigger at once (to buy, a stop must be above the current price; to
 * sell, below it). A limit the price already meets is allowed: it fills at
 * once, at that price or better (see `marketable`).
 */
export function priceProblem(
  type: "limit" | "stop",
  side: OrderSide,
  price: string,
  current: DecimalString,
): PriceProblem | null {
  const units = amountUnits(price);
  if (units === 0n) return "empty";
  if (type === "stop") {
    const now = toUnits(current);
    if (side === "buy" ? units <= now : units >= now) return "wrong_side";
  }
  return null;
}

/** What is entered in the checkout, as typed. */
export interface OrderDraft {
  type: OrderType;
  side: OrderSide;
  /** Pesos ("1250.5"). */
  amount: string;
  /** The limit or stop price; ignored by the other types. */
  price: string;
  /** How often a savings plan buys; ignored by the other types. */
  frequency: PlanFrequency;
}

/** What the checkout knows about the asset and the vault. */
export interface OrderContext {
  /** The asset's price now. */
  current: DecimalString;
  /** Decimal places of the asset's quantity increment. */
  decimals: number;
  /** The vault's available cash. */
  cash: DecimalString;
  /** Units held; "0" for none. */
  held: DecimalString;
}

export interface OrderAssessment {
  /** The price the estimate uses: the current one (market, plan) or the order's own (limit, stop); null while it is missing. */
  price: DecimalString | null;
  /** What the amount can draw on: cash to buy, the position's value at `price` to sell; null for a plan, charged at each purchase. */
  available: DecimalString | null;
  /** The most that can be entered (`maxAmount`); "" when there is no ceiling to offer. */
  max: string;
  /** Units the amount buys (or sells) at `price`; null while `price` is missing. */
  quantity: DecimalString | null;
  amountProblem: OrderProblem | null;
  priceProblem: PriceProblem | null;
  /** A limit the current price already meets: it would fill at once, at that price or better. */
  marketable: boolean;
  /** Nothing stops it: it can be placed (if its type is live). */
  ready: boolean;
}

/**
 * Everything the checkout shows about an order before it is placed, the same
 * rules for every type: an amount that buys nothing ("too_small") or exceeds
 * what it draws on cannot be placed; a sale that reaches the position's whole
 * value at its price sells all of it (`estimateSale`).
 */
export function assessOrder(draft: OrderDraft, ctx: OrderContext): OrderAssessment {
  const priced = draft.type === "limit" || draft.type === "stop";
  const priceIssue =
    draft.type === "limit" || draft.type === "stop"
      ? priceProblem(draft.type, draft.side, draft.price, ctx.current)
      : null;
  const price = !priced ? ctx.current : priceIssue === "empty" ? null : fromUnits(amountUnits(draft.price));
  const plan = draft.type === "savings_plan";
  const selling = draft.side === "sell" && !plan;
  const available = plan ? null : selling ? positionValue(ctx.held, price ?? ctx.current) : ctx.cash;
  const units = amountUnits(draft.amount);
  const quantity =
    price === null
      ? null
      : selling
        ? estimateSale(units, ctx.held, price, ctx.decimals)
        : estimateQuantity(units, price, ctx.decimals);
  let amountProblem: OrderProblem | null =
    available === null ? (units === 0n ? "empty" : null) : orderProblem(draft.amount, available);
  if (amountProblem === null && quantity !== null && toUnits(quantity) === 0n) amountProblem = "too_small";
  const now = toUnits(ctx.current);
  const marketable =
    draft.type === "limit" &&
    price !== null &&
    (draft.side === "buy" ? toUnits(price) >= now : toUnits(price) <= now);
  return {
    price,
    available,
    max: available === null ? "" : maxAmount(available),
    quantity,
    amountProblem,
    priceProblem: priceIssue,
    marketable,
    ready: amountProblem === null && priceIssue === null,
  };
}

/** The request for `draft` (types/market.ts), amounts and prices in the API's format. */
export function buildOrder(draft: OrderDraft, ticker: string, idempotencyKey: string): OrderRequest {
  const base = { ticker, amount: amountForApi(draft.amount), idempotency_key: idempotencyKey };
  switch (draft.type) {
    case "market":
      return { ...base, order_type: "market", side: draft.side };
    case "limit":
      return { ...base, order_type: "limit", side: draft.side, limit_price: priceForApi(draft.price) };
    case "stop":
      return { ...base, order_type: "stop", side: draft.side, stop_price: priceForApi(draft.price) };
    case "savings_plan":
      return { ...base, order_type: "savings_plan", side: "buy", frequency: draft.frequency };
  }
}

/**
 * The order without its key: the checkout keeps one idempotency key while
 * this stays the same (a retry), and draws a new one when anything changes.
 */
export function draftSignature(draft: OrderDraft): string {
  return JSON.stringify(buildOrder(draft, "", ""));
}
