/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  FREQUENCY_LABELS,
  actionSentence,
  applyAction,
  formatPlanDate,
  formatPlanDay,
  lastPurchaseSentence,
  monthlyTotal,
  nextPurchaseLabel,
  parseSavingsPlan,
  parseSavingsPlans,
  planCadence,
  plansCount,
  skipReason,
} from "./savings-plans.ts";
import type { SavingsPlan } from "../types/market.ts";

/** Tuesday, Nov 24, 2026, 12:03 p.m. in Mexico City. */
const NOW = Date.parse("2026-11-24T18:03:30Z");

/** The backend's answer for a weekly 500 MXN plan of AUG1, its first purchase made (backend/tests/planes.rs). */
function plan(overrides: Record<string, unknown> = {}) {
  return {
    plan_id: "9a000000-0000-0000-0000-000000000001",
    order_type: "savings_plan",
    status: "active",
    ticker: "AUG1",
    side: "buy",
    amount: "500.00000000",
    frequency: "weekly",
    anchor_date: "2026-11-24",
    next_execution_at: "2026-12-01T15:00:00Z",
    upcoming: ["2026-12-01T15:00:00Z", "2026-12-08T15:00:00Z", "2026-12-15T15:00:00Z"],
    last_execution_at: "2026-11-24T18:03:00.123456Z",
    last_result: "executed",
    failure_reason: null,
    last_purchase: {
      order_id: "0e000000-0000-0000-0000-000000000001",
      quantity: "0.31000000",
      price: "1612.40000000",
      amount: "499.84000000",
      commission: "0.00000000",
      executed_at: "2026-11-24T18:03:00.123456Z",
    },
    purchases: 1,
    skipped: 0,
    invested: "499.84000000",
    quantity: "0.31000000",
    created_at: "2026-11-24T18:02:59.987654Z",
    cancelled_at: null,
    ...overrides,
  };
}

function parsed(overrides: Record<string, unknown> = {}): SavingsPlan {
  const p = parseSavingsPlan(plan(overrides));
  assert.ok(p, JSON.stringify(overrides));
  return p;
}

test("a plan as the backend answers it", () => {
  assert.deepEqual(parseSavingsPlan(plan()), plan());
});

test("paused, cancelled, skipped and not yet bought are all coherent plans", () => {
  const paused = { status: "paused", next_execution_at: null, upcoming: [] };
  assert.ok(parseSavingsPlan(plan(paused)));
  assert.ok(parseSavingsPlan(plan({ ...paused, status: "cancelled", cancelled_at: "2026-11-25T10:00:00Z" })));
  assert.ok(
    parseSavingsPlan(plan({ last_result: "skipped", failure_reason: "SALDO_INSUFICIENTE", skipped: 1, purchases: 1 })),
  );
  // Created a moment ago: its first purchase is due and not yet made.
  assert.ok(
    parseSavingsPlan(
      plan({
        next_execution_at: "2026-11-24T18:02:59.987654Z",
        upcoming: ["2026-11-24T18:02:59.987654Z", "2026-12-01T15:00:00Z", "2026-12-08T15:00:00Z"],
        last_execution_at: null,
        last_result: null,
        last_purchase: null,
        purchases: 0,
        invested: "0.00000000",
        quantity: "0.00000000",
      }),
    ),
  );
});

test("a malformed or incoherent plan is rejected whole", () => {
  for (const bad of [
    null,
    [],
    "plan",
    { ...plan(), order_type: "market" },
    { ...plan(), side: "sell" },
    { ...plan(), status: "running" },
    { ...plan(), frequency: "daily" },
    { ...plan(), ticker: "aug1" },
    { ...plan(), amount: "0" },
    { ...plan(), amount: 500 },
    { ...plan(), anchor_date: "24/11/2026" },
    { ...plan(), next_execution_at: "next week" },
    { ...plan(), upcoming: "2026-12-01T15:00:00Z" },
    { ...plan(), upcoming: ["2026-12-08T15:00:00Z", "2026-12-01T15:00:00Z"] },
    { ...plan(), failure_reason: "no había dinero" },
    { ...plan(), purchases: -1 },
    { ...plan(), purchases: 1.5 },
    { ...plan(), invested: "-1" },
    { ...plan(), last_purchase: { order_id: "x" } },
    { ...plan(), created_at: 1764007379 },
    // Only an active plan has dates ahead, starting with the next one.
    { ...plan(), next_execution_at: null },
    { ...plan(), upcoming: [] },
    { ...plan(), upcoming: ["2026-12-08T15:00:00Z"] },
    { ...plan(), status: "paused" },
    { ...plan(), status: "paused", next_execution_at: null },
    // Cancelled says when; nothing else does.
    { ...plan(), status: "cancelled", next_execution_at: null, upcoming: [] },
    { ...plan(), cancelled_at: "2026-11-25T10:00:00Z" },
    // The last result agrees with its time, its reason and the counts.
    { ...plan(), last_execution_at: null },
    { ...plan(), last_result: null },
    { ...plan(), failure_reason: "SALDO_INSUFICIENTE" },
    { ...plan(), last_result: "skipped" },
    { ...plan(), last_result: "skipped", failure_reason: "SALDO_INSUFICIENTE" },
    { ...plan(), purchases: 0 },
    { ...plan(), last_purchase: null },
  ]) {
    assert.equal(parseSavingsPlan(bad), null, JSON.stringify(bad));
  }
});

