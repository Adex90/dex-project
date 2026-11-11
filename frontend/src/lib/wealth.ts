/**
 * Wealth and banking previews for the Account tab: the Sovereign Yield
 * counter (components/wealth/cash-yield-section.tsx) and the card's saveback
 * tracker (components/wealth/card-management.tsx). Neither product is live:
 * there is no yield payout and no card issuer yet, so these figures are
 * simulations, and the screens say so.
 *
 * - Licensing: the Ley Fintech bars an IFPE from paying interest on e-money
 *   balances (docs/FRONTEND_ROADMAP.md, "Licensing scope"). Paying this
 *   yield needs a structure the licence allows before anything is paid.
 * - The month is the bank's: Mexico City time, UTC−6 all year (no daylight
 *   saving since 2022), as `todayInMexicoCity` in lib/kyc.ts.
 * - Exact: money in BigInt units of 10^-8 (lib/decimal.ts), rounded down,
 *   so a counter never shows more than the simulation has accrued. Interest
 *   counts actual time over a 360-day year (ACT/360), the convention of the
 *   SOFOM's contracts (`interestAct360`).
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { SCALE, toUnits } from "./decimal.ts";
import type { DecimalString } from "../types/vault.ts";

const DAY_MS = 86_400_000;
/** Mexico City is UTC−6 all year. */
const MEXICO_CITY_OFFSET_MS = 6 * 3_600_000;
const UNIT = 10n ** BigInt(SCALE);

/** The mocked Sovereign Yield, percent a year: a preview until a licensed product pays it. */
export const SOVEREIGN_YIELD_PCT: DecimalString = "7.25";
/** Days in the interest year (ACT/360). */
export const YIELD_BASIS_DAYS = 360;

export interface MonthWindow {
  /** The first instant of the month in Mexico City, epoch ms. */
  start: number;
  /** The first instant of the next month, epoch ms. */
  end: number;
  /** Today's day of the month in Mexico City, from 1. */
  day: number;
  /** Days in the month. */
  days: number;
}

/** The Mexico City calendar month that holds `now` (epoch ms). */
export function monthWindow(now: number): MonthWindow {
  const local = new Date(now - MEXICO_CITY_OFFSET_MS);
  const year = local.getUTCFullYear();
  const month = local.getUTCMonth();
  const start = Date.UTC(year, month, 1) + MEXICO_CITY_OFFSET_MS;
  const end = Date.UTC(year, month + 1, 1) + MEXICO_CITY_OFFSET_MS;
  return { start, end, day: local.getUTCDate(), days: Math.round((end - start) / DAY_MS) };
}

/**
 * Interest on `balance` at `ratePct` a year over `elapsedMs` (ACT/360), in
 * units of 10^-8, rounded down. Nothing accrues on a balance or a time that
 * is not positive.
 */
export function accrued(balance: DecimalString, ratePct: DecimalString, elapsedMs: number): bigint {
  const units = toUnits(balance);
  const rate = toUnits(ratePct);
  const ms = BigInt(Math.max(0, Math.floor(elapsedMs)));
  if (units <= 0n || rate <= 0n || ms === 0n) return 0n;
  // units × (rate / 100) × ms / (360 days in ms); the rate carries 10^-8 too.
  return (units * rate * ms) / (100n * UNIT * BigInt(YIELD_BASIS_DAYS) * BigInt(DAY_MS));
}

export interface YieldPreview {
  /** Accrued since the 1st on today's balance, units of 10^-8. */
  earned: bigint;
  /** What a whole day earns, units of 10^-8. */
  perDay: bigint;
  /** What the whole month would earn at this balance, units of 10^-8. */
  projected: bigint;
}

/** The counter's figures at instant `now`, as if `balance` had stood since the 1st. */
export function yieldPreview(
  balance: DecimalString,
  now: number,
  ratePct: DecimalString = SOVEREIGN_YIELD_PCT,
): YieldPreview {
  const month = monthWindow(now);
  return {
    earned: accrued(balance, ratePct, now - month.start),
    perDay: accrued(balance, ratePct, DAY_MS),
    projected: accrued(balance, ratePct, month.end - month.start),
  };
}

