/**
 * Candles: the OHLCV periods the asset page's pro chart draws
 * (components/charts/pro-candlestick-chart.tsx). How a quote's price history
 * becomes candles, how candles group into the period a range reads best in,
 * and the window the chart's two modes (line and candles) share.
 *
 * - `Candle` is the shape a market data feed serves: the price a period
 *   opened at, its highest and lowest, the price it closed at, and the MXN
 *   traded in it.
 * - Until the price oracle serves candles, `historyCandles` derives one from
 *   each point of the quote's history (types/market.ts): it opens at the
 *   point before and closes at its own price. For simulated prices (demo
 *   mode) it also simulates what two samples cannot show: the wicks traded
 *   between them, and the volume, whose last 24 hours add up to the quote's
 *   24-hour volume exactly. The draws are seeded with the ticker and the
 *   instant, so a quote always draws the same candles. A live price never
 *   gets invented wicks or volume: its candles span its own samples, and its
 *   volume is unknown.
 * - `groupCandles` aggregates exactly: the first open, the highest high, the
 *   lowest low, the last close and the sum of the volumes.
 * - `priceView` is what a range shows: the candle period that fits it, the
 *   window its candles fill, the line over the same window, and the price
 *   extent both modes draw against, so switching modes keeps the axes still.
 *
 * Prices and volumes are exact decimal strings (BigInt units of 10^-8, as in
 * lib/decimal.ts); only pixel positions and axis labels use floating point.
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { SCALE, divRoundHalfEven, fromUnits, toUnits } from "./decimal.ts";
import { formatPrice } from "./market.ts";
import { compactMoney, drawable, pointsSince, rangeDays, type BalancePoint, type ChartRangeId } from "./portfolio.ts";
import type { DecimalString } from "../types/vault.ts";

/** One period of trading. */
export interface Candle {
  /** When the period opened, epoch ms. */
  start: number;
  /** When its close was read, epoch ms: its last instant so far (`now` for a period still forming). */
  end: number;
  open: DecimalString;
  high: DecimalString;
  low: DecimalString;
  close: DecimalString;
  /** MXN traded in the period; null when unknown. */
  volume: DecimalString | null;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/* ------------------------------------------------------------------------ */
/* Periods                                                                  */
/* ------------------------------------------------------------------------ */

/**
 * The periods a candle can last, shortest first. `label` is the legend's chip
 * (in words, so a daily candle never reads like the 1D range); `name` reads
 * before "candle(s)".
 */
export const CANDLE_INTERVALS = [
  { id: "15m", label: "15 min", name: "15-minute", ms: 15 * MINUTE },
  { id: "30m", label: "30 min", name: "30-minute", ms: 30 * MINUTE },
  { id: "1h", label: "1 hour", name: "1-hour", ms: HOUR },
  { id: "4h", label: "4 hours", name: "4-hour", ms: 4 * HOUR },
  { id: "1d", label: "Daily", name: "Daily", ms: DAY },
  { id: "1w", label: "Weekly", name: "Weekly", ms: WEEK },
] as const;

export type CandleInterval = (typeof CANDLE_INTERVALS)[number];

/** At most this many candles across a window, so each stays wide enough to read. */
export const MAX_CANDLES = 60;

/**
 * The candle period for a window `span` ms long whose samples are at most
 * `maxGap` apart: the shortest that puts at most MAX_CANDLES across it and
 * is never shorter than the gap between samples (a period without a sample
 * would draw nothing). With the quote's history: 30 minutes over a day,
 * 4 hours over a week, a day over a month, a week over a year.
 */
export function candleInterval(span: number, maxGap: number): CandleInterval {
  return (
    CANDLE_INTERVALS.find((interval) => interval.ms >= maxGap && span <= MAX_CANDLES * interval.ms) ??
    CANDLE_INTERVALS[CANDLE_INTERVALS.length - 1]!
  );
}

/**
 * When the period holding instant `at` opens: on the clock for periods
 * shorter than a day (multiples of the period since the epoch: :00 and :30
 * for 30 minutes), at local midnight for a day, and Monday's for a week.
 */
export function bucketStart(at: number, interval: CandleInterval): number {
  if (interval.ms < DAY) return Math.floor(at / interval.ms) * interval.ms;
  const day = new Date(at);
  const back = interval.id === "1w" ? (day.getDay() + 6) % 7 : 0;
  return new Date(day.getFullYear(), day.getMonth(), day.getDate() - back).getTime();
}

/** When the period opening at `start` closes: the next one's opening (a calendar day, across a clock change too). */
export function bucketEnd(start: number, interval: CandleInterval): number {
  if (interval.ms < DAY) return start + interval.ms;
  const day = new Date(start);
  return new Date(day.getFullYear(), day.getMonth(), day.getDate() + (interval.id === "1w" ? 7 : 1)).getTime();
}

/* ------------------------------------------------------------------------ */
/* From the price history                                                   */
/* ------------------------------------------------------------------------ */

/** What simulated candles are drawn from. */
export interface CandleSimulation {
  /** Seeds the draws: the asset's ticker. */
  seed: string;
  /** MXN traded over the last 24 hours (the quote's `volume_24h`); null leaves the volume unknown. */
  volume24h: DecimalString | null;
}

/**
 * One candle per point of `history` (oldest first): it opens at the point
 * before (the first point opens at its own price) and closes at its own.
 *
 * With `simulation` (simulated prices only), each candle also gets:
 * - wicks: up to three quarters of the largest move the simulator makes over
 *   that spacing beyond the body on each side (about 6 basis points for 15
 *   minutes, 18 for an hour, 90 for a day: backend/src/mercado/historial.rs),
 *   on the history's own price step, never below one step;
 * - a volume weighted by the time it spans, a draw and the size of its move,
 *   scaled so the candles of the last 24 hours add up to `volume24h` exactly
 *   (the last candle takes the cents the rounding leaves), and every older
 *   candle on the same scale.
 */
export function historyCandles(history: readonly BalancePoint[], simulation: CandleSimulation | null = null): Candle[] {
  const step = priceStep(history);
  const weights: bigint[] = [];
  const candles: Candle[] = history.map((point, i) => {
    const before = history[i - 1] ?? point;
    const open = toUnits(before.value);
    const close = toUnits(point.value);
    let high = open > close ? open : close;
    let low = open < close ? open : close;
    if (simulation) {
      const spacing = point.at - before.at;
      const reach = (high * wickReach(spacing)) / 10_000n;
      high += floorTo((reach * BigInt(draw(simulation.seed, point.at, 1) % 76)) / 100n, step);
      const floor = low < step ? low : step;
      const below = low - floorTo((reach * BigInt(draw(simulation.seed, point.at, 2) % 76)) / 100n, step);
      low = below > floor ? below : floor;
      const minutes = BigInt(Math.max(1, Math.round(spacing / MINUTE)));
      const move = open > 0n ? ((close > open ? close - open : open - close) * 10_000n) / open : 0n;
      const boost = move * 15n > 1_500n ? 1_500n : move * 15n;
      weights.push(minutes * (500n + BigInt(draw(simulation.seed, point.at, 3) % 1_001)) * (1_000n + boost));
    }
    return {
      start: before.at,
      end: point.at,
      open: fromUnits(open),
      high: fromUnits(high),
      low: fromUnits(low),
      close: fromUnits(close),
      volume: null,
    };
  });

  const last = history.at(-1);
  if (!simulation || simulation.volume24h === null || !last) return candles;
  const target = toUnits(simulation.volume24h);
  const dayStart = last.at - DAY;
  const dayWeight = history.reduce((sum, point, i) => (point.at > dayStart ? sum + weights[i]! : sum), 0n);
  const cent = 10n ** BigInt(SCALE - 2);
  let dayVolume = 0n;
  const volumes = weights.map((weight, i) => {
    const volume = floorTo((target * weight) / dayWeight, cent);
    if (history[i]!.at > dayStart) dayVolume += volume;
    return volume;
  });
  const lastIndex = volumes.length - 1;
  volumes[lastIndex] = volumes[lastIndex]! + target - dayVolume;
  return candles.map((candle, i) => ({ ...candle, volume: fromUnits(volumes[i]!) }));
}

/** Basis points of the price a simulated wick may reach, by the time its candle spans. */
function wickReach(spacing: number): bigint {
  return spacing <= 15 * MINUTE ? 6n : spacing <= HOUR ? 18n : 90n;
}

/** The step the history's prices are quoted in, in units of 10^-8: a cent, or finer when a price has more decimals. */
function priceStep(history: readonly BalancePoint[]): bigint {
  let decimals = 2;
  for (const point of history) {
    const fraction = point.value.split(".")[1]?.replace(/0+$/, "") ?? "";
    if (fraction.length > decimals) decimals = Math.min(fraction.length, SCALE);
  }
  return 10n ** BigInt(SCALE - decimals);
}

/** `value` (not negative) rounded down to a multiple of `step`. */
function floorTo(value: bigint, step: bigint): bigint {
  return value - (value % step);
}

/** A uniform 32-bit draw for (`seed`, `at`, `salt`): FNV-1a, then MurmurHash3's finalizer. */
function draw(seed: string, at: number, salt: number): number {
  const text = `${seed}:${at}:${salt}`;
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  h ^= h >>> 16;
  return h >>> 0;
}

/* ------------------------------------------------------------------------ */
/* Grouping and the window                                                  */
/* ------------------------------------------------------------------------ */

/**
 * Candles (oldest first) grouped into periods of `interval`: each candle
 * counts in the period its close falls in (a close on the boundary closes
 * the period before). Exact: the first open, the highest high, the lowest
 * low, the last close, and the sum of the volumes (unknown if any is).
 */
export function groupCandles(candles: readonly Candle[], interval: CandleInterval): Candle[] {
  const groups: Candle[] = [];
  let group: {
    start: number;
    end: number;
    open: string;
    high: bigint;
    low: bigint;
    close: string;
    volume: bigint | null;
  } | null = null;
  const flush = () => {
    if (!group) return;
    const { start, end, open, high, low, close, volume } = group;
    groups.push({
      start,
      end,
      open,
      high: fromUnits(high),
      low: fromUnits(low),
      close,
      volume: volume === null ? null : fromUnits(volume),
    });
  };
  for (const candle of candles) {
    const start = bucketStart(candle.end - 1, interval);
    const high = toUnits(candle.high);
    const low = toUnits(candle.low);
    const volume = candle.volume === null ? null : toUnits(candle.volume);
    if (!group || group.start !== start) {
      flush();
      group = { start, end: candle.end, open: candle.open, high, low, close: candle.close, volume };
      continue;
    }
    group.end = candle.end;
    if (high > group.high) group.high = high;
    if (low < group.low) group.low = low;
    group.close = candle.close;
    group.volume = group.volume === null || volume === null ? null : group.volume + volume;
  }
  flush();
  return groups;
}

/** What one range of the chart shows. */
export interface PriceView {
  interval: CandleInterval;
  /** The window both modes draw, epoch ms: from the first candle's opening to when the forming one closes. */
  from: number;
  to: number;
  /** The line over the window, oldest first: the price in force at `from`, then every point after. */
  line: BalancePoint[];
  /** The candles over the window, oldest first; the last one is still forming. */
  candles: Candle[];
  /** The line's lowest and highest price. */
  lineLow: DecimalString;
  lineHigh: DecimalString;
  /** The lowest and highest price the window reaches, wicks included: the price axis of both modes. */
  low: DecimalString;
  high: DecimalString;
  /** The largest volume of a candle; null when the volume is unknown. */
  maxVolume: DecimalString | null;
}

/**
 * The view of `range`: its candle period (`candleInterval` over the samples
 * inside it), whole periods from the one the range starts in to the one
 * forming now, the candles grouped over them, and the line over the same
 * window. `candles` are the finest available, oldest first
 * (`historyCandles(history)`). Null without history.
 *
 * The first candle of the window may open before it: only its time inside
 * counts (its open, its close and its volume pro rata, not wicks it may have
 * drawn before the window).
 */
export function priceView(
  history: readonly BalancePoint[],
  candles: readonly Candle[],
  range: ChartRangeId,
  now: number,
): PriceView | null {
  const first = history[0];
  const last = history.at(-1);
  if (!first || !last || candles.length === 0) return null;
  const end = Math.max(now, last.at);
  const days = rangeDays(range);
  const start = days === null ? first.at : Math.max(end - days * DAY, first.at);
  let maxGap = 0;
  for (let i = 1; i < history.length; i++) {
    const gap = history[i]!.at - history[i - 1]!.at;
    if (history[i - 1]!.at >= start && gap > maxGap) maxGap = gap;
  }
  const interval = candleInterval(Math.max(end - start, 1), maxGap);
  const forming = bucketStart(end - 1, interval);
  const from = Math.min(bucketStart(start, interval), forming);
  const to = bucketEnd(forming, interval);

  const inside = candles.filter((candle) => candle.end > from);
  const grouped = groupCandles(
    inside.map((candle, i) => (i === 0 && candle.start < from ? clip(candle, from) : candle)),
    interval,
  );
  const line = drawable(pointsSince(history, from, end));

  let lineLow = toUnits(line[0]!.value);
  let lineHigh = lineLow;
  for (const point of line) {
    const value = toUnits(point.value);
    if (value < lineLow) lineLow = value;
    if (value > lineHigh) lineHigh = value;
  }
  let low = lineLow;
  let high = lineHigh;
  let maxVolume: bigint | null = 0n;
  for (const candle of grouped) {
    const candleLow = toUnits(candle.low);
    const candleHigh = toUnits(candle.high);
    if (candleLow < low) low = candleLow;
    if (candleHigh > high) high = candleHigh;
    if (candle.volume === null) maxVolume = null;
    else if (maxVolume !== null && toUnits(candle.volume) > maxVolume) maxVolume = toUnits(candle.volume);
  }

  return {
    interval,
    from,
    to,
    line,
    candles: grouped,
    lineLow: fromUnits(lineLow),
    lineHigh: fromUnits(lineHigh),
    low: fromUnits(low),
    high: fromUnits(high),
    maxVolume: maxVolume === null || grouped.length === 0 ? null : fromUnits(maxVolume),
  };
}

/** The part of `candle` from `from` on: its open and close (no wicks), and its volume for that share of its time. */
function clip(candle: Candle, from: number): Candle {
  const open = toUnits(candle.open);
  const close = toUnits(candle.close);
  const span = BigInt(candle.end - candle.start);
  return {
    ...candle,
    start: from,
    high: fromUnits(open > close ? open : close),
    low: fromUnits(open < close ? open : close),
    volume: candle.volume === null ? null : fromUnits((toUnits(candle.volume) * BigInt(candle.end - from)) / span),
  };
}

/* ------------------------------------------------------------------------ */
/* Reading a candle                                                         */
/* ------------------------------------------------------------------------ */

/** The index of the candle whose period is nearest to instant `at` (the one holding it); -1 for none. */
export function candleIndexAt(candles: readonly Candle[], interval: CandleInterval, at: number): number {
  let best = -1;
  let bestDistance = Infinity;
  candles.forEach((candle, i) => {
    const distance = Math.abs(at - (candle.start + bucketEnd(candle.start, interval)) / 2);
    if (distance < bestDistance) {
      best = i;
      bestDistance = distance;
    }
  });
  return best;
}

/** Close minus open, and that change in percent of the open (null for an open at zero). Exact. */
export function candleChange(candle: Pick<Candle, "open" | "close">): {
  amount: DecimalString;
  percent: DecimalString | null;
} {
  const open = toUnits(candle.open);
  const delta = toUnits(candle.close) - open;
  const percent = open > 0n ? fromUnits(divRoundHalfEven(delta * 100n * 10n ** BigInt(SCALE), open)) : null;
  return { amount: fromUnits(delta), percent };
}

const intradayFormat = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});
const dayFormat = new Intl.DateTimeFormat("en-US", {
  weekday: "short",
  month: "short",
  day: "numeric",
  year: "numeric",
});
const weekFormat = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", year: "numeric" });

