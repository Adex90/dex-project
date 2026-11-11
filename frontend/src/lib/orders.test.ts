/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { PendingOrder } from "../types/market.ts";
import { cancelledSentence, estimatedUnits, heldLabel, orderKind, targetPrice, triggerPhrase } from "./orders.ts";

/** A pending limit buy, as backend/tests/ordenes_avanzadas.rs places it. */
function order(overrides: Partial<PendingOrder> = {}): PendingOrder {
  return {
    order_id: "f1000000-0000-0000-0000-000000000001",
    order_type: "limit",
    status: "pending",
    ticker: "AUG1",
    side: "buy",
    amount: "1250.00000000",
    limit_price: "1600.00000000",
    stop_price: null,
    held_cash: "1250.00000000",
    held_quantity: "0.00000000",
    created_at: "2026-11-10T12:00:00.123Z",
    closed_at: null,
    reject_reason: null,
    execution: null,
    ...overrides,
  };
}

const limitSell = order({
  side: "sell",
  amount: "500.00000000",
  limit_price: "1700.00000000",
  held_cash: "0.00000000",
  held_quantity: "0.29410000",
});
const stopBuy = order({ order_type: "stop", amount: "100.00000000", limit_price: null, stop_price: "1650.00000000", held_cash: "100.00000000" });
const stopSell = order({
  order_type: "stop",
  side: "sell",
  amount: "200.00000000",
  limit_price: null,
  stop_price: "1550.00000000",
  held_cash: "0.00000000",
  held_quantity: "0.12900000",
});

test("orderKind: limit or stop, and a stop that sells is a stop-loss", () => {
  assert.deepEqual([order(), limitSell, stopBuy, stopSell].map(orderKind), ["Limit buy", "Limit sell", "Stop buy", "Stop-loss"]);
});

test("targetPrice and triggerPhrase: the price each type waits for, and how", () => {
  assert.deepEqual([order(), limitSell, stopBuy, stopSell].map(targetPrice), [
    "1600.00000000",
    "1700.00000000",
    "1650.00000000",
    "1550.00000000",
  ]);
  assert.deepEqual([order(), limitSell, stopBuy, stopSell].map(triggerPhrase), [
    "at $1,600.00 or lower",
    "at $1,700.00 or higher",
    "when the price rises to $1,650.00",
    "when the price falls to $1,550.00",
  ]);
  // Four decimals when the price has them (CETES).
  assert.equal(triggerPhrase(order({ limit_price: "9.94390000" })), "at $9.9439 or lower");
});

test("heldLabel: a buy holds its cash, a sell its units", () => {
  assert.equal(heldLabel(order()), "$1,250.00");
  assert.equal(heldLabel(limitSell), "0.2941 AUG1");
});

test("estimatedUnits: a buy at its own price, rounded down; a sell, exactly what it holds", () => {
  // 1,250 at 1,600 is 0.78125: 0.7812 in 0.0001 increments, never more than the money buys.
  assert.equal(estimatedUnits(order()), "0.7812");
  // Whole units (CETES28): 1,000 at 9.9439 buys 100.
  assert.equal(estimatedUnits(order({ amount: "1000.00000000", limit_price: "9.94390000" }), 0), "100");
  assert.equal(estimatedUnits(stopBuy), "0.0606");
  assert.equal(estimatedUnits(limitSell), "0.29410000");
  assert.equal(estimatedUnits(order({ limit_price: null })), null, "a limit without its price");
});

test("cancelledSentence: what was cancelled and what is free again", () => {
  assert.equal(cancelledSentence(order()), "Limit buy of AUG1 cancelled. $1,250.00 is back in your cash.");
  assert.equal(cancelledSentence(stopSell), "Stop-loss of AUG1 cancelled. Your 0.129 AUG1 are free to sell again.");
});