/**
 * How many decimals the counter shows: four at least, more while the last
 * one would otherwise sit still for longer than a tenth of a second, eight at
 * most (the exact unit). $1,000,000 ticks at four, $10,000 at six, $100 at eight.
 */
export function counterDecimals(balance: DecimalString, ratePct: DecimalString = SOVEREIGN_YIELD_PCT): number {
  const perTenth = (Number(balance) * (Number(ratePct) / 100)) / (YIELD_BASIS_DAYS * 864_000);
  if (!(perTenth > 0)) return 4;
  return Math.min(8, Math.max(4, Math.ceil(-Math.log10(perTenth))));
}

const grouping = new Intl.NumberFormat("en-US");

/** Units of 10^-8 as a peso amount with `decimals` places, rounded down: "12.345678", "1,234.5678". */
export function formatTruncated(units: bigint, decimals: number): string {
  const negative = units < 0n;
  const abs = negative ? -units : units;
  const kept = abs / 10n ** BigInt(SCALE - decimals);
  const base = 10n ** BigInt(decimals);
  const fraction = decimals > 0 ? `.${(kept % base).toString().padStart(decimals, "0")}` : "";
  return `${negative ? "-" : ""}${grouping.format(kept / base)}${fraction}`;
}

/** A share in basis points (10,000 is all of it), rounded down; 0 for a whole that is not positive. */
export function shareBasisPoints(part: bigint, whole: bigint): number {
  if (whole <= 0n || part <= 0n) return 0;
  return Number(part >= whole ? 10_000n : (part * 10_000n) / whole);
}

/* ------------------------------------------------------------------------ */
/* Saveback                                                                 */
/* ------------------------------------------------------------------------ */

/** The card's mocked saveback: a share of each card payment, up to a monthly cap. */
export const SAVEBACK = { ratePct: "1" as DecimalString, monthlyCap: "300.00" as DecimalString };

export interface SavebackPreview {
  /** Simulated card spending this month through today, units of 10^-8. */
  spent: bigint;
  /** Saveback on it, capped, rounded down to the cent, units of 10^-8. */
  earned: bigint;
  cap: bigint;
  /** `earned` of `cap`, in basis points. */
  progress: number;
}

/**
 * The saveback tracker's figures: card spending simulated for each day of
 * the month through today (a whole-peso amount from 220 to 899, seeded with
 * `seed` and the date, so it is the same on every visit), and its saveback.
 */
export function savebackPreview(now: number, seed: string): SavebackPreview {
  const month = monthWindow(now);
  const local = new Date(month.start - MEXICO_CITY_OFFSET_MS + DAY_MS / 2);
  let spent = 0n;
  for (let day = 1; day <= month.day; day++) {
    const key = `${seed}:${local.getUTCFullYear()}-${local.getUTCMonth() + 1}-${day}`;
    spent += BigInt(220 + (hash(key) % 680)) * UNIT;
  }
  return saveback(spent);
}

/** Saveback on `spent` (units of 10^-8): the rate's share, rounded down to the cent, never above the cap. */
export function saveback(
  spent: bigint,
  ratePct: DecimalString = SAVEBACK.ratePct,
  monthlyCap: DecimalString = SAVEBACK.monthlyCap,
): SavebackPreview {
  const cent = 10n ** BigInt(SCALE - 2);
  const raw = spent > 0n ? (spent * toUnits(ratePct)) / (100n * UNIT) : 0n;
  const cap = toUnits(monthlyCap);
  const earned = raw >= cap ? cap : raw - (raw % cent);
  return { spent, earned, cap, progress: shareBasisPoints(earned, cap) };
}

/** FNV-1a, 32 bits: a stable draw for the simulated spending. */
function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/* ------------------------------------------------------------------------ */
/* Card                                                                     */
/* ------------------------------------------------------------------------ */

/**
 * The name as a card carries it: first name and first surname in capitals,
 * without accents, at most 26 characters (the embossing limit). "José Ñúñez"
 * is "JOSE NUNEZ".
 */
export function cardholderName(client: { nombre: string; apellido_paterno: string | null }): string {
  return [client.nombre, client.apellido_paterno ?? ""]
    .join(" ")
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toUpperCase()
    .replace(/[^A-Z0-9 .'-]/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 26)
    .trim();
}
