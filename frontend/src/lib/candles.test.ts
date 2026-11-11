/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CANDLE_INTERVALS,
  bucketEnd,
  bucketStart,
  candleChange,
  candleIndexAt,
  candleInterval,
  candleTime,
  describeCandle,
  groupCandles,
  historyCandles,
  priceView,
  timeTicks,
  type Candle,
  type CandleInterval,
} from "./candles.ts";
import { addUnits, compareAmounts, fromUnits, toUnits } from "./decimal.ts";
import type { BalancePoint } from "./portfolio.ts";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NOW = Date.parse("2026-11-11T18:07:31Z");

const interval = (id: CandleInterval["id"]) => CANDLE_INTERVALS.find((i) => i.id === id)!;
const cents = (n: number) => fromUnits(BigInt(Math.round(n * 100)) * 1_000_000n);

/**
 * A year of history shaped like the backend's (backend/src/mercado/historial.rs):
 * daily points on UTC midnights, hourly over the week before yesterday, the
 * previous close exactly 24 hours ago, every 15 minutes since, and `now`.
 */
function yearOfHistory(now = NOW): BalancePoint[] {
  const instants: number[] = [now];
  const yesterday = now - DAY;
  for (let at = Math.floor((now - 1) / (15 * MINUTE)) * 15 * MINUTE; at > yesterday; at -= 15 * MINUTE)
    instants.push(at);
  instants.push(yesterday);
  let next = yesterday;
  for (const [step, count] of [
    [HOUR, 144],
    [DAY, 358],
  ] as const) {
    let at = Math.floor((next - 1) / step) * step;
    for (let i = 0; i < count; i++) {
      instants.push(at);
      next = at;
      at -= step;
    }
  }
  return instants.reverse().map((at, i) => ({ at, value: cents(1600 + 25 * Math.sin(i / 9) + (i % 7) * 0.37) }));
}

const HISTORY = yearOfHistory();
const SIMULATED = historyCandles(HISTORY, { seed: "AUG1", volume24h: "184500000.00" });

/* From the price history */

test("historyCandles: each point opens at the one before and closes at its own, nothing invented", () => {
  const history: BalancePoint[] = [
    { at: 1_000, value: "100.00000000" },
    { at: 2_000, value: "101.50000000" },
    { at: 3_000, value: "99.25000000" },
  ];
  assert.deepEqual(historyCandles(history), [
    {
      start: 1_000,
      end: 1_000,
      open: "100.00000000",
      high: "100.00000000",
      low: "100.00000000",
      close: "100.00000000",
      volume: null,
    },
    {
      start: 1_000,
      end: 2_000,
      open: "100.00000000",
      high: "101.50000000",
      low: "100.00000000",
      close: "101.50000000",
      volume: null,
    },
    {
      start: 2_000,
      end: 3_000,
      open: "101.50000000",
      high: "101.50000000",
      low: "99.25000000",
      close: "99.25000000",
      volume: null,
    },
  ]);
  assert.deepEqual(historyCandles([]), []);
});

test("historyCandles: simulated wicks wrap the body, on the cent, within reach, and the same every time", () => {
  assert.deepEqual(historyCandles(HISTORY, { seed: "AUG1", volume24h: "184500000.00" }), SIMULATED);
  assert.notDeepEqual(historyCandles(HISTORY, { seed: "STSLA", volume24h: "184500000.00" }), SIMULATED);
  let wicks = 0;
  SIMULATED.forEach((candle, i) => {
    const point = HISTORY[i]!;
    assert.equal(candle.close, point.value);
    assert.equal(candle.open, (HISTORY[i - 1] ?? point).value);
    const [open, close, high, low] = [candle.open, candle.close, candle.high, candle.low].map(toUnits) as [
      bigint,
      bigint,
      bigint,
      bigint,
    ];
    const top = open > close ? open : close;
    const bottom = open < close ? open : close;
    assert.ok(low <= bottom && top <= high, `wicks wrap the body at ${i}`);
    assert.ok(low >= 1_000_000n, "never below a cent");
    assert.equal(high % 1_000_000n, 0n, "on the cent");
    assert.equal(low % 1_000_000n, 0n, "on the cent");
    // At most three quarters of 90 basis points (a daily spacing) beyond the body.
    assert.ok((high - top) * 10_000n <= top * 68n, `upper wick within reach at ${i}`);
    assert.ok((bottom - low) * 10_000n <= bottom * 68n, `lower wick within reach at ${i}`);
    if (high > top || low < bottom) wicks++;
  });
  assert.ok(wicks > SIMULATED.length / 2, "most simulated candles have a wick");
});

