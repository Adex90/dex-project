/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import type { MarketListing } from "../types/market.ts";
import { isWatched, useWatchlistStore } from "./watchlist.ts";

const store = () => useWatchlistStore.getState();
beforeEach(() => store().reset());

function listing(ticker: string): MarketListing {
  return {
    ticker,
    name: `${ticker} name`,
    asset_class: "Commodity",
    price: "1612.40000000",
    change_24h_pct: "0.84",
    market_cap: null,
    volume_24h: null,
    yield_pct: null,
    quantity_decimals: 4,
    price_source: "simulated",
  };
}
const item = (ticker: string) => ({ ticker, added_at: "2026-11-24T18:03:00Z", asset: listing(ticker) });

/** A promise settled from the outside. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a valid list loads; a malformed one is refused whole", async () => {
  await store().load(async () => ({ items: [item("AUG1")] }));
  assert.equal(store().status, "ready");
  assert.equal(isWatched(store(), "AUG1"), true);
  store().reset();
  await store().load(async () => ({ items: [item("AUG1"), item("AUG1")] }));
  assert.equal(store().status, "error");
});

test("the star turns at once; the server's answer settles it", async () => {
  await store().load(async () => ({ items: [item("AUG1")] }));
  const call = deferred<unknown>();
  let asked: [string, boolean] | null = null;
  const done = store().set(
    "AGT1",
    true,
    (ticker, watched) => {
      asked = [ticker, watched];
      return call.promise;
    },
    listing("AGT1"),
  );
  assert.deepEqual(asked, ["AGT1", true], "it asks for the state, never a blind flip");
  assert.equal(isWatched(store(), "AGT1"), true, "followed at once");
  call.resolve({ ticker: "AGT1", watched: true, added_at: "2026-11-24T18:05:00Z" });
  await done;
  assert.deepEqual(
    store().items.map((i) => [i.ticker, i.added_at]),
    [
      ["AGT1", "2026-11-24T18:05:00Z"],
      ["AUG1", "2026-11-24T18:03:00Z"],
    ],
  );
  const unfollowed = async () => ({ ticker: "AUG1", watched: false, added_at: null });
  await store().set("AUG1", false, unfollowed, listing("AUG1"));
  assert.deepEqual(
    store().items.map((i) => i.ticker),
    ["AGT1"],
  );
  assert.deepEqual(store().pending, {});
});

test("a failure restores the star and says so; a stale load cannot undo a change", async () => {
  await store().load(async () => ({ items: [item("AUG1")] }));
  const offline = new Error("offline");
  const failing = async () => {
    throw offline;
  };
  await store().set("AUG1", false, failing, listing("AUG1"));
  assert.equal(isWatched(store(), "AUG1"), true);
  assert.deepEqual(store().notice, { ticker: "AUG1", watched: false, error: offline });

  const slow = deferred<unknown>();
  const loading = store().load(() => slow.promise);
  await store().set("AUG1", false, async () => ({ ticker: "AUG1", watched: false, added_at: null }), listing("AUG1"));
  slow.resolve({ items: [item("AUG1")] });
  await loading;
  assert.equal(isWatched(store(), "AUG1"), false);
});