test("the list: every plan valid, none twice", () => {
  const other = plan({ plan_id: "9a000000-0000-0000-0000-000000000002", frequency: "monthly" });
  assert.deepEqual(parseSavingsPlans({ plans: [plan(), other] }), { plans: [plan(), other] });
  assert.deepEqual(parseSavingsPlans({ plans: [] }), { plans: [] });
  assert.equal(parseSavingsPlans({ plans: [plan(), plan()] }), null);
  assert.equal(parseSavingsPlans({ plans: [plan(), { ...plan(), side: "sell" }] }), null);
  assert.equal(parseSavingsPlans({ plans: plan() }), null);
  assert.equal(parseSavingsPlans(null), null);
});

test("the dates, in Mexico City where the calendar lives", () => {
  // 15:00 UTC is 9:00 a.m. in Mexico City (UTC−6 all year).
  assert.equal(formatPlanDate("2026-12-01T15:00:00Z", NOW), "Tue, Dec 1 · 9:00 AM");
  assert.equal(formatPlanDate("2027-01-05T15:00:00Z", NOW), "Tue, Jan 5, 2027 · 9:00 AM");
  assert.equal(formatPlanDate("2026-11-24T18:03:00Z", NOW), "Tue, Nov 24 · 12:03 PM");
  // Late at night in UTC is still the evening before in Mexico City.
  assert.equal(formatPlanDay("2026-12-02T03:30:00Z"), "Dec 1");
  assert.equal(nextPurchaseLabel(parsed(), NOW), "Tue, Dec 1 · 9:00 AM");
  assert.equal(nextPurchaseLabel({ next_execution_at: "2026-11-24T18:02:59Z" }, NOW), "In a moment");
  assert.equal(nextPurchaseLabel({ next_execution_at: null }, NOW), null);
});

test("the cadence, the counts and what the active plans add up to in a month", () => {
  assert.equal(planCadence(parsed()), "$500.00 every week");
  assert.equal(planCadence({ amount: "1250.50", frequency: "biweekly" }), "$1,250.50 every two weeks");
  assert.deepEqual(FREQUENCY_LABELS, { weekly: "Weekly", biweekly: "Every 2 weeks", monthly: "Monthly" });
  // 500 weekly is 2,166.67 a month; 1,000 monthly, 1,000; a paused plan adds nothing.
  const plans = [
    { status: "active" as const, amount: "500.00", frequency: "weekly" as const },
    { status: "active" as const, amount: "1000.00", frequency: "monthly" as const },
    { status: "paused" as const, amount: "300.00", frequency: "biweekly" as const },
  ];
  assert.equal(monthlyTotal(plans), "3166.67000000");
  assert.equal(monthlyTotal([]), "0");
  assert.equal(plansCount(plans), "2 active · 1 paused");
  assert.equal(plansCount(plans.slice(2)), "1 paused");
  assert.equal(plansCount([]), "No savings plans");
});

test("the last purchase, made or skipped", () => {
  assert.equal(lastPurchaseSentence(parsed(), 4), "Bought 0.31 AUG1 at $1,612.40 on Nov 24");
  const skipped = parsed({
    last_result: "skipped",
    failure_reason: "SALDO_INSUFICIENTE",
    last_execution_at: "2026-12-01T15:00:01Z",
    skipped: 1,
  });
  assert.equal(lastPurchaseSentence(skipped), "Skipped on Dec 1: not enough available cash");
  assert.equal(
    lastPurchaseSentence({
      ticker: "AUG1",
      last_result: null,
      last_execution_at: null,
      failure_reason: null,
      last_purchase: null,
    }),
    null,
  );
});

test("why a purchase was skipped: what a client can act on, otherwise nothing about why", () => {
  assert.equal(skipReason("SALDO_INSUFICIENTE"), "not enough available cash");
  assert.equal(skipReason("MERCADO_CERRADO"), "the market was closed");
  assert.equal(skipReason("MONTO_INSUFICIENTE"), "the amount was below one unit at that price");
  const quiet = "the purchase was not available at that time";
  for (const code of ["OPERACION_NO_DISPONIBLE", "CUENTA_CONGELADA", "CLIENTE_EN_LISTA_BLOQUEADAS", "toString", null]) {
    assert.equal(skipReason(code), quiet, String(code));
  }
  // Nothing on a client screen may use the words of an AML hold (e2e/aml.mjs TIP_OFF).
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  for (const code of [
    "SALDO_INSUFICIENTE",
    "MERCADO_CERRADO",
    "MERCADO_NO_DISPONIBLE",
    "MONTO_INSUFICIENTE",
    "KYC_NO_VERIFICADO",
    "OPERACION_NO_DISPONIBLE",
    "CUENTA_CONGELADA",
  ]) {
    assert.doesNotMatch(skipReason(code), TIP_OFF, code);
  }
  for (const action of ["pause", "resume", "cancel"] as const) {
    assert.doesNotMatch(actionSentence(parsed(), action, NOW), TIP_OFF, action);
  }
});

test("an action shows at once, before the server answers", () => {
  const p = parsed();
  assert.deepEqual(applyAction(p, "pause"), { ...p, status: "paused", next_execution_at: null, upcoming: [] });
  // Resumed, its dates come from the server.
  assert.deepEqual(applyAction(applyAction(p, "pause"), "resume"), {
    ...p,
    status: "active",
    next_execution_at: null,
    upcoming: [],
  });
  assert.equal(applyAction(p, "cancel").status, "cancelled");
});

test("the sentence that confirms an action", () => {
  const p = parsed();
  assert.equal(actionSentence(p, "pause", NOW), "Savings plan for AUG1 paused: no purchases until you resume it.");
  assert.equal(actionSentence(p, "resume", NOW), "Savings plan for AUG1 resumed. Next purchase: Tue, Dec 1 · 9:00 AM.");
  assert.equal(
    actionSentence(p, "cancel", NOW),
    "Savings plan for AUG1 cancelled. What it bought stays in your vault.",
  );
});