test("historyCandles: a price quoted to 4 decimals gets wicks on its own step", () => {
  const history = HISTORY.slice(-20).map((p, i) => ({
    ...p,
    value: fromUnits(toUnits(p.value) / 100n + BigInt(i) * 10_000n),
  }));
  const candles = historyCandles(history, { seed: "CETES28", volume24h: null });
  assert.ok(
    candles.some((c) => toUnits(c.high) % 1_000_000n !== 0n),
    "finer than the cent",
  );
  assert.ok(candles.every((c) => toUnits(c.high) % 10_000n === 0n && toUnits(c.low) % 10_000n === 0n));
  assert.ok(
    candles.every((c) => c.volume === null),
    "no volume without a 24-hour volume",
  );
});

test("historyCandles: the last 24 hours of simulated volume add up to the quote's 24-hour volume exactly", () => {
  const lastDay = SIMULATED.filter((c) => c.end > NOW - DAY);
  assert.equal(lastDay.length, 97, "every 15 minutes since the previous close, and now");
  assert.equal(addUnits(...lastDay.map((c) => c.volume!)), "184500000.00000000");
  assert.ok(SIMULATED.every((c) => c.volume !== null && toUnits(c.volume) >= 0n));
  // Older days trade on the same scale: a day's candle carries roughly a day's volume.
  const daily = SIMULATED.slice(1, 300).map((c) => Number(c.volume));
  const mean = daily.reduce((a, b) => a + b, 0) / daily.length;
  assert.ok(mean > 50_000_000 && mean < 600_000_000, `a day trades about the 24-hour volume (${mean})`);
});

/* Periods */

test("candleInterval: the shortest period that fits at most 60 candles and every gap between samples", () => {
  assert.equal(candleInterval(DAY, 15 * MINUTE).id, "30m");
  assert.equal(candleInterval(7 * DAY, HOUR).id, "4h");
  assert.equal(candleInterval(30 * DAY, DAY).id, "1d");
  assert.equal(candleInterval(30 * DAY, HOUR).id, "1d");
  assert.equal(candleInterval(365 * DAY, DAY).id, "1w");
  assert.equal(candleInterval(3 * HOUR, 15 * MINUTE).id, "15m");
  assert.equal(candleInterval(3 * HOUR, 2 * HOUR).id, "4h", "never shorter than the gap between samples");
  assert.equal(candleInterval(20 * 365 * DAY, DAY).id, "1w", "weekly at most");
});

test("bucketStart and bucketEnd: on the clock within a day, local midnight for a day, Monday for a week", () => {
  const at = Date.parse("2026-11-11T18:07:31Z");
  assert.equal(bucketStart(at, interval("30m")), Date.parse("2026-11-11T18:00:00Z"));
  assert.equal(bucketEnd(bucketStart(at, interval("30m")), interval("30m")), Date.parse("2026-11-11T18:30:00Z"));
  assert.equal(bucketStart(at, interval("4h")), Date.parse("2026-11-11T16:00:00Z"));

  const local = new Date(2026, 10, 11, 14, 7);
  const day = bucketStart(local.getTime(), interval("1d"));
  assert.equal(day, new Date(2026, 10, 11).getTime());
  assert.equal(bucketEnd(day, interval("1d")), new Date(2026, 10, 12).getTime());
  const week = bucketStart(local.getTime(), interval("1w"));
  assert.equal(week, new Date(2026, 10, 9).getTime(), "Monday, November 9");
  assert.equal(new Date(week).getDay(), 1);
  assert.equal(bucketEnd(week, interval("1w")), new Date(2026, 10, 16).getTime());
  assert.equal(
    bucketStart(new Date(2026, 10, 9).getTime(), interval("1w")),
    week,
    "Monday's midnight opens its own week",
  );
  assert.equal(
    bucketStart(new Date(2026, 10, 15, 23, 59).getTime(), interval("1w")),
    week,
    "Sunday night is still that week",
  );
});

