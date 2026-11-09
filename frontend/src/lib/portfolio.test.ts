/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { TransaccionIfpe } from "./api/types.ts";
import {
  cashBalanceHistory,
  drawable,
  cashDelta,
  axisMoney,
  compactMoney,
  linePath,
  nearestIndex,
  niceTicks,
  pointsInRange,
  rangeChange,
  scale,
  timeDomain,
  valueDomain,
  type BalancePoint,
} from "./portfolio.ts";

const ACCOUNT = "acc-ana";
const OTHER = "acc-beto";
const DAY = 86_400_000;
const T0 = Date.parse("2026-11-01T12:00:00Z");

let sequence = 0;
function tx(tipo: TransaccionIfpe["tipo"], monto: string, day: number, origin: string | null = null) {
  sequence += 1;
  return {
    secuencia: sequence,
    tipo,
    monto,
    cuenta_origen_id: origin,
    creado_en: new Date(T0 + day * DAY).toISOString(),
  };
}

/* History */

test("cashDelta: credits, debits, transfers by side, and holds that change nothing", () => {
  assert.equal(cashDelta(tx("DEPOSITO", "100.5", 0), ACCOUNT), 10_050_000_000n);
  assert.equal(cashDelta(tx("RETORNO_DE_SOFOM", "1", 0), ACCOUNT), 100_000_000n);
  assert.equal(cashDelta(tx("RETIRO", "1", 0, ACCOUNT), ACCOUNT), -100_000_000n);
  assert.equal(cashDelta(tx("TRASPASO_A_SOFOM", "1", 0, ACCOUNT), ACCOUNT), -100_000_000n);
  assert.equal(cashDelta(tx("TRANSFERENCIA", "1", 0, ACCOUNT), ACCOUNT), -100_000_000n, "sent");
  assert.equal(cashDelta(tx("TRANSFERENCIA", "1", 0, OTHER), ACCOUNT), 100_000_000n, "received");
  assert.equal(cashDelta(tx("RETENCION", "1", 0, ACCOUNT), ACCOUNT), 0n);
  assert.equal(cashDelta(tx("LIBERACION_RETENCION", "1", 0, ACCOUNT), ACCOUNT), 0n);
  // Market orders: a buy pays from the vault's cash, a sale pays into it.
  assert.equal(cashDelta(tx("COMPRA_MERCADO", "1249.93", 0, ACCOUNT), ACCOUNT), -124_993_000_000n);
  assert.equal(cashDelta(tx("VENTA_MERCADO", "499.84", 0), ACCOUNT), 49_984_000_000n);
});

test("cashBalanceHistory: rebuilt backwards from the current balance, exact, oldest first", () => {
  const movements = [
    tx("DEPOSITO", "25000", 0),
    tx("TRASPASO_A_SOFOM", "10000", 1, ACCOUNT),
    tx("RETENCION", "500", 2, ACCOUNT),
    tx("TRANSFERENCIA", "0.01", 3, OTHER),
  ];
  // Newest first, as the API returns them: the order of the input does not matter.
  const now = T0 + 5 * DAY;
  const points = cashBalanceHistory([...movements].reverse(), ACCOUNT, "15000.01", now);
  assert.deepEqual(
    points.map((p) => [(p.at - T0) / DAY, p.value]),
    [
      [0, "0.00000000"], // before the first deposit: the account was empty
      [0, "25000.00000000"],
      [1, "15000.00000000"],
      [2, "15000.00000000"], // the hold moved nothing out of the account
      [3, "15000.01000000"],
      [5, "15000.01000000"], // now
    ],
  );
});

test("cashBalanceHistory: no movements is the current balance alone; a lagging clock never runs backwards", () => {
  assert.deepEqual(cashBalanceHistory([], ACCOUNT, "42", T0), [{ at: T0, value: "42.00000000" }]);
  const late = tx("DEPOSITO", "1", 3);
  const points = cashBalanceHistory([late], ACCOUNT, "1", T0); // "now" is 3 days before the movement
  assert.deepEqual(
    points.map((p) => p.at),
    [Date.parse(late.creado_en), Date.parse(late.creado_en), Date.parse(late.creado_en)],
  );
});

test("cashBalanceHistory: a partial page starts from the balance before its oldest movement", () => {
  // Only the last two of many movements are loaded: the start is not zero.
  const page = [tx("DEPOSITO", "10", 8), tx("RETIRO", "4", 9, ACCOUNT)];
  const points = cashBalanceHistory(page, ACCOUNT, "106", T0 + 10 * DAY);
  assert.equal(points[0]?.value, "100.00000000");
  assert.equal(points.at(-1)?.value, "106.00000000");
});

/* Ranges */

const series: BalancePoint[] = [
  { at: T0, value: "0" },
  { at: T0, value: "100" },
  { at: T0 + 20 * DAY, value: "150" },
  { at: T0 + 28 * DAY, value: "120" },
  { at: T0 + 30 * DAY, value: "120" },
];
const NOW = T0 + 30 * DAY;

test("pointsInRange: the balance in force at the start opens the range", () => {
  assert.deepEqual(pointsInRange(series, "1W", NOW), [
    { at: NOW - 7 * DAY, value: "150" },
    { at: T0 + 28 * DAY, value: "120" },
    { at: NOW, value: "120" },
  ]);
});

