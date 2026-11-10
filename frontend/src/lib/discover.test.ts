/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { normalize, searchCatalog, trending } from "./discover.ts";
import type { MarketListing } from "../types/market.ts";

/** The backend's catalog (migrations 0013 and 0014), by ticker as it arrives. */
function listing(ticker: string, name: string, asset_class: string, volume: string | null): MarketListing {
  return {
    ticker,
    name,
    asset_class,
    price: "1.00000000",
    change_24h_pct: "0.00",
    market_cap: null,
    volume_24h: volume,
    yield_pct: null,
    quantity_decimals: 4,
    price_source: "simulated",
  };
}

const CATALOG: MarketListing[] = [
  listing("AGT1", "Tokenized silver, 1 oz", "Commodity", "12800000.00"),
  listing("AUG1", "Tokenized gold, 1 g", "Commodity", "84300000.00"),
  listing("CETES28", "CETES, 28 days", "Fixed income", "18400000.00"),
  listing("SCT28", "Synthetic T-bill, 28 days", "Fixed income", "5200000.00"),
  listing("STSLA", "Synthetic Tesla, 1 share", "Equity", "96500000.00"),
];

const tickers = (assets: readonly MarketListing[]) => assets.map((a) => a.ticker);

test("normalize: case, accents and spacing never matter", () => {
  assert.equal(normalize("  Tokenized   GOLD "), "tokenized gold");
  assert.equal(normalize("CETES a 28 días"), "cetes a 28 dias");
  assert.equal(normalize("Señor\tÁGUILA"), "senor aguila");
  assert.equal(normalize(""), "");
});

test("trending: the most traded first, unknown volume last, ties by ticker", () => {
  assert.deepEqual(tickers(trending(CATALOG)), ["STSLA", "AUG1", "CETES28", "AGT1", "SCT28"]);
  const ties = [listing("BBB", "B", "Fund", null), listing("AAA", "A", "Fund", null), listing("CCC", "C", "Fund", "1")];
  assert.deepEqual(tickers(trending(ties)), ["CCC", "AAA", "BBB"]);
  assert.deepEqual(tickers(CATALOG), ["AGT1", "AUG1", "CETES28", "SCT28", "STSLA"], "the input is left as it was");
  // Exact decimals, not floats: these differ only in the eighth decimal.
  const close = [listing("LOW", "Low", "Fund", "0.00000001"), listing("HIGH", "High", "Fund", "0.00000002")];
  assert.deepEqual(tickers(trending(close)), ["HIGH", "LOW"]);
});

test("searchCatalog: an empty query is the whole catalog, trending", () => {
  for (const query of ["", "   ", "\t"]) {
    assert.deepEqual(tickers(searchCatalog(CATALOG, query)), ["STSLA", "AUG1", "CETES28", "AGT1", "SCT28"], query);
  }
});

test("searchCatalog: by ticker, name or asset class, case and accents aside", () => {
  const cases: [string, string[]][] = [
    ["gold", ["AUG1"]],
    ["GOLD", ["AUG1"]],
    ["aug", ["AUG1"]],
    ["tesla", ["STSLA"]],
    ["TSLA", ["STSLA"]],
    ["sTSLA", ["STSLA"]],
    ["cetes", ["CETES28"]],
    ["cétes", ["CETES28"]],
    ["fixed income", ["CETES28", "SCT28"]],
    ["commodity", ["AUG1", "AGT1"]],
    ["silver oz", ["AGT1"]],
    ["28 days", ["CETES28", "SCT28"]],
    ["synthetic", ["STSLA", "SCT28"]],
    ["gold silver", []],
    ["bitcoin", []],
  ];
  for (const [query, expected] of cases) {
    assert.deepEqual(tickers(searchCatalog(CATALOG, query)), expected, query);
  }
});

test("searchCatalog: the ticker ranks first, then a word of the name, then the rest", () => {
  const assets = [
    listing("XGOLD", "Gold miners fund", "Fund", "1.00"),
    listing("GOLDX", "Something else", "Fund", "2.00"),
    listing("AUG1", "Tokenized gold, 1 g", "Commodity", "9.00"),
    listing("MARI", "Marigold Bank", "Equity", "8.00"),
    listing("GOLD", "Plain gold", "Commodity", "0.50"),
  ];
  // GOLD is the query; GOLDX starts with it; AUG1 and XGOLD have "gold" as a
  // word of the name (trending order between them); MARI only contains it.
  assert.deepEqual(tickers(searchCatalog(assets, "gold")), ["GOLD", "GOLDX", "AUG1", "XGOLD", "MARI"]);
});
