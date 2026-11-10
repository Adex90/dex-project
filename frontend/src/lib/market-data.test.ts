/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  assetPath,
  isTicker,
  parseMarketCatalog,
  parseMarketQuote,
  parseOrderReceipt,
  positionFor,
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
  assert.deepEqual(parseOrderReceipt(receipt({ side: "sell", status: "pending" })), receipt({ side: "sell", status: "pending" }));
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
    ["empty order id", receipt({ order_id: "" })],
    ["unreadable time", receipt({ executed_at: "now" })],
  ];
  for (const [motivo, valor] of malos) {
    assert.equal(parseOrderReceipt(valor), null, motivo);
  }
});

test("positionFor: the holding's shares, or 0", () => {
  const holdings: Holding[] = [
    {
      ticker: "AUG1",
      name: "Tokenized gold, 1 g",
      shares: "0.77520000",
      current_price: "1612.40000000",
      total_value: "1249.93248000",
      daily_change_pct: "0.84",
    },
  ];
  assert.equal(positionFor(holdings, "AUG1"), "0.77520000");
  assert.equal(positionFor(holdings, "AGT1"), "0");
  assert.equal(positionFor([], "AUG1"), "0");
});

test("isTicker and assetPath", () => {
  for (const bueno of ["AUG1", "SCT28", "A", "ABCDEFGHIJKL"]) assert.ok(isTicker(bueno), bueno);
  for (const malo of ["", "aug1", "1AUG", "AU-G1", "ABCDEFGHIJKLM", " AUG1", 7]) assert.ok(!isTicker(malo), String(malo));
  assert.equal(assetPath("AUG1"), "/dashboard/asset/AUG1");
  assert.equal(assetPath("A/B"), "/dashboard/asset/A%2FB");
});
