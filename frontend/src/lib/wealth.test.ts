/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  SOVEREIGN_YIELD_PCT,
  accrued,
  cardholderName,
  counterDecimals,
  formatTruncated,
  monthWindow,
  saveback,
  savebackPreview,
  shareBasisPoints,
  yieldPreview,
} from "./wealth.ts";

const DAY = 86_400_000;
const NOW = Date.parse("2026-11-11T18:07:31Z"); // 12:07:31 in Mexico City

test("monthWindow: the Mexico City month, UTC−6 all year", () => {
  assert.deepEqual(monthWindow(NOW), {
    start: Date.parse("2026-11-01T06:00:00Z"),
    end: Date.parse("2026-12-01T06:00:00Z"),
    day: 11,
    days: 30,
  });
  const lastSecond = monthWindow(Date.parse("2026-12-01T05:59:59Z"));
  assert.equal(lastSecond.day, 30, "still November in Mexico City");
  assert.equal(lastSecond.start, Date.parse("2026-11-01T06:00:00Z"));
  const december = monthWindow(Date.parse("2026-12-01T06:00:00Z"));
  assert.deepEqual([december.day, december.days], [1, 31]);
  assert.equal(
    monthWindow(Date.parse("2027-01-01T05:00:00Z")).start,
    Date.parse("2026-12-01T06:00:00Z"),
    "New Year's Eve",
  );
  assert.equal(monthWindow(Date.parse("2028-02-15T12:00:00Z")).days, 29, "a leap February");
});

test("accrued: ACT/360 on the balance, exact to 10^-8 and rounded down", () => {
  assert.equal(accrued("15000.00", "7.25", DAY), 302_083_333n, "15,000 × 7.25 % / 360 = 3.020833333…");
  assert.equal(accrued("15000.00", "7.25", 360 * DAY), 108_750_000_000n, "a whole 360-day year: 1,087.50");
  assert.equal(accrued("15000.00", "7.25", 30 * DAY), 9_062_500_000n, "thirty days: 90.625");
  assert.equal(accrued("0.00", "7.25", DAY), 0n);
  assert.equal(accrued("-10.00", "7.25", DAY), 0n, "nothing accrues on a balance below zero");
  assert.equal(accrued("15000.00", "7.25", -5), 0n);
  assert.equal(accrued("15000.00", "0", DAY), 0n);
  assert.equal(SOVEREIGN_YIELD_PCT, "7.25");
});

test("yieldPreview: since the 1st, a day, and the whole month, on today's balance", () => {
  const preview = yieldPreview("15000.00", NOW);
  assert.equal(preview.perDay, 302_083_333n);
  assert.equal(preview.projected, 9_062_500_000n, "November has 30 days");
  assert.equal(preview.earned, accrued("15000.00", "7.25", NOW - Date.parse("2026-11-01T06:00:00Z")));
  assert.ok(preview.earned > 10n * preview.perDay && preview.earned < 11n * preview.perDay, "10 and a half days in");
  assert.ok(yieldPreview("15000.00", NOW + 1_000).earned > preview.earned, "a second later it has grown");
});

test("counterDecimals: four at least, enough for the last digit to move every tenth of a second, eight at most", () => {
  assert.equal(counterDecimals("1000000.00"), 4);
  assert.equal(counterDecimals("10000.00"), 6);
  assert.equal(counterDecimals("100.00"), 8);
  assert.equal(counterDecimals("0.00"), 4);
});

test("formatTruncated: rounds down, groups thousands", () => {
  assert.equal(formatTruncated(123_456_789_012n, 4), "1,234.5678", "never rounds up to …5679");
  assert.equal(formatTruncated(123_456_789_012n, 6), "1,234.567890");
  assert.equal(formatTruncated(123_456_789_012n, 8), "1,234.56789012");
  assert.equal(formatTruncated(123_456_789_012n, 0), "1,234");
  assert.equal(formatTruncated(0n, 4), "0.0000");
  assert.equal(formatTruncated(-150_000_000n, 2), "-1.50");
});

test("shareBasisPoints: rounded down, whole at most", () => {
  assert.equal(shareBasisPoints(1n, 3n), 3333);
  assert.equal(shareBasisPoints(5n, 3n), 10_000);
  assert.equal(shareBasisPoints(1n, 0n), 0);
  assert.equal(shareBasisPoints(0n, 3n), 0);
});

test("saveback: 1 % of spending, rounded down to the cent, never above the cap", () => {
  const peso = 100_000_000n;
  assert.deepEqual(saveback(12_345n * peso + 67_000_000n), {
    spent: 12_345n * peso + 67_000_000n,
    earned: 123n * peso + 45_000_000n,
    cap: 300n * peso,
    progress: 4115,
  });
  assert.equal(saveback(40_000n * peso).earned, 300n * peso, "capped at 300.00");
  assert.equal(saveback(40_000n * peso).progress, 10_000);
  assert.equal(saveback(0n).earned, 0n);
});

test("savebackPreview: the same simulated month on every visit, growing day by day", () => {
  const today = savebackPreview(NOW, "client-1");
  assert.deepEqual(savebackPreview(NOW, "client-1"), today, "deterministic");
  assert.notDeepEqual(savebackPreview(NOW, "client-2"), today, "seeded per client");
  const peso = 100_000_000n;
  assert.ok(today.spent >= 11n * 220n * peso && today.spent <= 11n * 899n * peso, "eleven days of 220 to 899");
  assert.equal(today.earned % 1_000_000n, 0n, "on the cent");
  assert.equal(today.earned, saveback(today.spent).earned);
  const tomorrow = savebackPreview(NOW + DAY, "client-1");
  assert.ok(tomorrow.spent > today.spent);
  assert.ok(
    savebackPreview(Date.parse("2026-12-01T12:00:00Z"), "client-1").spent < today.spent,
    "a new month starts over",
  );
});

test("cardholderName: capitals without accents, first surname, 26 characters at most", () => {
  assert.equal(cardholderName({ nombre: "José", apellido_paterno: "Ñúñez" }), "JOSE NUNEZ");
  assert.equal(cardholderName({ nombre: "Ana  María", apellido_paterno: null }), "ANA MARIA");
  assert.equal(
    cardholderName({ nombre: "Maximiliana Guadalupe", apellido_paterno: "Hernández-Villaseñor" }),
    "MAXIMILIANA GUADALUPE HERN",
  );
});
