/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  COLLECTIONS,
  NO_FILTERS,
  applyCollection,
  classLabel,
  classOptions,
  collectionStats,
  describeFilters,
  direction,
  facetCounts,
  isCollectionActive,
  isFiltered,
  screen,
  toggleClass,
  toggleDirection,
  topMovers,
  type ScreenerFilters,
} from "./screener.ts";
import type { MarketListing } from "../types/market.ts";

function listing(ticker: string, asset_class: string, change: string, volume: string | null): MarketListing {
  return {
    ticker,
    name: `${ticker} asset`,
    asset_class,
    price: "1.00000000",
    change_24h_pct: change,
    market_cap: null,
    volume_24h: volume,
    yield_pct: null,
    quantity_decimals: 4,
    price_source: "simulated",
  };
}

/** The backend's catalog (migrations 0013 and 0014): classes, 24-hour changes and volumes as served. */
const CATALOG: MarketListing[] = [
  listing("AGT1", "Commodity", "-0.52", "12800000.00"),
  listing("AUG1", "Commodity", "0.84", "84300000.00"),
  listing("CETES28", "Fixed income", "0.02", "18400000.00"),
  listing("SCT28", "Fixed income", "0.00", "5200000.00"),
  listing("STSLA", "Equity", "-2.40", "96500000.00"),
];

const tickers = (assets: readonly MarketListing[]) => assets.map((a) => a.ticker);
const filters = (classes: string[] = [], directions: ("up" | "down")[] = []): ScreenerFilters => ({
  classes,
  directions,
});
const collection = (id: string) => COLLECTIONS.find((c) => c.id === id)!;

test("classOptions: equities, commodities and fixed income always; funds and unknown classes once listed", () => {
  assert.deepEqual(classOptions(CATALOG), [
    { value: "Equity", label: "Equities" },
    { value: "Commodity", label: "Commodities" },
    { value: "Fixed income", label: "Fixed income" },
  ]);
  assert.deepEqual(
    classOptions([]).map((o) => o.label),
    ["Equities", "Commodities", "Fixed income"],
    "offered before the catalog lists any",
  );
  assert.deepEqual(
    classOptions([...CATALOG, listing("ETF1", "Fund", "0.10", null), listing("BTC1", "Crypto", "1.00", null)]).map(
      (o) => o.label,
    ),
    ["Equities", "Commodities", "Fixed income", "Funds", "Crypto"],
  );
  assert.equal(classLabel("Commodity"), "Commodities");
  assert.equal(classLabel("Crypto"), "Crypto");
});

test("direction: the 24-hour change as the list prints it, so 0.00% is neither way", () => {
  assert.equal(direction({ change_24h_pct: "0.84" }), "up");
  assert.equal(direction({ change_24h_pct: "-2.40" }), "down");
  assert.equal(direction({ change_24h_pct: "0.00" }), "flat");
  assert.equal(direction({ change_24h_pct: "0.004" }), "flat", "prints 0.00%");
  assert.equal(direction({ change_24h_pct: "-0.004" }), "flat");
  assert.equal(direction({ change_24h_pct: "0.005" }), "flat", "half-even: 0.005 prints 0.00%");
  assert.equal(direction({ change_24h_pct: "0.015" }), "up", "half-even: 0.015 prints 0.02%");
});

test("screen: classes add up, dimensions narrow, order kept; flat changes pass no direction", () => {
  assert.deepEqual(tickers(screen(CATALOG, NO_FILTERS)), tickers(CATALOG));
  assert.deepEqual(tickers(screen(CATALOG, filters(["Commodity"]))), ["AGT1", "AUG1"]);
  assert.deepEqual(tickers(screen(CATALOG, filters(["Commodity", "Equity"]))), ["AGT1", "AUG1", "STSLA"]);
  assert.deepEqual(tickers(screen(CATALOG, filters([], ["up"]))), ["AUG1", "CETES28"]);
  assert.deepEqual(tickers(screen(CATALOG, filters([], ["down"]))), ["AGT1", "STSLA"]);
  assert.deepEqual(tickers(screen(CATALOG, filters([], ["up", "down"]))), ["AGT1", "AUG1", "CETES28", "STSLA"]);
  assert.deepEqual(tickers(screen(CATALOG, filters(["Fixed income"], ["up"]))), ["CETES28"], "SCT28 is flat");
  assert.deepEqual(tickers(screen(CATALOG, filters(["Equity"], ["up"]))), []);
  assert.equal(isFiltered(NO_FILTERS), false);
  assert.equal(isFiltered(filters([], ["down"])), true);
});

