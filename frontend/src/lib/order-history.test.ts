/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  appendPage,
  formatOrderTime,
  historyAmount,
  historyKind,
  historyPrice,
  ordersPath,
  ordersTab,
  parseHistoryEntry,
  parseOrderHistory,
  rejectReasonText,
  waited,
} from "./order-history.ts";

/** The history of backend/tests/seguimiento.rs, newest first. */
const REJECTED = {
  order_id: "0f000000-0000-0000-0000-000000000005",
  status: "rejected",
  order_type: "limit",
  side: "buy",
  ticker: "AUG1",
  amount: "100.00000000",
  limit_price: "1500.00000000",
  stop_price: null,
  quantity: null,
  price: null,
  total: null,
  commission: null,
  reject_reason: "KYC_NO_VERIFICADO",
  placed_at: "2026-11-24T18:10:00Z",
  closed_at: "2026-11-24T18:12:00Z",
  source_order_id: null,
  plan_id: null,
};
const LIMIT_FILL = {
  ...REJECTED,
  order_id: "0e000000-0000-0000-0000-000000000004",
  status: "filled",
  amount: "1250.00000000",
  limit_price: "1600.00000000",
  quantity: "0.78610000",
  price: "1590.00000000",
  total: "1249.90000000",
  commission: "0.00000000",
  reject_reason: null,
  placed_at: "2026-11-24T18:04:00Z",
  closed_at: "2026-11-24T18:09:00Z",
  source_order_id: "0f000000-0000-0000-0000-000000000003",
};
const CANCELLED = {
  ...REJECTED,
  order_id: "0f000000-0000-0000-0000-000000000002",
  status: "cancelled",
  side: "sell",
  limit_price: "1700.00000000",
  reject_reason: null,
  placed_at: "2026-11-24T18:05:00Z",
  closed_at: "2026-11-24T18:06:00Z",
};
const PLAN_FILL = {
  ...LIMIT_FILL,
  order_id: "0e000000-0000-0000-0000-000000000001",
  order_type: "savings_plan",
  ticker: "CETES28",
  amount: "100.00000000",
  limit_price: null,
  quantity: "10.00000000",
  price: "9.94390000",
  total: "99.44000000",
  placed_at: "2026-11-24T18:02:00Z",
  closed_at: "2026-11-24T18:02:00Z",
  source_order_id: null,
  plan_id: "9a000000-0000-0000-0000-000000000001",
};
const MARKET_FILL = {
  ...PLAN_FILL,
  order_id: "0e000000-0000-0000-0000-000000000000",
  order_type: "market",
  ticker: "AUG1",
  amount: "500.00000000",
  quantity: "0.31000000",
  price: "1612.40000000",
  total: "499.84000000",
  placed_at: "2026-11-24T18:01:00Z",
  closed_at: "2026-11-24T18:01:00Z",
  plan_id: null,
};
const PAGE = {
  orders: [REJECTED, LIMIT_FILL, CANCELLED, PLAN_FILL, MARKET_FILL],
  next_cursor: "1795546401502123_0e00",
};

test("a page as the backend answers it", () => {
  assert.deepEqual(parseOrderHistory(PAGE), PAGE);
  assert.deepEqual(parseOrderHistory({ orders: [], next_cursor: null }), { orders: [], next_cursor: null });
});

test("a malformed or incoherent order is rejected, and its page with it", () => {
  for (const bad of [
    { ...MARKET_FILL, status: "pending" },
    { ...MARKET_FILL, order_type: "twap" },
    { ...MARKET_FILL, quantity: null },
    { ...MARKET_FILL, total: null },
    { ...CANCELLED, price: "1700.00000000" },
    { ...CANCELLED, reject_reason: "SALDO_INSUFICIENTE" },
    { ...REJECTED, reject_reason: null },
    { ...REJECTED, reject_reason: "no había dinero" },
    { ...LIMIT_FILL, limit_price: null },
    { ...MARKET_FILL, limit_price: "1600.00000000" },
    { ...PLAN_FILL, plan_id: null },
    { ...MARKET_FILL, source_order_id: "x" },
    { ...MARKET_FILL, status: "cancelled", quantity: null, price: null, total: null, commission: null },
    { ...PLAN_FILL, side: "sell" },
    { ...LIMIT_FILL, closed_at: "2026-11-24T18:00:00Z" },
  ]) {
    assert.equal(parseHistoryEntry(bad), null, JSON.stringify(bad));
    assert.equal(parseOrderHistory({ orders: [MARKET_FILL, bad], next_cursor: null }), null);
  }
  assert.equal(parseOrderHistory({ orders: [MARKET_FILL, MARKET_FILL], next_cursor: null }), null, "an order twice");
  assert.equal(parseOrderHistory({ orders: [], next_cursor: 7 }), null);
});

