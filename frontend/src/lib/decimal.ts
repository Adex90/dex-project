/**
 * Exact decimal arithmetic for money, mirroring the backend's NUMERIC(18, 8).
 *
 * Amounts travel as strings ("1500.00000000") and are handled as BigInt
 * units of 10^-8. Floating point is never used for money.
 */

export const SCALE = 8;
const UNIT = 10n ** BigInt(SCALE);
/** Largest amount representable in NUMERIC(18, 8): 9 999 999 999.99999999 */
export const MAX_UNITS = 999_999_999_999_999_999n;

const DECIMAL_RE = /^(\d+)(?:\.(\d+))?$/;

/** Parses a decimal string (as returned by the API) into units of 10^-8. */
export function toUnits(value: string): bigint {
  const trimmed = value.trim();
  const negative = trimmed.startsWith("-");
  const match = DECIMAL_RE.exec(negative ? trimmed.slice(1) : trimmed);
  if (!match) throw new Error(`invalid decimal: ${value}`);
  const [, int = "0", frac = ""] = match;
  if (frac.length > SCALE) throw new Error(`more than ${SCALE} decimals: ${value}`);
  const units = BigInt(int) * UNIT + BigInt(frac.padEnd(SCALE, "0"));
  return negative ? -units : units;
}

/** Units of 10^-8 back to a canonical 8-decimal string (same as NUMERIC(18,8)::text). */
export function fromUnits(units: bigint): string {
  const negative = units < 0n;
  const abs = negative ? -units : units;
  const int = abs / UNIT;
  const frac = (abs % UNIT).toString().padStart(SCALE, "0");
  return `${negative ? "-" : ""}${int}.${frac}`;
}

/** Integer division rounding half to even (banker's rounding), like the backend. */
export function divRoundHalfEven(numerator: bigint, denominator: bigint): bigint {
  if (denominator <= 0n) throw new Error("denominator must be positive");
  const negative = numerator < 0n;
  const n = negative ? -numerator : numerator;
  const q = n / denominator;
  const r = n % denominator;
  const twice = r * 2n;
  let rounded = q;
  if (twice > denominator || (twice === denominator && q % 2n === 1n)) rounded = q + 1n;
  return negative ? -rounded : rounded;
}

export type AmountParse = { ok: true; value: string; units: bigint } | { ok: false; error: string };

/** Validates user input for an amount: positive, at most 8 decimals, within range. */
export function parseAmountInput(raw: string): AmountParse {
  const cleaned = raw.replace(/[\s,$]/g, "");
  if (cleaned === "") return { ok: false, error: "Enter an amount." };
  if (!DECIMAL_RE.test(cleaned)) return { ok: false, error: "Use digits and an optional decimal point." };
  let units: bigint;
  try {
    units = toUnits(cleaned);
  } catch {
    return { ok: false, error: `At most ${SCALE} decimal places.` };
  }
  if (units <= 0n) return { ok: false, error: "The amount must be greater than zero." };
  if (units > MAX_UNITS) return { ok: false, error: "The amount exceeds the maximum allowed." };
  return { ok: true, value: fromUnits(units), units };
}

const grouping = new Intl.NumberFormat("en-US");

/**
 * Formats an amount for display, rounding to `decimals` places (half-even)
 * from the exact value. Thousands separators come from BigInt formatting,
 * so there is no precision loss for large balances.
 */
export function formatAmount(value: string | bigint, decimals = 2): string {
  const units = typeof value === "bigint" ? value : toUnits(value);
  const factor = 10n ** BigInt(SCALE - decimals);
  const rounded = divRoundHalfEven(units, factor);
  const negative = rounded < 0n;
  const abs = negative ? -rounded : rounded;
  const base = 10n ** BigInt(decimals);
  const int = grouping.format(abs / base);
  const frac = decimals > 0 ? `.${(abs % base).toString().padStart(decimals, "0")}` : "";
  return `${negative ? "-" : ""}${int}${frac}`;
}

/** "$1,234.56" — the currency (MXN) is shown separately by the UI. */
export function formatMoney(value: string | bigint, decimals = 2): string {
  const s = formatAmount(value, decimals);
  return s.startsWith("-") ? `-$${s.slice(1)}` : `$${s}`;
}

/** Annual rate "0.072500" -> "7.25%". */
export function formatRate(rate: string): string {
  const units = toUnits(rate); // 10^-8
  const bps = divRoundHalfEven(units * 100n, 10n ** 6n); // percent with 2 decimals
  const int = bps / 100n;
  const frac = (bps % 100n).toString().padStart(2, "0");
  return `${int}.${frac}%`;
}

/**
 * Simple ACT/360 interest at maturity, rounded to the cent with banker's
 * rounding — the exact formula the backend uses (sofom::tasas::interes).
 */
export function interestAct360(principal: string, annualRate: string, days: number): string {
  const p = toUnits(principal); // 10^-8
  const r = toUnits(annualRate); // 10^-8
  // p * r * days / 360, scaled 10^-16 -> cents (10^-2) means dividing by 10^14.
  const cents = divRoundHalfEven(p * r * BigInt(days), 360n * 10n ** 14n);
  return fromUnits(cents * 10n ** 6n);
}

export function addUnits(...values: string[]): string {
  return fromUnits(values.reduce((acc, v) => acc + toUnits(v), 0n));
}

export function compareAmounts(a: string, b: string): number {
  const d = toUnits(a) - toUnits(b);
  return d === 0n ? 0 : d > 0n ? 1 : -1;
}

export function isZero(value: string): boolean {
  return toUnits(value) === 0n;
}