test("facetCounts: each choice counts what it would show with the other dimension's choices", () => {
  assert.deepEqual(facetCounts(CATALOG, NO_FILTERS), {
    classes: { Commodity: 2, "Fixed income": 2, Equity: 1 },
    directions: { up: 2, down: 2 },
  });
  assert.deepEqual(facetCounts(CATALOG, filters([], ["up"])), {
    classes: { Commodity: 1, "Fixed income": 1 },
    directions: { up: 2, down: 2 },
  });
  assert.deepEqual(facetCounts(CATALOG, filters(["Commodity"])), {
    classes: { Commodity: 2, "Fixed income": 2, Equity: 1 },
    directions: { up: 1, down: 1 },
  });
  assert.deepEqual(facetCounts([], NO_FILTERS), { classes: {}, directions: { up: 0, down: 0 } });
});

test("toggles turn a choice on and off; describeFilters says them in the screener's order", () => {
  let state = toggleClass(NO_FILTERS, "Fixed income");
  state = toggleClass(state, "Commodity");
  state = toggleDirection(state, "up");
  assert.deepEqual(state, filters(["Fixed income", "Commodity"], ["up"]));
  assert.equal(describeFilters(state), "Commodities, Fixed income · Positive 24h");
  assert.deepEqual(toggleClass(state, "Fixed income"), filters(["Commodity"], ["up"]));
  assert.deepEqual(toggleDirection(state, "up"), filters(["Fixed income", "Commodity"]));
  assert.equal(describeFilters(filters([], ["down", "up"])), "Positive or Negative 24h");
  assert.equal(describeFilters(filters(["Crypto", "Equity"])), "Equities, Crypto");
  assert.equal(describeFilters(NO_FILTERS), "");
});

test("collections preset the classes: picked, switched, picked again to clear; the direction stays", () => {
  const hard = collection("hard-assets");
  const fixed = collection("fixed-income");
  const picked = applyCollection(filters([], ["up"]), hard);
  assert.deepEqual(picked, filters(["Commodity"], ["up"]));
  assert.equal(isCollectionActive(picked, hard), true);
  assert.equal(isCollectionActive(picked, fixed), false);
  assert.deepEqual(applyCollection(picked, fixed), filters(["Fixed income"], ["up"]), "switching replaces the classes");
  assert.deepEqual(applyCollection(picked, hard), filters([], ["up"]), "picking the active one clears them");
  assert.equal(
    isCollectionActive(toggleClass(picked, "Equity"), hard),
    false,
    "another class on: no longer the collection",
  );
  assert.equal(isCollectionActive(filters(["Commodity"]), hard), true, "the toggles alone can match a collection");
  assert.deepEqual(
    COLLECTIONS.map((c) => c.title),
    ["Hard Assets", "Synthetic Equities", "Fixed Income"],
  );
});

test("collectionStats: the assets, most traded first, and the equal-weight change, exact", () => {
  const hard = collectionStats(CATALOG, collection("hard-assets"));
  assert.deepEqual(tickers(hard.members), ["AUG1", "AGT1"]);
  assert.equal(hard.change, "0.16000000", "(0.84 − 0.52) / 2");
  assert.equal(collectionStats(CATALOG, collection("fixed-income")).change, "0.01000000", "(0.02 + 0.00) / 2");
  assert.equal(collectionStats(CATALOG, collection("synthetic-equities")).change, "-2.40000000");
  const thirds = [
    listing("A", "Commodity", "0.01", null),
    listing("B", "Commodity", "0.02", null),
    listing("C", "Commodity", "0.02", null),
  ];
  assert.equal(collectionStats(thirds, collection("hard-assets")).change, "0.01666667", "0.05 / 3, half-even");
  const down = [
    listing("A", "Equity", "-0.01", null),
    listing("B", "Equity", "-0.02", null),
    listing("C", "Equity", "-0.02", null),
  ];
  assert.equal(collectionStats(down, collection("synthetic-equities")).change, "-0.01666667");
  assert.deepEqual(collectionStats(CATALOG.slice(0, 2), collection("fixed-income")), { members: [], change: null });
});

test("topMovers: the largest rise and fall in the whole catalog, ties to the more traded", () => {
  const { gainer, loser } = topMovers(CATALOG);
  assert.equal(gainer?.ticker, "AUG1");
  assert.equal(loser?.ticker, "STSLA");
  const tie = [listing("LOW", "Equity", "1.50", "100.00"), listing("HIGH", "Equity", "1.50", "900.00")];
  assert.equal(topMovers(tie).gainer?.ticker, "HIGH", "on a tie, the more traded");
  assert.equal(topMovers(tie).loser, null, "nothing fell");
  const flat = [listing("A", "Equity", "0.004", "1.00"), listing("B", "Equity", "-0.004", "1.00")];
  assert.deepEqual(topMovers(flat), { gainer: null, loser: null }, "a change that prints 0.00% moves nothing");
  assert.equal(
    topMovers([listing("A", "Equity", "-0.10", "1.00"), listing("B", "Equity", "-3.00", "1.00")]).loser?.ticker,
    "B",
  );
  assert.deepEqual(topMovers([]), { gainer: null, loser: null });
});
