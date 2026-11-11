/**
 * Portfolio chart data and geometry: the balance history rebuilt from the
 * IFPE ledger, the time ranges, the change over a range, and the scales the
 * chart draws with.
 *
 * Pure functions with no React and no DOM: `node --test` runs them directly
 * (hence the relative `.ts` imports, which Node resolves without the
 * bundler's `@/` alias). Money stays exact (BigInt units of 10^-8); only
 * pixel positions and axis labels use floating point.
 */

import type { TransaccionIfpe } from "./api/types.ts";
import { SCALE, divRoundHalfEven, fromUnits, toUnits } from "./decimal.ts";
import type { DecimalString } from "../types/vault.ts";

/** A balance that holds from `at` (epoch milliseconds) until the next point. */
export interface BalancePoint {
  at: number;
  value: DecimalString;
}

/* ------------------------------------------------------------------------ */
/* History                                                                  */
/* ------------------------------------------------------------------------ */

type LedgerEntry = Pick<TransaccionIfpe, "secuencia" | "tipo" | "monto" | "cuenta_origen_id" | "creado_en">;

/**
 * How a ledger movement changed the account's cash (available plus held),
 * in units of 10^-8. A hold, or its release, moves money between available
 * and held inside the account, so it changes nothing here.
 */
export function cashDelta(tx: Pick<LedgerEntry, "tipo" | "monto" | "cuenta_origen_id">, accountId: string): bigint {
  const amount = toUnits(tx.monto);
  switch (tx.tipo) {
    case "DEPOSITO":
    case "RETORNO_DE_SOFOM":
    case "VENTA_MERCADO":
      return amount;
    case "RETIRO":
    case "TRASPASO_A_SOFOM":
    case "COMPRA_MERCADO":
      return -amount;
    case "TRANSFERENCIA":
      return tx.cuenta_origen_id === accountId ? -amount : amount;
    case "RETENCION":
    case "LIBERACION_RETENCION":
    case "COMPROMISO_ORDEN":
    case "LIBERACION_ORDEN":
      return 0n;
    default:
      // A kind this build does not know yet: leave the line flat rather than guess.
      return 0n;
  }
}

/**
 * The account's cash balance over time, oldest first: the balance before the
 * oldest movement given, the balance after each movement, and `current` at
 * `now`. Rebuilt backwards from `current` (the balance read together with
 * these movements), so every point is exact however much history is loaded:
 * a longer history only adds older points.
 *
 * `movements` may come in any order (they are ordered by ledger sequence).
 * `now` is clamped to the newest movement, so a client clock behind the
 * server's never draws the line backwards.
 */
export function cashBalanceHistory(
  movements: readonly LedgerEntry[],
  accountId: string,
  current: DecimalString,
  now: number,
): BalancePoint[] {
  const newestFirst = [...movements].sort((a, b) => b.secuencia - a.secuencia);
  let balance = toUnits(current);
  const newestAt = newestFirst.length > 0 ? Date.parse(newestFirst[0]!.creado_en) : now;
  const points: BalancePoint[] = [{ at: Math.max(now, newestAt), value: fromUnits(balance) }];
  for (const tx of newestFirst) {
    points.push({ at: Date.parse(tx.creado_en), value: fromUnits(balance) });
    balance -= cashDelta(tx, accountId);
  }
  const oldest = newestFirst.at(-1);
  if (oldest) points.push({ at: Date.parse(oldest.creado_en), value: fromUnits(balance) });
  return points.reverse();
}

/* ------------------------------------------------------------------------ */
/* Ranges                                                                   */
/* ------------------------------------------------------------------------ */

export const CHART_RANGES = [
  { id: "1D", label: "1D", name: "the last day", days: 1 },
  { id: "1W", label: "1W", name: "the last week", days: 7 },
  { id: "1M", label: "1M", name: "the last month", days: 30 },
  { id: "3M", label: "3M", name: "the last 3 months", days: 90 },
  { id: "1Y", label: "1Y", name: "the last year", days: 365 },
  { id: "ALL", label: "All", name: "all loaded history", days: null },
] as const;

