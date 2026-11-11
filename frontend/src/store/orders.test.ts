/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import type { PendingOrder } from "../types/market.ts";
import { useOrdersStore, visibleOrders } from "./orders.ts";

const store = () => useOrdersStore.getState();

beforeEach(() => store().reset());

function order(id: string, overrides: Partial<PendingOrder> = {}): PendingOrder {
  return {
    order_id: id,
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

const A = order("f1000000-0000-0000-0000-00000000000a");
const B = order("f1000000-0000-0000-0000-00000000000b", { side: "sell", held_cash: "0.00000000", held_quantity: "0.29410000" });

/** The cancelled answer the backend sends for `o`. */
function cancelled(o: PendingOrder): PendingOrder {
  return { ...o, status: "cancelled", held_cash: "0.00000000", held_quantity: "0.00000000", closed_at: "2026-11-10T12:05:00Z" };
}

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

/** What `ApiError` looks like to the store: a status and a code (strip-only TypeScript: no parameter properties). */
class FakeApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

test("a valid list loads; a malformed one is refused whole", async () => {
  await store().load(async () => ({ orders: [A, B] }));
  assert.equal(store().status, "ready");
  assert.deepEqual(store().orders, [A, B]);

  store().reset();
  await store().load(async () => ({ orders: [A, { ...B, side: "hold" }] }));
  assert.equal(store().status, "error");
  assert.deepEqual(store().orders, []);
});

test("a failed refresh keeps what is shown and reports the error", async () => {
  await store().load(async () => ({ orders: [A] }));
  await store().load(async () => {
    throw new Error("offline");
  });
  assert.equal(store().status, "ready");
  assert.equal(store().error?.message, "offline");
  assert.deepEqual(store().orders, [A]);
});

test("a newer load supersedes one still in flight", async () => {
  const slow = deferred<unknown>();
  const first = store().load(() => slow.promise);
  await store().load(async () => ({ orders: [B] }));
  slow.resolve({ orders: [A] });
  await first;
  assert.deepEqual(store().orders, [B], "the older answer is ignored");
});

test("cancel is optimistic: the row leaves at once, and for good on success", async () => {
  await store().load(async () => ({ orders: [A, B] }));
  const answer = deferred<unknown>();
  const done = store().cancel(A.order_id, () => answer.promise);
  assert.deepEqual(visibleOrders(store()), [B], "hidden while in flight");
  assert.deepEqual(store().orders, [A, B], "still known until the server answers");
  // A second tap while it is in flight does nothing.
  let calls = 0;
  await store().cancel(A.order_id, async () => {
    calls += 1;
  });
  assert.equal(calls, 0);
  answer.resolve(cancelled(A));
  await done;
  assert.deepEqual(store().orders, [B]);
  assert.deepEqual(store().cancelling, []);
  assert.deepEqual(store().notice, { kind: "cancelled", order: A });
});

test("a failed cancel brings the order back and says why", async () => {
  await store().load(async () => ({ orders: [A, B] }));
  const error = new FakeApiError(503, "REINTENTAR");
  await store().cancel(A.order_id, async () => {
    throw error;
  });
  assert.deepEqual(visibleOrders(store()), [A, B]);
  assert.deepEqual(store().notice, { kind: "failed", order: A, error });
});

test("an order that filled or was rejected meanwhile leaves the list (409)", async () => {
  await store().load(async () => ({ orders: [A, B] }));
  await store().cancel(B.order_id, async () => {
    throw new FakeApiError(409, "ORDEN_NO_PENDIENTE");
  });
  assert.deepEqual(store().orders, [A]);
  assert.deepEqual(store().notice, { kind: "gone", order: B });
});

test("a load that started before a cancel cannot bring the cancelled order back", async () => {
  await store().load(async () => ({ orders: [A, B] }));
  const stale = deferred<unknown>();
  const loading = store().load(() => stale.promise);
  await store().cancel(A.order_id, async () => cancelled(A));
  stale.resolve({ orders: [A, B] });
  await loading;
  assert.deepEqual(store().orders, [B]);
});

test("a cancel for an order not on the list does nothing; reset forgets everything", async () => {
  let calls = 0;
  await store().cancel(A.order_id, async () => {
    calls += 1;
  });
  assert.equal(calls, 0);
  await store().load(async () => ({ orders: [A] }));
  await store().cancel(A.order_id, async () => cancelled(A));
  store().reset();
  assert.deepEqual(store().orders, []);
  assert.equal(store().status, "idle");
  // A new session (another user, the same id in a test) lists it again.
  await store().load(async () => ({ orders: [A] }));
  assert.deepEqual(store().orders, [A]);
  store().dismissNotice();
  assert.equal(store().notice, null);
});
