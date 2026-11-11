/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  parsePendingOrder,
  parsePendingOrders,
  assetPath,
  isTicker,
  parseMarketCatalog,
  parseMarketQuote,
  parseOrderReceipt,
  positionFor,
  sellableFor,
} from "./market-data.ts";
import type { Holding } from "../types/vault.ts";

/** The backend's answer for AUG1 (backend/tests/mercado.rs), with a short history. */
function quote(overrides: Record<string, unknown> = {}, asset: Record<string, unknown> = {}) {
  return {
    asset: {
      ticker: "AUG1",
      name: "Tokenized gold, 1 g",
      asset_class: "Commodity",
      price: "1612.40000000",
      change_24h_pct: "0.84",
      market_cap: "1250000000.00",
      volume_24h: "84300000.00",
      yield_pct: null,
      quantity_decimals: 4,
      ...asset,
    },
    history: [
      { at: 1_762_600_000_000, value: "1598.97000000" },
      { at: 1_762_650_000_000, value: "1605.10000000" },
      { at: 1_762_686_400_000, value: "1612.40000000" },
    ],
    as_of: "2026-11-09T12:00:00.123Z",
    price_source: "simulated",
    ...overrides,
  };
}

/** A filled buy (backend/tests/mercado.rs). */
function receipt(overrides: Record<string, unknown> = {}) {
  return {
    order_id: "0d000000-0000-0000-0000-000000000001",
    order_type: "market",
    status: "filled",
    ticker: "AUG1",
    side: "buy",
    quantity: "0.77520000",
    price: "1612.40000000",
    amount: "1249.93000000",
    commission: "0.00000000",
    executed_at: "2026-11-09T12:00:01.456Z",
    ...overrides,
  };
}

test("parseMarketQuote: the backend's quote, as sent", () => {
  const parsed = parseMarketQuote(quote());
  assert.ok(parsed);
  assert.equal(parsed.asset.ticker, "AUG1");
  assert.equal(parsed.asset.market_cap, "1250000000.00");
  assert.equal(parsed.asset.yield_pct, null);
  assert.equal(parsed.history.length, 3);
  assert.equal(parsed.price_source, "simulated");
  assert.deepEqual(parseMarketQuote(quote({}, { yield_pct: "10.25", market_cap: null, quantity_decimals: 0 }))?.asset, {
    ...quote().asset,
    yield_pct: "10.25",
    market_cap: null,
    quantity_decimals: 0,
  });
});

test("parseMarketQuote: a malformed quote is rejected whole", () => {
  const malos: [string, unknown][] = [
    ["not an object", "AUG1"],
    ["no asset", quote({ asset: undefined })],
    ["lowercase ticker", quote({}, { ticker: "aug1" })],
    ["price as a number", quote({}, { price: 1612.4 })],
    ["zero price", quote({}, { price: "0" })],
    ["nine decimals", quote({}, { price: "1612.400000001" })],
    ["negative market cap", quote({}, { market_cap: "-1" })],
    ["quantity decimals out of range", quote({}, { quantity_decimals: 9 })],
    ["fractional quantity decimals", quote({}, { quantity_decimals: 1.5 })],
    ["empty name", quote({}, { name: "  " })],
    ["empty history", quote({ history: [] })],
    ["history out of order", quote({ history: [{ at: 2, value: "1" }, { at: 1, value: "1" }] })],
    ["two points at one instant", quote({ history: [{ at: 1, value: "1" }, { at: 1, value: "2" }] })],
    ["a point without a value", quote({ history: [{ at: 1 }] })],
    ["a point at a fractional ms", quote({ history: [{ at: 1.5, value: "1" }] })],
    ["unreadable as_of", quote({ as_of: "yesterday" })],
    ["unknown price source", quote({ price_source: "guess" })],
  ];
  for (const [motivo, valor] of malos) {
    assert.equal(parseMarketQuote(valor), null, motivo);
  }
});

/** The backend's catalog (backend/tests/mercado.rs), two of its five listings. */
function catalog(assets: unknown[] = [listing(), listing({ ticker: "STSLA", name: "Synthetic Tesla, 1 share" })]) {
  return { assets, as_of: "2026-11-09T12:00:00.123Z" };
}

