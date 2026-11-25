/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  alertsCount,
  alertsFor,
  buildPriceAlert,
  directionFor,
  notifySentence,
  parsePriceAlert,
  parsePriceAlerts,
  reachedSentence,
  targetPhrase,
  toAnnounce,
} from "./price-alerts.ts";
import type { PriceAlert } from "../types/market.ts";

/** The backend's answer for "notify me if gold rises to 1,700" (backend/tests/seguimiento.rs). */
function alert(overrides: Record<string, unknown> = {}) {
  return {
    alert_id: "7a000000-0000-0000-0000-000000000001",
    ticker: "AUG1",
    direction: "above",
    target_price: "1700.00000000",
    status: "active",
    reference_price: "1612.40000000",
    triggered_price: null,
    created_at: "2026-11-24T18:03:00.123Z",
    triggered_at: null,
    cancelled_at: null,
    ...overrides,
  };
}

const TRIGGERED = { status: "triggered", triggered_price: "1701.20000000", triggered_at: "2026-11-24T19:00:00Z" };

function parsed(overrides: Record<string, unknown> = {}): PriceAlert {
  const a = parsePriceAlert(alert(overrides));
  assert.ok(a, JSON.stringify(overrides));
  return a;
}

test("an alert as the backend answers it: active, reached, cancelled", () => {
  assert.deepEqual(parsePriceAlert(alert()), alert());
  assert.ok(parsePriceAlert(alert(TRIGGERED)));
  assert.ok(parsePriceAlert(alert({ status: "cancelled", cancelled_at: "2026-11-24T19:00:00Z" })));
  assert.ok(parsePriceAlert(alert({ direction: "below", target_price: "1500.00000000" })));
});

test("a malformed or incoherent alert is rejected whole", () => {
  for (const bad of [
    null,
    alert({ direction: "cross" }),
    alert({ ticker: "aug1" }),
    alert({ target_price: "0" }),
    alert({ target_price: 1700 }),
    alert({ status: "waiting" }),
    // The target lies on its direction's side of the price it was set at.
    alert({ target_price: "1600.00000000" }),
    alert({ direction: "below" }),
    // Reached: with its price and time, and a price that reached the target.
    alert({ status: "triggered" }),
    alert({ ...TRIGGERED, triggered_at: null }),
    alert({ ...TRIGGERED, triggered_price: "1650.00000000" }),
    alert({ triggered_price: "1701.20000000" }),
    alert({ status: "cancelled" }),
    alert({ cancelled_at: "2026-11-24T19:00:00Z" }),
    alert({ created_at: "soon" }),
  ]) {
    assert.equal(parsePriceAlert(bad), null, JSON.stringify(bad));
  }
  assert.equal(parsePriceAlerts({ alerts: [alert(), alert()] }), null, "the same alert twice");
  assert.deepEqual(parsePriceAlerts({ alerts: [] }), { alerts: [] });
});

test("the target's side sets the direction; the current price cannot be one", () => {
  assert.equal(directionFor("1700", "1612.40000000"), "above");
  assert.equal(directionFor("1500.5", "1612.40000000"), "below");
  assert.equal(directionFor("1612.4", "1612.40000000"), null);
  assert.equal(directionFor("", "1612.40000000"), null);
  assert.equal(directionFor("0", "1612.40000000"), null);
  assert.equal(notifySentence("AUG1", "1700", "1612.40000000"), "Notify me when AUG1 rises to $1,700.00");
  assert.equal(notifySentence("CETES28", "9.5", "9.94390000"), "Notify me when CETES28 falls to $9.50");
  assert.equal(
    notifySentence("AUG1", "1612.40", "1612.40000000"),
    "AUG1 is at $1,612.40 now: choose a price above or below it.",
  );
  assert.equal(notifySentence("AUG1", "", "1612.40000000"), null);
  assert.deepEqual(buildPriceAlert("AUG1", "1700.25", "1612.40000000", "k-12345678"), {
    ticker: "AUG1",
    target_price: "1700.25",
    direction: "above",
    idempotency_key: "k-12345678",
  });
  assert.equal(buildPriceAlert("AUG1", "1612.4", "1612.40000000", "k-12345678"), null);
});

test("words for each alert", () => {
  assert.equal(targetPhrase(parsed()), "Rises to $1,700.00");
  assert.equal(
    targetPhrase(parsed({ direction: "below", target_price: "9.50000000", reference_price: "9.94390000" })),
    "Falls to $9.50",
  );
  assert.equal(reachedSentence(parsed(TRIGGERED)), "AUG1 rose to $1,701.20, past your $1,700.00 target.");
  assert.equal(
    reachedSentence(parsed({ ...TRIGGERED, triggered_price: "1700.00000000" })),
    "AUG1 rose to $1,700.00, your target.",
  );
  assert.equal(alertsCount([parsed(), parsed(), parsed(TRIGGERED)]), "2 active · 1 reached");
  assert.equal(alertsCount([]), "None set");
});

test("an asset's card: active ones nearest first, then the reached ones, newest first", () => {
  const near = parsed({ alert_id: "a-near", target_price: "1650.00000000" });
  const far = parsed({ alert_id: "a-far", target_price: "1800.00000000" });
  const below = parsed({ alert_id: "a-below", direction: "below", target_price: "1600.00000000" });
  const old = parsed({ alert_id: "a-old", ...TRIGGERED, triggered_at: "2026-11-20T10:00:00Z" });
  const recent = parsed({ alert_id: "a-recent", ...TRIGGERED });
  const other = parsed({
    alert_id: "o",
    ticker: "AGT1",
    target_price: "600.00000000",
    reference_price: "548.20000000",
  });
  const cancelled = parsed({ alert_id: "c", status: "cancelled", cancelled_at: "2026-11-24T19:00:00Z" });
  assert.deepEqual(
    alertsFor([far, old, other, near, recent, cancelled, below], "AUG1", "1612.40000000").map((a) => a.alert_id),
    ["a-below", "a-near", "a-far", "a-recent", "a-old"],
  );
  // What to announce: reached after the last one acknowledged, newest first.
  assert.deepEqual(
    toAnnounce([old, recent, near], Date.parse("2026-11-21T00:00:00Z")).map((a) => a.alert_id),
    ["a-recent"],
  );
  assert.deepEqual(
    toAnnounce([old, recent], 0).map((a) => a.alert_id),
    ["a-recent", "a-old"],
  );
});

test("no wording would tip off a client under review", () => {
  // Nothing on a client screen may use the words of an AML hold (e2e/aml.mjs TIP_OFF):
  // the feature is "Price alerts", plural, and no sentence says "alert".
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  for (const words of [
    targetPhrase(parsed()),
    reachedSentence(parsed(TRIGGERED)),
    notifySentence("AUG1", "1700", "1612.40000000")!,
    notifySentence("AUG1", "1612.4", "1612.40000000")!,
    alertsCount([parsed(), parsed(TRIGGERED)]),
    "Price alerts",
  ]) {
    assert.doesNotMatch(words, TIP_OFF, words);
  }
});