/* Grouping */

const sub = (
  end: number,
  open: string,
  high: string,
  low: string,
  close: string,
  volume: string | null = "10",
): Candle => ({ start: end - 15 * MINUTE, end, open, high, low, close, volume });

test("groupCandles: first open, highest high, lowest low, last close, summed volume", () => {
  const t = Date.parse("2026-11-11T18:00:00Z");
  const candles = [
    sub(t + 15 * MINUTE, "100", "101", "99.5", "100.5", "10.25"),
    sub(t + 30 * MINUTE, "100.5", "103", "100", "102", "4.75"), // closes on the boundary: still 18:00-18:30
    sub(t + 45 * MINUTE, "102", "102.5", "98", "98.5", "1"),
    sub(t + 50 * MINUTE, "98.5", "99", "97.25", "98.75", "2"),
  ];
  assert.deepEqual(groupCandles(candles, interval("30m")), [
    {
      start: t,
      end: t + 30 * MINUTE,
      open: "100",
      high: "103.00000000",
      low: "99.50000000",
      close: "102",
      volume: "15.00000000",
    },
    {
      start: t + 30 * MINUTE,
      end: t + 50 * MINUTE,
      open: "102",
      high: "102.50000000",
      low: "97.25000000",
      close: "98.75",
      volume: "3.00000000",
    },
  ]);
  const unknown = groupCandles([candles[0]!, { ...candles[1]!, volume: null }], interval("30m"));
  assert.equal(unknown[0]!.volume, null, "a period with any unknown volume has an unknown volume");
  assert.deepEqual(groupCandles([], interval("1d")), []);
});

/* The window */

test("priceView: each range gets its period, whole periods up to the forming one, and continuous candles", () => {
  const expected = { "1D": "30m", "1W": "4h", "1M": "1d", "1Y": "1w", ALL: "1w" } as const;
  for (const [range, id] of Object.entries(expected) as [keyof typeof expected, CandleInterval["id"]][]) {
    const view = priceView(HISTORY, SIMULATED, range, NOW)!;
    assert.equal(view.interval.id, id, range);
    assert.ok(view.candles.length >= 2 && view.candles.length <= 61, `${range}: ${view.candles.length} candles`);
    assert.equal(view.from, view.candles[0]!.start, `${range} starts with its first candle`);
    assert.ok(
      view.to > NOW && view.to === bucketEnd(view.candles.at(-1)!.start, view.interval),
      `${range} ends with the forming candle`,
    );
    const last = view.candles.at(-1)!;
    assert.equal(last.close, HISTORY.at(-1)!.value, `${range} closes at the price`);
    assert.equal(last.end, NOW);
    view.candles.forEach((candle, i) => {
      if (i > 0)
        assert.equal(candle.open, view.candles[i - 1]!.close, `${range}: each candle opens at the close before`);
      assert.equal(candle.start, bucketStart(candle.start, view.interval), `${range}: candles open on their period`);
      assert.ok(compareAmounts(candle.low, view.low) >= 0 && compareAmounts(candle.high, view.high) <= 0);
    });
    assert.equal(view.line[0]!.at, Math.max(view.from, HISTORY[0]!.at), `${range}: the line starts with the window`);
    assert.equal(view.line[0]!.value, view.candles[0]!.open, `${range}: at the price the first candle opens at`);
    assert.equal(view.line.at(-1)!.at, NOW);
    assert.ok(compareAmounts(view.low, view.lineLow) <= 0 && compareAmounts(view.lineHigh, view.high) <= 0);
    assert.equal(
      view.maxVolume,
      view.candles.map((c) => c.volume!).reduce((a, b) => (compareAmounts(a, b) >= 0 ? a : b)),
      `${range}: the largest volume`,
    );
  }
  const day = priceView(HISTORY, SIMULATED, "1D", NOW)!;
  assert.ok(day.candles.length === 48 || day.candles.length === 49, `${day.candles.length} half hours`);
  assert.ok(day.from <= NOW - DAY && day.from > NOW - DAY - 30 * MINUTE);
});