/** When a candle opened, as its readout says: "Nov 11, 2026, 2:30 PM"; "Wed, Nov 11, 2026" for a day; "Week of Nov 9, 2026". */
export function candleTime(candle: Pick<Candle, "start">, interval: CandleInterval): string {
  if (interval.ms < DAY) return intradayFormat.format(candle.start);
  return interval.id === "1d" ? dayFormat.format(candle.start) : `Week of ${weekFormat.format(candle.start)}`;
}

/** A candle in words, as the keyboard reaches it. */
export function describeCandle(candle: Candle, interval: CandleInterval): string {
  const figures = [
    `opened ${formatPrice(candle.open)}`,
    `high ${formatPrice(candle.high)}`,
    `low ${formatPrice(candle.low)}`,
    `closed ${formatPrice(candle.close)}`,
  ];
  if (candle.volume !== null) figures.push(`volume ${compactMoney(Number(candle.volume))}`);
  return `${candleTime(candle, interval)}, ${interval.name.toLowerCase()} candle: ${figures.join(", ")}`;
}

/* ------------------------------------------------------------------------ */
/* Time axis                                                                */
/* ------------------------------------------------------------------------ */

export interface TimeTick {
  at: number;
  label: string;
}

type TickUnit = "minute" | "hour" | "day" | "week" | "month";