function listing(overrides: Record<string, unknown> = {}) {
  return { ...quote().asset, price_source: "simulated", ...overrides };
}

test("parseMarketCatalog: the backend's catalog, as sent", () => {
  assert.deepEqual(parseMarketCatalog(catalog()), catalog());
  assert.deepEqual(parseMarketCatalog(catalog([])), catalog([]), "an empty catalog is still a catalog");
  const live = parseMarketCatalog(catalog([listing({ price_source: "oracle" })]));
  assert.equal(live?.assets[0]?.price_source, "oracle");
});

test("parseMarketCatalog: a malformed catalog is rejected whole", () => {
  const malos: [string, unknown][] = [
    ["not an object", []],
    ["no assets", { as_of: "2026-11-09T12:00:00Z" }],
    ["assets not a list", catalog("AUG1" as unknown as unknown[])],
    ["unreadable as_of", { ...catalog(), as_of: "now" }],
    ["a listing without its price source", catalog([listing({ price_source: undefined })])],
    ["an unknown price source", catalog([listing({ price_source: "guess" })])],
    ["a malformed asset among good ones", catalog([listing(), listing({ ticker: "stsla" })])],
    ["a ticker listed twice", catalog([listing(), listing({ name: "Gold again" })])],
    ["far too many listings", catalog(Array.from({ length: 1001 }, (_, i) => listing({ ticker: `A${i}` })))],
  ];
  for (const [motivo, valor] of malos) {
    assert.equal(parseMarketCatalog(valor), null, motivo);
  }
});

test("parseOrderReceipt: the backend's receipt, as sent", () => {
  assert.deepEqual(parseOrderReceipt(receipt()), receipt());
  assert.deepEqual(parseOrderReceipt(receipt({ side: "sell" })), receipt({ side: "sell" }));
  // A backend before 0015 sent no order_type: still a market receipt.
  const sinTipo: Record<string, unknown> = receipt();
  delete sinTipo.order_type;
  assert.deepEqual(parseOrderReceipt(sinTipo), receipt());
});

test("parseOrderReceipt: a malformed receipt is rejected whole", () => {
  const malos: [string, unknown][] = [
    ["null", null],
    ["no commission", receipt({ commission: undefined })],
    ["negative commission", receipt({ commission: "-0.01" })],
    ["zero quantity", receipt({ quantity: "0" })],
    ["amount as a number", receipt({ amount: 1249.93 })],
    ["unknown side", receipt({ side: "hold" })],
    ["unknown status", receipt({ status: "done" })],
    ["a market order never waits", receipt({ status: "pending" })],
    ["a limit order is a PendingOrder", receipt({ order_type: "limit" })],
    ["empty order id", receipt({ order_id: "" })],
    ["unreadable time", receipt({ executed_at: "now" })],
  ];
  for (const [motivo, valor] of malos) {
    assert.equal(parseOrderReceipt(valor), null, motivo);
  }
});

test("positionFor: the holding's shares; sellableFor: those not held by pending sells", () => {
  const holdings: Holding[] = [
    {
      ticker: "AUG1",
      name: "Tokenized gold, 1 g",
      shares: "0.77520000",
      held_shares: "0.00000000",
      current_price: "1612.40000000",
      total_value: "1249.93248000",
      daily_change_pct: "0.84",
    },
  ];
  assert.equal(positionFor(holdings, "AUG1"), "0.77520000");
  assert.equal(positionFor(holdings, "AGT1"), "0");
  assert.equal(positionFor([], "AUG1"), "0");
  assert.equal(sellableFor(holdings, "AUG1"), "0.77520000");
  assert.equal(sellableFor([], "AUG1"), "0");
  // 0.4231 g held by a limit and a stop sell (tests/db/0015_ordenes_avanzadas.sql): 0.3521 free.
  const held = [{ ...holdings[0]!, held_shares: "0.42310000" }];
  assert.equal(positionFor(held, "AUG1"), "0.77520000");
  assert.equal(sellableFor(held, "AUG1"), "0.35210000");
});