test("pointsInRange: history that starts inside the range, and ALL, show every point", () => {
  assert.deepEqual(pointsInRange(series, "3M", NOW), series);
  assert.deepEqual(pointsInRange(series, "ALL", NOW), series);
  assert.deepEqual(pointsInRange([], "1M", NOW), []);
});

test("pointsInRange: nothing inside the range is a flat line across it", () => {
  const old: BalancePoint[] = [{ at: T0, value: "7" }];
  assert.deepEqual(pointsInRange(old, "1W", NOW), [
    { at: NOW - 7 * DAY, value: "7" },
    { at: NOW, value: "7" },
  ]);
});

test("rangeChange: exact amount and percent; no percent from zero", () => {
  assert.deepEqual(rangeChange(pointsInRange(series, "1W", NOW)), { amount: "-30.00000000", percent: "-20.00000000" });
  assert.deepEqual(rangeChange([{ at: 0, value: "3" }, { at: 1, value: "4" }]), {
    amount: "1.00000000",
    percent: "33.33333333",
  });
  assert.deepEqual(rangeChange(series), { amount: "120.00000000", percent: null });
  assert.equal(rangeChange([]), null);
});

/* Geometry */

test("valueDomain: headroom, never below zero for a non-negative series, a band for a flat line", () => {
  assert.deepEqual(valueDomain([{ at: 0, value: "100" }, { at: 1, value: "200" }]), { min: 90, max: 210 });
  assert.deepEqual(valueDomain([{ at: 0, value: "0" }, { at: 1, value: "50" }]), { min: 0, max: 55 });
  assert.deepEqual(valueDomain([{ at: 0, value: "1000" }]), { min: 950, max: 1050 });
  assert.deepEqual(valueDomain([{ at: 0, value: "0" }]), { min: 0, max: 1 });
});

test("timeDomain and scale: a single instant still has a span; scales can flip", () => {
  assert.deepEqual(timeDomain([{ at: 5, value: "1" }]), { min: 5, max: 6 });
  const y = scale({ min: 0, max: 100 }, 200, 0);
  assert.equal(y(0), 200);
  assert.equal(y(100), 0);
  assert.equal(y(25), 150);
});

test("linePath: a step holds each value until the next point; linear joins them", () => {
  const points: BalancePoint[] = [
    { at: 0, value: "0" },
    { at: 10, value: "10" },
    { at: 20, value: "5" },
  ];
  const identity = (n: number) => n;
  assert.equal(linePath(points, identity, identity, "step"), "M0,0H10V10H20V5");
  assert.equal(linePath(points, identity, identity, "linear"), "M0,0L10,10L20,5");
  assert.equal(linePath([], identity, identity), "");
});

test("niceTicks: round steps inside the domain, no floating-point dust", () => {
  assert.deepEqual(niceTicks(0, 100, 4), [0, 25, 50, 75, 100]);
  assert.deepEqual(niceTicks(14_250, 15_750, 3), [14_500, 15_000, 15_500]);
  assert.deepEqual(niceTicks(0.1, 0.4, 3), [0.1, 0.2, 0.3, 0.4]);
  assert.deepEqual(niceTicks(5, 5), [5]);
});

test("nearestIndex: the closest point, and the later one of a step", () => {
  assert.equal(nearestIndex(series, T0 + 21 * DAY), 2);
  assert.equal(nearestIndex(series, T0), 1, "the balance after the step");
  assert.equal(nearestIndex([], 0), -1);
});

test("compactMoney: short axis labels", () => {
  assert.equal(compactMoney(25_000), "$25K");
  assert.equal(compactMoney(1_250_000), "$1.3M");
  assert.equal(compactMoney(950), "$950");
  assert.equal(compactMoney(-1_500), "−$1.5K");
});

test("axisMoney: compact when the labels differ, in full when compact would repeat", () => {
  assert.deepEqual(axisMoney([0, 5_000, 10_000, 15_000]), ["$0", "$5K", "$10K", "$15K"]);
  assert.deepEqual(axisMoney([15_000, 15_100, 15_200]), ["$15K", "$15.1K", "$15.2K"]);
  // A price over a day: "$1.6K" three times would say nothing.
  assert.deepEqual(axisMoney([1_575, 1_600, 1_625]), ["$1,575", "$1,600", "$1,625"]);
  assert.deepEqual(axisMoney([1_607.5, 1_610, 1_612.5]), ["$1,607.5", "$1,610.0", "$1,612.5"]);
  assert.deepEqual(axisMoney([1_500_000, 1_502_000]), ["$1,500,000", "$1,502,000"]);
  assert.deepEqual(axisMoney([-1_520, -1_510]), ["−$1,520", "−$1,510"]);
  assert.deepEqual(axisMoney([15_000.37]), ["$15K"], "a flat line's one label");
  assert.deepEqual(axisMoney([]), []);
});

test("pointsInRange: 1D keeps the last day, opened by the balance in force a day ago", () => {
  assert.deepEqual(pointsInRange(series, "1D", NOW), [
    { at: NOW - DAY, value: "120" },
    { at: NOW, value: "120" },
  ]);
});

test("drawable: a lone point gets a twin a millisecond earlier; anything else is unchanged", () => {
  assert.deepEqual(drawable([{ at: 10, value: "5" }]), [
    { at: 9, value: "5" },
    { at: 10, value: "5" },
  ]);
  assert.deepEqual(drawable(series), series);
  assert.deepEqual(drawable([]), []);
});
