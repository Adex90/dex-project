/**
 * Order entry: the amount a numpad or a keyboard builds, what it buys, and
 * whether it can be placed.
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

import { SCALE, fromUnits, toUnits } from "./decimal.ts";
import type { DecimalString } from "../types/vault.ts";

/** Pesos have cents: at most two decimals in an amount. */
export const AMOUNT_DECIMALS = 2;
/** Integer digits an amount may have: up to 999,999,999. */
export const MAX_AMOUNT_DIGITS = 9;

export type NumpadKey = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "." | "back";

/** The numpad, row by row: 1 2 3 / 4 5 6 / 7 8 9 / . 0 back. */
export const NUMPAD_KEYS: readonly NumpadKey[] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", ".", "0", "back"];

/**
 * The amount after one numpad key. A key that would make it invalid (a
 * second point, a third decimal, a tenth integer digit) changes nothing.
 * Leading zeros never pile up: "0" then "5" is "5".
 */
export function pressKey(amount: string, key: NumpadKey): string {
  if (key === "back") return amount.slice(0, -1);
  if (key === ".") return amount.includes(".") ? amount : `${amount === "" ? "0" : amount}.`;
  const [integer = "", fraction] = amount.split(".");
  if (fraction !== undefined) return fraction.length >= AMOUNT_DECIMALS ? amount : amount + key;
  if (integer === "0") return key;
  if (integer.length >= MAX_AMOUNT_DIGITS) return amount;
  return amount + key;
}

/**
 * Typed or pasted text as an amount ("$1,250.50" gives "1250.50"), or null
 * when it cannot be one: other characters, a second point, more than two
 * decimals or nine integer digits. The caller then keeps what it had.
 */
export function sanitizeAmount(raw: string): string | null {
  const text = raw.replace(/[\s,$]/g, "");
  if (!/^\d*(\.\d*)?$/.test(text)) return null;
  const [integerPart = "", fraction] = text.split(".");
  if (fraction !== undefined && fraction.length > AMOUNT_DECIMALS) return null;
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

/**
 * The most that can be entered: `available` rounded down to the cent, in the
 * entry format ("15000.12345678" gives "15000.12"; "15000" stays "15000").
 */
export function maxAmount(available: DecimalString): string {
  const units = toUnits(available);
  if (units <= 0n) return "";
  const step = 10n ** BigInt(SCALE - AMOUNT_DECIMALS);
  const cents = units / step;
  const integer = (cents / 100n).toString();
  const fraction = (cents % 100n).toString().padStart(2, "0").replace(/0+$/, "");
  return fraction === "" ? integer : `${integer}.${fraction}`;
}

/**
 * Units an amount buys at `price`, rounded down to `decimals` places so an
 * order never asks for more than its money covers: 1,250 at 1,612.40 with 4
 * decimals is "0.7752". Null for a price that is not positive.
 */
export function estimateQuantity(units: bigint, price: DecimalString, decimals: number): DecimalString | null {
  const priceUnits = toUnits(price);
  if (priceUnits <= 0n || units < 0n) return null;
  const scaled = (units * 10n ** BigInt(decimals)) / priceUnits;
  if (decimals === 0) return scaled.toString();
  const base = 10n ** BigInt(decimals);
  return `${scaled / base}.${(scaled % base).toString().padStart(decimals, "0")}`;
}

/** What a position is worth at `price`, exact to 10^-8 (rounded down). */
export function positionValue(quantity: DecimalString, price: DecimalString): DecimalString {
  return fromUnits((toUnits(quantity) * toUnits(price)) / 10n ** BigInt(SCALE));
}

export type OrderProblem = "empty" | "exceeds";

/**
 * Whether an amount can be placed against what is `available` (cash for a
 * buy, the position's value for a sale): null when it can, "empty" for
 * nothing entered yet, "exceeds" for more than is there.
 */
export function orderProblem(amount: string, available: DecimalString): OrderProblem | null {
  const units = amountUnits(amount);
  if (units === 0n) return "empty";
  return units > toUnits(available) ? "exceeds" : null;
}
