/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import type { PlanAction, SavingsPlan } from "../types/market.ts";
import { useSavingsPlansStore, visiblePlans } from "./savings-plans.ts";

const store = () => useSavingsPlansStore.getState();

beforeEach(() => store().reset());

function plan(id: string, overrides: Partial<SavingsPlan> = {}): SavingsPlan {
  return {
    plan_id: id,
    order_type: "savings_plan",
    status: "active",
    ticker: "AUG1",
    side: "buy",
    amount: "500.00000000",
    frequency: "weekly",
    anchor_date: "2026-11-24",
    next_execution_at: "2026-12-01T15:00:00Z",
    upcoming: ["2026-12-01T15:00:00Z", "2026-12-08T15:00:00Z", "2026-12-15T15:00:00Z"],
    last_execution_at: "2026-11-24T18:03:00Z",
    last_result: "executed",
    failure_reason: null,
    last_purchase: {
      order_id: "0e000000-0000-0000-0000-000000000001",
      quantity: "0.31000000",
      price: "1612.40000000",
      amount: "499.84000000",
      commission: "0.00000000",
      executed_at: "2026-11-24T18:03:00Z",
    },
    purchases: 1,
    skipped: 0,
    invested: "499.84000000",
    quantity: "0.31000000",
    created_at: "2026-11-24T18:02:59Z",
    cancelled_at: null,
    ...overrides,
  };
}

const A = plan("9a000000-0000-0000-0000-00000000000a");
const B = plan("9a000000-0000-0000-0000-00000000000b", {
  ticker: "CETES28",
  amount: "1000.00000000",
  frequency: "monthly",
});
const PAUSED = { status: "paused" as const, next_execution_at: null, upcoming: [] };

/** The answer the backend sends for `p` after `action`. */
function answered(p: SavingsPlan, action: PlanAction): SavingsPlan {
  switch (action) {
    case "pause":
      return { ...p, ...PAUSED };
    case "resume":
      return { ...p, status: "active", next_execution_at: "2026-12-08T15:00:00Z", upcoming: ["2026-12-08T15:00:00Z"] };
    case "cancel":
      return { ...p, ...PAUSED, status: "cancelled", cancelled_at: "2026-11-25T10:00:00Z" };
  }
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
  await store().load(async () => ({ plans: [A, B] }));
  assert.equal(store().status, "ready");
  assert.deepEqual(store().plans, [A, B]);

  store().reset();
  await store().load(async () => ({ plans: [A, { ...B, side: "sell" }] }));
  assert.equal(store().status, "error");
  assert.deepEqual(store().plans, []);
});

test("a failed refresh keeps what is shown and reports the error", async () => {
  await store().load(async () => ({ plans: [A] }));
  await store().load(async () => {
    throw new Error("offline");
  });
  assert.equal(store().status, "ready");
  assert.equal(store().error?.message, "offline");
  assert.deepEqual(store().plans, [A]);
});

test("pausing shows at once and takes the server's answer", async () => {
  await store().load(async () => ({ plans: [A, B] }));
  const call = deferred<unknown>();
  let asked: [string, PlanAction] | null = null;
  const done = store().act(A.plan_id, "pause", (id, action) => {
    asked = [id, action];
    return call.promise;
  });
  assert.deepEqual(asked, [A.plan_id, "pause"]);
  // Paused on screen before the server answers.
  assert.deepEqual(visiblePlans(store())[0], { ...A, ...PAUSED });
  // A second action on the same plan waits for the first.
  await store().act(A.plan_id, "cancel", async () => assert.fail("not while pausing"));
  call.resolve(answered(A, "pause"));
  await done;
  assert.deepEqual(store().plans, [answered(A, "pause"), B]);
  assert.deepEqual(store().changing, {});
  assert.deepEqual(store().notice, { kind: "done", action: "pause", plan: answered(A, "pause") });
});

test("resuming takes its next date from the server", async () => {
  const paused = answered(A, "pause");
  await store().load(async () => ({ plans: [paused] }));
  const call = deferred<unknown>();
  const done = store().act(A.plan_id, "resume", () => call.promise);
  // Active at once; its dates are still to come.
  assert.equal(visiblePlans(store())[0]?.status, "active");
  assert.equal(visiblePlans(store())[0]?.next_execution_at, null);
  call.resolve(answered(paused, "resume"));
  await done;
  assert.equal(store().plans[0]?.next_execution_at, "2026-12-08T15:00:00Z");
});

test("cancelling hides the row at once and for good", async () => {
  await store().load(async () => ({ plans: [A, B] }));
  const slowLoad = deferred<unknown>();
  // A load that started before the cancellation settled...
  const loading = store().load(() => slowLoad.promise);
  await store().act(A.plan_id, "cancel", async () => answered(A, "cancel"));
  assert.deepEqual(visiblePlans(store()), [B]);
  assert.deepEqual(store().notice, { kind: "done", action: "cancel", plan: answered(A, "cancel") });
  // ...cannot bring it back.
  slowLoad.resolve({ plans: [A, B] });
  await loading;
  assert.deepEqual(store().plans, [B]);
  // Nor can a later one that still lists it (a replica behind).
  await store().load(async () => ({ plans: [A, B] }));
  assert.deepEqual(store().plans, [B]);
});

test("a plan cancelled elsewhere leaves the list; any other failure brings the row back", async () => {
  await store().load(async () => ({ plans: [A, B] }));
  await store().act(A.plan_id, "pause", async () => {
    throw new FakeApiError(409, "PLAN_CANCELADO");
  });
  assert.deepEqual(store().plans, [B]);
  assert.deepEqual(store().notice, { kind: "gone", plan: A });

  const offline = new Error("offline");
  await store().act(B.plan_id, "pause", async () => {
    throw offline;
  });
  assert.deepEqual(store().plans, [B]);
  assert.deepEqual(visiblePlans(store()), [B]);
  assert.deepEqual(store().notice, { kind: "failed", action: "pause", plan: B, error: offline });
  store().dismissNotice();
  assert.equal(store().notice, null);
});

test("an answer that cannot be read still counts: the action happened", async () => {
  await store().load(async () => ({ plans: [A, B] }));
  await store().act(A.plan_id, "pause", async () => ({ garbled: true }));
  assert.deepEqual(store().plans[0], { ...A, ...PAUSED });
  await store().act(B.plan_id, "cancel", async () => null);
  assert.deepEqual(
    store().plans.map((p) => p.plan_id),
    [A.plan_id],
  );
});

test("unknown plans are ignored, cancelled ones never listed, and reset clears everything", async () => {
  await store().load(async () => ({ plans: [A, answered(B, "cancel")] }));
  assert.deepEqual(store().plans, [A]);
  await store().act("9a000000-0000-0000-0000-0000000000ff", "pause", async () => assert.fail("unknown plan"));
  store().reset();
  assert.deepEqual(
    { plans: store().plans, status: store().status, changing: store().changing, notice: store().notice },
    { plans: [], status: "idle", changing: {}, notice: null },
  );
});