export type ChartRangeId = (typeof CHART_RANGES)[number]["id"];

const DAY_MS = 86_400_000;

/**
 * The points a range shows: those inside it, preceded by the balance in
 * force when it starts (the last point at or before its start), so the line
 * spans the whole range. A history that begins inside the range starts at its
 * first point; "ALL" is everything.
 */
export function pointsInRange(points: readonly BalancePoint[], range: ChartRangeId, now: number): BalancePoint[] {
  const days = rangeDays(range);
  if (days === null || points.length === 0) return [...points];
  return pointsSince(points, now - days * DAY_MS, now);
}

/** How many days a range spans; null for "ALL". */
export function rangeDays(range: ChartRangeId): number | null {
  return CHART_RANGES.find((r) => r.id === range)?.days ?? null;
}

/**
 * The points from `start` to `now`: those after `start`, preceded by the one
 * in force at `start` (the last at or before it), so the line spans the whole
 * window. A history that begins after `start` starts at its first point.
 */
export function pointsSince(points: readonly BalancePoint[], start: number, now: number): BalancePoint[] {
  if (points.length === 0) return [];
  const firstInside = points.findIndex((p) => p.at > start);
  if (firstInside === -1) {
    // Nothing moved inside the range: the last balance held all along.
    const { value } = points.at(-1)!;
    return [
      { at: start, value },
      { at: now, value },
    ];
  }
  if (firstInside === 0) return [...points];
  return [{ at: start, value: points[firstInside - 1]!.value }, ...points.slice(firstInside)];
}

/**
 * The points to draw: a single point (a balance nothing has moved yet) gets a
 * twin one millisecond earlier, so it draws as a flat line across the plot.
 */
export function drawable(points: readonly BalancePoint[]): BalancePoint[] {
  return points.length === 1 ? [{ ...points[0]!, at: points[0]!.at - 1 }, points[0]!] : [...points];
}

export interface RangeChange {
  /** Last value minus first value. */
  amount: DecimalString;
  /** That change relative to the first value, in percent; null when the range starts at zero. */
  percent: DecimalString | null;
}

/** The change from the first point to the last, exact. Null for no points. */
export function rangeChange(points: readonly BalancePoint[]): RangeChange | null {
  const first = points[0];
  const last = points.at(-1);
  if (!first || !last) return null;
  const from = toUnits(first.value);
  const delta = toUnits(last.value) - from;
  const percent = from > 0n ? fromUnits(divRoundHalfEven(delta * 100n * 10n ** BigInt(SCALE), from)) : null;
  return { amount: fromUnits(delta), percent };
}

/* ------------------------------------------------------------------------ */
/* Geometry                                                                 */
/* ------------------------------------------------------------------------ */

export interface Domain {
  min: number;
  max: number;
}

/**
 * The value range the chart draws: the series with 10 % headroom (or a band
 * around a flat line), never below zero for a series that never is.
 */
export function valueDomain(points: readonly BalancePoint[]): Domain {
  if (points.length === 0) return { min: 0, max: 1 };
  const values = points.map((p) => Number(p.value));
  const low = Math.min(...values);
  const high = Math.max(...values);
  const pad = high > low ? (high - low) * 0.1 : Math.max(Math.abs(low) * 0.05, 1);
  const floor = low >= 0 ? 0 : -Infinity;
  return { min: Math.max(low - pad, floor), max: high + pad };
}

/** The time range the chart draws: first to last point, or a 1 ms span for a single instant. */
export function timeDomain(points: readonly BalancePoint[]): Domain {
  if (points.length === 0) return { min: 0, max: 1 };
  const min = points[0]!.at;
  const max = points.at(-1)!.at;
  return max > min ? { min, max } : { min, max: min + 1 };
}

/** A linear scale from `domain` onto [`from`, `to`] (to < from flips it, as for SVG y). */
export function scale(domain: Domain, from: number, to: number): (value: number) => number {
  const span = domain.max - domain.min || 1;
  return (value) => from + ((value - domain.min) / span) * (to - from);
}