test("priceView: the volume a window shows is the volume of the candles it groups, exactly", () => {
  for (const range of ["1D", "1W", "1M", "1Y"] as const) {
    const view = priceView(HISTORY, SIMULATED, range, NOW)!;
    const inside = SIMULATED.filter((c) => c.end > view.from);
    // A candle that opened before the window counts for its time inside only.
    const expected = inside.reduce(
      (sum, c, i) =>
        sum +
        (i === 0 && c.start < view.from
          ? (toUnits(c.volume!) * BigInt(c.end - view.from)) / BigInt(c.end - c.start)
          : toUnits(c.volume!)),
      0n,
    );
    assert.equal(addUnits(...view.candles.map((c) => c.volume!)), fromUnits(expected), range);
  }
  // The week opens inside the hour after a daily sample: that hour's candle is clipped, its wicks dropped.
  const week = priceView(HISTORY, SIMULATED, "1W", NOW)!;
  const opener = SIMULATED.find((c) => c.end > week.from)!;
  assert.ok(opener.start < week.from, "the week's first candle opened before the window");
  const first = week.candles[0]!;
  const rest = SIMULATED.filter((c) => c.end > opener.end && bucketStart(c.end - 1, week.interval) === week.from);
  const prices = [opener.open, opener.close, ...rest.flatMap((c) => [c.high, c.low])].map(toUnits);
  assert.equal(first.open, opener.open);
  assert.equal(
    toUnits(first.high),
    prices.reduce((a, b) => (a > b ? a : b)),
  );
  assert.equal(
    toUnits(first.low),
    prices.reduce((a, b) => (a < b ? a : b)),
  );
});

test("priceView: live prices draw candles from their samples alone, without volume", () => {
  const view = priceView(HISTORY, historyCandles(HISTORY), "1M", NOW)!;
  assert.equal(view.maxVolume, null);
  assert.ok(view.candles.every((c) => c.volume === null));
  assert.equal(view.low, view.lineLow, "no wick reaches past the samples");
  assert.equal(view.high, view.lineHigh);
});

test("priceView: a short history starts at its first point; a lone price is one candle", () => {
  const recent = HISTORY.slice(-10);
  const view = priceView(recent, historyCandles(recent), "1M", NOW)!;
  assert.equal(view.interval.id, "15m");
  assert.equal(view.line[0]!.value, recent[0]!.value);
  const lone = [{ at: NOW, value: "1612.40000000" }];
  const single = priceView(lone, historyCandles(lone), "1D", NOW)!;
  assert.equal(single.candles.length, 1);
  assert.equal(single.line.length, 2, "drawn flat");
  assert.ok(single.from < NOW && single.to > NOW);
  assert.equal(priceView([], [], "1D", NOW), null);
});

/* Reading */

test("candleIndexAt: the candle holding the instant, or the nearest", () => {
  const view = priceView(HISTORY, SIMULATED, "1D", NOW)!;
  const third = view.candles[2]!;
  assert.equal(candleIndexAt(view.candles, view.interval, third.start + 5 * MINUTE), 2);
  assert.equal(candleIndexAt(view.candles, view.interval, view.from - DAY), 0);
  assert.equal(candleIndexAt(view.candles, view.interval, view.to + DAY), view.candles.length - 1);
  assert.equal(candleIndexAt([], view.interval, NOW), -1);
});