test("isTicker and assetPath", () => {
  for (const bueno of ["AUG1", "SCT28", "A", "ABCDEFGHIJKL"]) assert.ok(isTicker(bueno), bueno);
  for (const malo of ["", "aug1", "1AUG", "AU-G1", "ABCDEFGHIJKLM", " AUG1", 7]) assert.ok(!isTicker(malo), String(malo));
  assert.equal(assetPath("AUG1"), "/dashboard/asset/AUG1");
  assert.equal(assetPath("A/B"), "/dashboard/asset/A%2FB");
});

/** A pending limit buy, as backend/tests/ordenes_avanzadas.rs places it. */
function pendingOrder(overrides: Record<string, unknown> = {}) {
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

const FILL = {
  order_id: "0d000000-0000-0000-0000-000000000011",
  quantity: "0.78610000",
  price: "1590.00000000",
  amount: "1249.90000000",
  commission: "0.00000000",
  executed_at: "2026-11-10T12:05:00.000Z",
};

test("parsePendingOrder: pending, filled, cancelled and rejected orders, as sent", () => {
  const casos = [
    pendingOrder(),
    pendingOrder({ order_type: "stop", limit_price: null, stop_price: "1650.00000000" }),
    pendingOrder({ side: "sell", held_cash: "0.00000000", held_quantity: "0.29410000", limit_price: "1700.00000000" }),
    pendingOrder({ status: "filled", held_cash: "0.00000000", closed_at: FILL.executed_at, execution: FILL }),
    pendingOrder({ status: "cancelled", held_cash: "0.00000000", closed_at: "2026-11-10T12:01:00Z" }),
    pendingOrder({
      status: "rejected",
      held_cash: "0.00000000",
      closed_at: "2026-11-10T12:01:00Z",
      reject_reason: "MONTO_INSUFICIENTE",
    }),
  ];
  for (const orden of casos) assert.deepEqual(parsePendingOrder(orden), orden, JSON.stringify(orden));
});

test("parsePendingOrder: a malformed or incoherent order is rejected whole", () => {
  const malos: [string, unknown][] = [
    ["null", null],
    ["a market order", pendingOrder({ order_type: "market" })],
    ["a limit without its price", pendingOrder({ limit_price: null })],
    ["a limit with a stop price", pendingOrder({ stop_price: "1700.00000000" })],
    ["a stop without its price", pendingOrder({ order_type: "stop", limit_price: null })],
    ["unknown status", pendingOrder({ status: "expired" })],
    ["pending but closed", pendingOrder({ closed_at: "2026-11-10T12:01:00Z" })],
    ["filled without its fill", pendingOrder({ status: "filled", closed_at: "2026-11-10T12:01:00Z" })],
    ["a fill on a pending order", pendingOrder({ execution: FILL })],
    ["a malformed fill", pendingOrder({ status: "filled", closed_at: "2026-11-10T12:01:00Z", execution: { ...FILL, price: "0" } })],
    ["rejected without a reason", pendingOrder({ status: "rejected", closed_at: "2026-11-10T12:01:00Z" })],
    ["a reason that is not a code", pendingOrder({ status: "rejected", closed_at: "2026-11-10T12:01:00Z", reject_reason: "sin fondos" })],
    ["negative held cash", pendingOrder({ held_cash: "-1" })],
    ["amount as a number", pendingOrder({ amount: 1250 })],
    ["unreadable time", pendingOrder({ created_at: "ayer" })],
  ];
  for (const [motivo, valor] of malos) {
    assert.equal(parsePendingOrder(valor), null, motivo);
  }
});

test("parsePendingOrders: the list, or nothing if any row is malformed", () => {
  assert.deepEqual(parsePendingOrders({ orders: [] }), { orders: [] });
  const dos = { orders: [pendingOrder(), pendingOrder({ order_id: "f1000000-0000-0000-0000-000000000002" })] };
  assert.deepEqual(parsePendingOrders(dos), dos);
  assert.equal(parsePendingOrders({ orders: [pendingOrder(), pendingOrder({ side: "hold" })] }), null);
  assert.equal(parsePendingOrders({ orders: "none" }), null);
  assert.equal(parsePendingOrders([]), null);
});
