/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { useOrderHistoryStore } from "./order-history.ts";

const store = () => useOrderHistoryStore.getState();
beforeEach(() => store().reset());

function fill(id: string, at: string) {
  return {
    order_id: id,
    status: "filled",
    order_type: "market",
    side: "buy",
    ticker: "AUG1",
    amount: "500.00000000",
    limit_price: null,
    stop_price: null,
    quantity: "0.31000000",
    price: "1612.40000000",
    total: "499.84000000",
    commission: "0.00000000",
    reject_reason: null,
    placed_at: at,
    closed_at: at,
    source_order_id: null,
    plan_id: null,
  };
}

test("the first page, then the next ones until the last", async () => {
  const pages: Record<string, unknown> = {
    first: { orders: [fill("e", "2026-11-24T18:05:00Z"), fill("d", "2026-11-24T18:04:00Z")], next_cursor: "c2" },
    c2: { orders: [fill("c", "2026-11-24T18:03:00Z"), fill("b", "2026-11-24T18:02:00Z")], next_cursor: "c3" },
    c3: { orders: [fill("a", "2026-11-24T18:01:00Z")], next_cursor: null },
  };
  const asked: (string | null)[] = [];
  const fetchPage = async (cursor: string | null) => {
    asked.push(cursor);
    return pages[cursor ?? "first"];
  };
  await store().load(fetchPage);
  await store().loadMore(fetchPage);
  await store().loadMore(fetchPage);
  await store().loadMore(fetchPage); // the last page was shown: nothing more to ask
  assert.deepEqual(asked, [null, "c2", "c3"]);
  assert.deepEqual(
    store().orders.map((o) => o.order_id),
    ["e", "d", "c", "b", "a"],
  );
  assert.equal(store().cursor, null);
});

test("a refresh puts new orders on top and keeps the older pages", async () => {
  await store().load(async () => ({ orders: [fill("b", "2026-11-24T18:02:00Z")], next_cursor: "older" }));
  await store().load(async () => ({
    orders: [fill("c", "2026-11-24T18:03:00Z"), fill("b", "2026-11-24T18:02:00Z")],
    next_cursor: "older",
  }));
  assert.deepEqual(
    store().orders.map((o) => o.order_id),
    ["c", "b"],
  );
  assert.equal(store().cursor, "older");
});

test("a malformed page is refused; a failed one keeps what is shown", async () => {
  await store().load(async () => ({
    orders: [{ ...fill("a", "2026-11-24T18:01:00Z"), status: "pending" }],
    next_cursor: null,
  }));
  assert.equal(store().status, "error");
  store().reset();
  await store().load(async () => ({ orders: [fill("a", "2026-11-24T18:01:00Z")], next_cursor: "next" }));
  await store().loadMore(async () => {
    throw new Error("offline");
  });
  assert.deepEqual(
    { orders: store().orders.length, loadingMore: store().loadingMore, error: store().error?.message },
    { orders: 1, loadingMore: false, error: "offline" },
  );
});