test("candleChange: exact, in pesos and in percent of the open", () => {
  assert.deepEqual(candleChange({ open: "1600.00", close: "1612.40" }), {
    amount: "12.40000000",
    percent: "0.77500000",
  });
  assert.deepEqual(candleChange({ open: "3", close: "2" }), { amount: "-1.00000000", percent: "-33.33333333" });
  assert.deepEqual(candleChange({ open: "0", close: "2" }), { amount: "2.00000000", percent: null });
});

test("candleTime and describeCandle: what the readout and the keyboard say", () => {
  const start = new Date(2026, 10, 11, 14, 30).getTime();
  assert.equal(candleTime({ start }, interval("30m")), "Nov 11, 2026, 2:30 PM");
  assert.equal(candleTime({ start: new Date(2026, 10, 11).getTime() }, interval("1d")), "Wed, Nov 11, 2026");
  assert.equal(candleTime({ start: new Date(2026, 10, 9).getTime() }, interval("1w")), "Week of Nov 9, 2026");
  const candle: Candle = {
    start,
    end: start + 30 * MINUTE,
    open: "1610.00000000",
    high: "1615.20000000",
    low: "1608.40000000",
    close: "1612.40000000",
    volume: "1250000.00000000",
  };
  assert.equal(
    describeCandle(candle, interval("30m")),
    "Nov 11, 2026, 2:30 PM, 30-minute candle: opened $1,610.00, high $1,615.20, low $1,608.40, closed $1,612.40, volume $1.3M",
  );
  assert.equal(
    describeCandle({ ...candle, start: new Date(2026, 10, 11).getTime(), volume: null }, interval("1d")),
    "Wed, Nov 11, 2026, daily candle: opened $1,610.00, high $1,615.20, low $1,608.40, closed $1,612.40",
  );
});

/* Time axis */

test("timeTicks: round local instants inside the window, at most as many as asked", () => {
  const from = new Date(2026, 10, 10, 13, 30).getTime();
  const to = new Date(2026, 10, 11, 14, 0).getTime();
  const ticks = timeTicks(from, to, 6);
  assert.ok(ticks.length >= 3 && ticks.length <= 6, `${ticks.length} ticks`);
  for (const tick of ticks) {
    const at = new Date(tick.at);
    assert.ok(tick.at >= from && tick.at <= to);
    assert.equal(at.getMinutes(), 0);
    assert.equal(at.getHours() % 6, 0, "every 6 hours");
  }
  const midnight = ticks.find((t) => new Date(t.at).getHours() === 0)!;
  assert.equal(midnight.label, "Nov 11", "midnight shows the date");
  assert.equal(ticks.find((t) => new Date(t.at).getHours() === 18)!.label, "6 PM");

  const halfHours = timeTicks(new Date(2026, 10, 11, 14, 10).getTime(), new Date(2026, 10, 11, 16, 50).getTime(), 6);
  assert.deepEqual(
    halfHours.map((t) => t.label),
    ["2:30 PM", "3 PM", "3:30 PM", "4 PM", "4:30 PM"],
  );

  const year = timeTicks(new Date(2025, 10, 12).getTime(), new Date(2026, 10, 12).getTime(), 6);
  assert.deepEqual(
    year.map((t) => t.label),
    ["2026", "Mar", "May", "Jul", "Sep", "Nov"],
    "every other month, and the year in January",
  );
  assert.ok(year.every((t) => new Date(t.at).getDate() === 1 && new Date(t.at).getHours() === 0));

  const month = timeTicks(new Date(2026, 9, 12).getTime(), new Date(2026, 10, 12).getTime(), 6);
  assert.ok(month.length <= 6 && month.every((t) => new Date(t.at).getDay() === 1), "Mondays across a month");
  assert.deepEqual(timeTicks(5, 5, 6), []);
});
