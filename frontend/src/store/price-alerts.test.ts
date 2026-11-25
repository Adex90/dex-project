/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { usePriceAlertsStore, visibleAlerts } from "./price-alerts.ts";

const store = () => usePriceAlertsStore.getState();
beforeEach(() => store().reset());

function alert(id: string, overrides: Record<string, unknown> = {}) {
  return {
    alert_id: id,
    ticker: "AUG1",
    direction: "above",
    target_price: "1700.00000000",
    status: "active",
    reference_price: "1612.40000000",
    triggered_price: null,
    created_at: "2026-11-24T18:03:00Z",
    triggered_at: null,
    cancelled_at: null,
    ...overrides,
  };
}

/** What `ApiError` looks like to the store (strip-only TypeScript: no parameter properties). */
class FakeApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

test("a valid list loads (cancelled ones left out); a malformed one is refused whole", async () => {
  const cancelled = alert("c", { status: "cancelled", cancelled_at: "2026-11-24T19:00:00Z" });
  await store().load(async () => ({ alerts: [alert("a"), cancelled] }));
  assert.deepEqual(
    store().alerts.map((a) => a.alert_id),
    ["a"],
  );
  store().reset();
  await store().load(async () => ({ alerts: [alert("a", { direction: "sideways" })] }));
  assert.equal(store().status, "error");
});

test("creating puts the new one first; a refusal reaches the card", async () => {
  await store().load(async () => ({ alerts: [alert("a")] }));
  const made = await store().create(async () => alert("b", { target_price: "1800.00000000" }));
  assert.equal(made.alert_id, "b");
  assert.deepEqual(
    store().alerts.map((a) => a.alert_id),
    ["b", "a"],
  );
  await assert.rejects(
    store().create(async () => {
      throw new FakeApiError(422, "AVISO_YA_ALCANZADO");
    }),
    (e: unknown) => (e as FakeApiError).code === "AVISO_YA_ALCANZADO",
  );
  assert.equal(store().alerts.length, 2);
});

test("cancel hides the row at once; a failure brings it back", async () => {
  await store().load(async () => ({ alerts: [alert("a"), alert("b")] }));
  let release!: () => void;
  const pending = store().cancel("a", () => new Promise((resolve) => (release = () => resolve({}))));
  assert.deepEqual(
    visibleAlerts(store()).map((a) => a.alert_id),
    ["b"],
  );
  release();
  await pending;
  assert.deepEqual(
    store().alerts.map((a) => a.alert_id),
    ["b"],
  );

  const reached = new FakeApiError(409, "AVISO_DISPARADO");
  await store().cancel("b", async () => {
    throw reached;
  });
  assert.deepEqual(
    visibleAlerts(store()).map((a) => a.alert_id),
    ["b"],
  );
  assert.equal(store().notice?.error, reached);
});