/** Tick spacings, finest first, with their rough length for counting. */
const TICK_STEPS: readonly { unit: TickUnit; count: number; approx: number }[] = [
  { unit: "minute", count: 15, approx: 15 * MINUTE },
  { unit: "minute", count: 30, approx: 30 * MINUTE },
  { unit: "hour", count: 1, approx: HOUR },
  { unit: "hour", count: 2, approx: 2 * HOUR },
  { unit: "hour", count: 3, approx: 3 * HOUR },
  { unit: "hour", count: 6, approx: 6 * HOUR },
  { unit: "hour", count: 12, approx: 12 * HOUR },
  { unit: "day", count: 1, approx: DAY },
  { unit: "day", count: 2, approx: 2 * DAY },
  { unit: "week", count: 1, approx: WEEK },
  { unit: "week", count: 2, approx: 2 * WEEK },
  { unit: "month", count: 1, approx: 30 * DAY },
  { unit: "month", count: 2, approx: 61 * DAY },
  { unit: "month", count: 3, approx: 91 * DAY },
  { unit: "month", count: 6, approx: 182 * DAY },
  { unit: "month", count: 12, approx: 365 * DAY },
];

const tickHour = new Intl.DateTimeFormat("en-US", { hour: "numeric" });
const tickMinute = new Intl.DateTimeFormat("en-US", { hour: "numeric", minute: "2-digit" });
const tickDay = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });
const tickMonth = new Intl.DateTimeFormat("en-US", { month: "short" });
const tickYear = new Intl.DateTimeFormat("en-US", { year: "numeric" });