test("pages join in order, each order once", () => {
  const first = parseOrderHistory({ orders: [REJECTED, LIMIT_FILL], next_cursor: "c" })!.orders;
  const second = parseOrderHistory({ orders: [LIMIT_FILL, CANCELLED], next_cursor: null })!.orders;
  assert.deepEqual(
    appendPage(first, second).map((e) => e.order_id),
    [REJECTED.order_id, LIMIT_FILL.order_id, CANCELLED.order_id],
  );
});

test("each row in words: the kind, the price, the money, when", () => {
  const [rejected, limitFill, cancelled, planFill, marketFill] = parseOrderHistory(PAGE)!.orders;
  assert.deepEqual(
    [rejected, limitFill, cancelled, planFill, marketFill].map((e) => historyKind(e!)),
    ["Limit buy", "Limit buy", "Limit sell", "Savings plan purchase", "Market buy"],
  );
  assert.equal(historyKind({ order_type: "stop", side: "sell" }), "Stop-loss");
  assert.equal(historyKind({ order_type: "market", side: "sell" }), "Market sale");
  assert.equal(historyPrice(limitFill!), "at $1,590.00");
  assert.equal(historyPrice(cancelled!), "Limit $1,700.00");
  assert.equal(historyPrice(planFill!), "at $9.9439");
  assert.equal(historyAmount(limitFill!), "1249.90000000");
  assert.equal(historyAmount(cancelled!), "100.00000000");
  assert.equal(waited(limitFill!), true);
  assert.equal(waited(marketFill!), false);
  // Mexico City time (UTC−6); the year only outside `now`'s.
  const now = Date.parse("2026-11-24T20:00:00Z");
  assert.equal(formatOrderTime("2026-11-24T18:12:00Z", now), "Nov 24 · 12:12 PM");
  assert.equal(formatOrderTime("2025-12-30T15:15:00Z", now), "Dec 30, 2025 · 9:15 AM");
});

test("why it was rejected: what a client can act on, otherwise nothing about why", () => {
  assert.equal(rejectReasonText("SALDO_INSUFICIENTE"), "Not enough available cash when it triggered.");
  assert.equal(rejectReasonText("KYC_NO_VERIFICADO"), "Your identity verification was pending when it triggered.");
  const quiet = "The bank could not fill it when it triggered. Nothing was charged.";
  for (const code of ["OPERACION_NO_DISPONIBLE", "CUENTA_CONGELADA", "CLIENTE_EN_LISTA_BLOQUEADAS", "toString", null]) {
    assert.equal(rejectReasonText(code), quiet, String(code));
  }
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  for (const code of [
    "SALDO_INSUFICIENTE",
    "MONTO_INSUFICIENTE",
    "POSICION_INSUFICIENTE",
    "KYC_NO_VERIFICADO",
    "CUENTA_CONGELADA",
  ]) {
    assert.doesNotMatch(rejectReasonText(code), TIP_OFF, code);
  }
});

test("the Orders page's tabs round-trip through the address", () => {
  assert.equal(ordersPath("open"), "/dashboard/orders");
  assert.equal(ordersPath("history"), "/dashboard/orders?tab=history");
  assert.equal(ordersTab("history"), "history");
  for (const other of [null, "", "open", "HISTORY", "pending"]) assert.equal(ordersTab(other), "open");
});
