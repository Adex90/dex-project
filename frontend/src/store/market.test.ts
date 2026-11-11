/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { UNREADABLE_CATALOG, useMarketStore } from "./market.ts";

const market = () => useMarketStore.getState();

beforeEach(() => market().reset());

/* Fixtures */

function listing(ticker: string) {
  return {
    ticker,
    name: `${ticker} asset`,
    asset_class: "Commodity",
    price: "1612.40000000",
    change_24h_pct: "0.84",
    market_cap: null,
    volume_24h: "84300000.00",
    yield_pct: null,
    quantity_decimals: 4,
    price_source: "simulated",
  };
}

const catalog = (...tickers: string[]) => ({ assets: tickers.map(listing), as_of: "2026-11-09T12:00:00.000Z" });

/** A fetch that resolves when the test says so, recording the signal it got. */
function deferred() {
  let resolve!: (value: unknown) => void;
  let reject!: (reason: unknown) => void;
  let signal: AbortSignal | null = null;
  const promise = new Promise<unknown>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  const fetcher = (s: AbortSignal) => {
    signal = s;
    return promise;
  };
  return { fetcher, resolve, reject, signal: () => signal };
}

/* Loading */

test("load: the first load goes loading, then ready with the validated catalog", async () => {
  const pending = deferred();
  const loading = market().load(pending.fetcher);
  assert.equal(market().status, "loading");
  pending.resolve(catalog("AUG1", "STSLA"));
  await loading;
  assert.equal(market().status, "ready");
  assert.deepEqual(market().catalog?.assets.map((a) => a.ticker), ["AUG1", "STSLA"]);
  assert.equal(market().error, null);
});

test("load: a malformed catalog is an error, never shown in part", async () => {
  await market().load(async () => ({ assets: [listing("AUG1"), { ticker: "STSLA" }], as_of: "2026-11-09T12:00:00Z" }));
  assert.equal(market().status, "error");
  assert.equal(market().catalog, null);
  assert.equal(market().error?.message, UNREADABLE_CATALOG);
});

test("load: a failed first load is an error; a failed refresh keeps the list", async () => {
  await market().load(async () => {
    throw new Error("offline");
  });
  assert.equal(market().status, "error");
  assert.equal(market().error?.message, "offline");

  await market().load(async () => catalog("AUG1"));
  const shown = market().catalog;
  await market().load(async () => {
    throw new Error("still offline");
  });
  assert.equal(market().status, "ready", "what is on screen stays");
  assert.equal(market().catalog, shown);
  assert.equal(market().error?.message, "still offline");

  await market().load(async () => catalog("AUG1", "AGT1"));
  assert.equal(market().error, null, "a successful refresh clears the error");
  assert.equal(market().catalog?.assets.length, 2);
});

test("load: a newer load cancels the older one, whose answer is ignored", async () => {
  const older = deferred();
  const first = market().load(older.fetcher);
  const second = market().load(async () => catalog("STSLA"));
  assert.equal(older.signal()?.aborted, true);
  await second;
  older.resolve(catalog("AUG1"));
  await first;
  assert.deepEqual(market().catalog?.assets.map((a) => a.ticker), ["STSLA"]);
});

/* Search and reset */

test("setQuery keeps what is typed; reset clears it and cancels a load", async () => {
  market().setQuery("Tesla ");
  assert.equal(market().query, "Tesla ");
  const pending = deferred();
  const loading = market().load(pending.fetcher);
  market().reset();
  assert.equal(pending.signal()?.aborted, true);
  pending.resolve(catalog("AUG1"));
  await loading;
  assert.deepEqual(
    { catalog: market().catalog, status: market().status, error: market().error, query: market().query },
    { catalog: null, status: "idle", error: null, query: "" },
  );
});

test("setFilters keeps the screener's choices across visits; a refresh keeps them; reset clears them", async () => {
  market().setFilters({ classes: ["Commodity"], directions: ["up"] });
  await market().load(async () => catalog("AUG1", "AGT1"));
  assert.deepEqual(market().filters, { classes: ["Commodity"], directions: ["up"] }, "new prices keep the filters");
  market().reset();
  assert.deepEqual(market().filters, { classes: [], directions: [] });
});
