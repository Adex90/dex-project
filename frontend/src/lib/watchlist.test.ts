/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { notHeld, parseWatchState, parseWatchlist, watchLabel } from "./watchlist.ts";

/** AUG1 as the catalog lists it (backend/tests/seguimiento.rs). */
function listing(ticker = "AUG1", overrides: Record<string, unknown> = {}) {
  return {
    ticker,
    name: "Tokenized gold, 1 g",
    asset_class: "Commodity",
    price: "1612.40000000",
    change_24h_pct: "0.84",
    market_cap: "1250000000.00",
    volume_24h: "84300000.00",
    yield_pct: null,
    quantity_decimals: 4,
    price_source: "simulated",
    ...overrides,
  };
}

const item = (ticker: string, at = "2026-11-24T18:03:00.123Z") => ({ ticker, added_at: at, asset: listing(ticker) });

test("a watchlist as the backend answers it, newest first", () => {
  const body = { items: [item("AGT1", "2026-11-24T18:05:00Z"), item("AUG1")] };
  assert.deepEqual(parseWatchlist(body), body);
  assert.deepEqual(parseWatchlist({ items: [] }), { items: [] });
});

test("a malformed watchlist is rejected whole", () => {
  for (const bad of [
    null,
    { items: "AUG1" },
    { items: [item("AUG1"), item("AUG1")] },
    { items: [{ ...item("AUG1"), ticker: "AGT1" }] },
    { items: [{ ...item("AUG1"), added_at: "yesterday" }] },
    { items: [{ ...item("AUG1"), asset: listing("AUG1", { price_source: "guess" }) }] },
    { items: [{ ...item("AUG1"), asset: listing("AUG1", { price: "0" }) }] },
  ]) {
    assert.equal(parseWatchlist(bad), null, JSON.stringify(bad));
  }
});

test("the state after a toggle: watched since a date, or not at all", () => {
  assert.deepEqual(parseWatchState({ ticker: "AUG1", watched: true, added_at: "2026-11-24T18:03:00Z" }), {
    ticker: "AUG1",
    watched: true,
    added_at: "2026-11-24T18:03:00Z",
  });
  assert.deepEqual(parseWatchState({ ticker: "AUG1", watched: false, added_at: null }), {
    ticker: "AUG1",
    watched: false,
    added_at: null,
  });
  for (const bad of [
    { ticker: "AUG1", watched: true, added_at: null },
    { ticker: "AUG1", watched: false, added_at: "2026-11-24T18:03:00Z" },
    { ticker: "AUG1", watched: "yes", added_at: null },
    { watched: true, added_at: "2026-11-24T18:03:00Z" },
  ]) {
    assert.equal(parseWatchState(bad), null, JSON.stringify(bad));
  }
});

test("the Vault shows what is followed and not held", () => {
  const items = parseWatchlist({ items: [item("AGT1"), item("AUG1"), item("STSLA")] })!.items;
  const holdings = [
    { ticker: "AUG1", shares: "0.31000000" },
    { ticker: "STSLA", shares: "0.00000000" },
  ];
  assert.deepEqual(
    notHeld(items, holdings).map((i) => i.ticker),
    ["AGT1", "STSLA"],
  );
  assert.equal(watchLabel("AUG1", false), "Add AUG1 to your watchlist");
  assert.equal(watchLabel("AUG1", true), "Remove AUG1 from your watchlist");
});