/** Local midnight on January 3, 2000, a Monday: where day and week steps count from. */
const EPOCH_MONDAY = new Date(2000, 0, 3).getTime();

/**
 * At most `maxTicks` round instants between `from` and `to` (inclusive), in
 * local time, for the time axis: on the hour or the half hour within a day
 * or two ("2 PM", "2:30 PM", and the date at midnight: "Nov 11"), on days or
 * Mondays within months ("Nov 11"), on the first of the month beyond
 * ("Nov", and the year in January: "2027").
 */
export function timeTicks(from: number, to: number, maxTicks: number): TimeTick[] {
  if (!(to > from) || maxTicks < 1) return [];
  const span = to - from;
  const step = TICK_STEPS.find((s) => span / s.approx <= maxTicks) ?? TICK_STEPS[TICK_STEPS.length - 1]!;
  const ticks: TimeTick[] = [];
  const add = (at: number, label: string) => {
    if (at >= from && at <= to) ticks.push({ at, label });
  };

  if (step.unit === "month") {
    const first = new Date(from);
    for (let i = 0; ; i++) {
      const month = new Date(first.getFullYear(), first.getMonth() + i, 1);
      if (month.getTime() > to) break;
      if (month.getMonth() % step.count === 0) {
        add(month.getTime(), month.getMonth() === 0 ? tickYear.format(month) : tickMonth.format(month));
      }
    }
    return ticks;
  }

  const firstDay = new Date(from);
  for (let i = 0; ; i++) {
    const day = new Date(firstDay.getFullYear(), firstDay.getMonth(), firstDay.getDate() + i);
    const midnight = day.getTime();
    if (midnight > to) break;
    const days = Math.round((midnight - EPOCH_MONDAY) / DAY);
    if (step.unit === "day") {
      if (days % step.count === 0) add(midnight, tickDay.format(day));
    } else if (step.unit === "week") {
      if (days % (7 * step.count) === 0) add(midnight, tickDay.format(day));
    } else {
      const perHour = step.unit === "minute" ? 60 / step.count : 1;
      for (let hour = 0; hour < 24; hour += step.unit === "hour" ? step.count : 1) {
        for (let k = 0; k < perHour; k++) {
          const minute = k * (step.unit === "minute" ? step.count : 0);
          const at = new Date(day.getFullYear(), day.getMonth(), day.getDate(), hour, minute);
          // A clock change can skip an hour or repeat one: keep the instants that fall where they say.
          if (at.getHours() !== hour || at.getDate() !== day.getDate()) continue;
          const label =
            hour === 0 && minute === 0
              ? tickDay.format(at)
              : minute === 0
                ? tickHour.format(at)
                : tickMinute.format(at);
          add(at.getTime(), label);
        }
      }
    }
  }
  return ticks;
}