const round2 = (n: number) => Math.round(n * 100) / 100;

/**
 * The SVG path of a series. "step" holds each value until the next point,
 * as a balance does; "linear" joins points with straight lines.
 */
export function linePath(
  points: readonly BalancePoint[],
  x: (at: number) => number,
  y: (value: number) => number,
  curve: "step" | "linear" = "step",
): string {
  if (points.length === 0) return "";
  const [first, ...rest] = points;
  let d = `M${round2(x(first!.at))},${round2(y(Number(first!.value)))}`;
  for (const p of rest) {
    const px = round2(x(p.at));
    const py = round2(y(Number(p.value)));
    d += curve === "step" ? `H${px}V${py}` : `L${px},${py}`;
  }
  return d;
}

/** About `count` round values across [min, max] for axis labels and grid lines. */
export function niceTicks(min: number, max: number, count = 4): number[] {
  if (!(max > min) || count < 1) return [min];
  const raw = (max - min) / count;
  const power = 10 ** Math.floor(Math.log10(raw));
  // The tolerance absorbs float error: (0.4 - 0.1) / 3 is 0.10000000000000002, still a step of 0.1.
  const fraction = raw / power - 1e-9;
  const step = (fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 2.5 ? 2.5 : fraction <= 5 ? 5 : 10) * power;
  const first = Math.ceil(min / step);
  const ticks: number[] = [];
  for (let i = first; i * step <= max + step * 1e-9; i++) ticks.push(Number((i * step).toPrecision(12)));
  return ticks;
}

/** The index of the point nearest in time to `at`; -1 for no points. */
export function nearestIndex(points: readonly BalancePoint[], at: number): number {
  let best = -1;
  let bestDistance = Infinity;
  points.forEach((p, i) => {
    const distance = Math.abs(p.at - at);
    // `<=`: of points at the same instant (a step), the later one is the balance after it.
    if (distance <= bestDistance) {
      best = i;
      bestDistance = distance;
    }
  });
  return best;
}

/* ------------------------------------------------------------------------ */
/* Labels (display only: never for amounts the user acts on)               */
/* ------------------------------------------------------------------------ */

const compact = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

/** "$25K", "$1.2M", "$950": axis labels. */
export function compactMoney(value: number): string {
  const text = compact.format(Math.abs(value));
  return `${value < 0 ? "−" : ""}$${text}`;
}

/**
 * Labels for a value axis: compact ("$25K", "$1.2M") when that tells every
 * tick apart; otherwise in full with the decimals the ticks need ("$1,575",
 * "$1,607.5"). A narrow range, such as a price over one day, would otherwise
 * print "$1.6K" on every line.
 */
export function axisMoney(ticks: readonly number[]): string[] {
  const short = ticks.map(compactMoney);
  if (new Set(short).size === short.length) return short;
  const decimals = Math.min(8, Math.max(0, ...ticks.map((t) => String(t).split(".")[1]?.length ?? 0)));
  const full = new Intl.NumberFormat("en-US", { minimumFractionDigits: decimals, maximumFractionDigits: decimals });
  return ticks.map((t) => `${t < 0 ? "−" : ""}$${full.format(Math.abs(t))}`);
}

const shortDate = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });
const shortDateYear = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", year: "numeric" });
const timeOfDay = new Intl.DateTimeFormat("en-US", { hour: "2-digit", minute: "2-digit" });

/**
 * A time-axis label sized to the span shown: the time of day within two days,
 * "Nov 9" within a year, "Nov 9, 2025" beyond.
 */
export function axisDate(at: number, span: number): string {
  if (span <= 2 * DAY_MS) return timeOfDay.format(at);
  return span <= 365 * DAY_MS ? shortDate.format(at) : shortDateYear.format(at);
}

const pointDate = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

/** "Nov 9, 2025, 02:05 PM": the date in the chart's readout. */
export function readoutDate(at: number): string {
  return pointDate.format(at);
}
