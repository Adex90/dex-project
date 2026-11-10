/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  LIVE_ORDER_TYPES,
  PRICE_DECIMALS,
  amountForApi,
  amountUnits,
  assessOrder,
  buildOrder,
  draftSignature,
  estimateQuantity,
  estimateSale,
  formatAmountInput,
  formatPrice,
  maxAmount,
  monthlyEquivalent,
  orderProblem,
  orderTypesFor,
  positionValue,
  pressKey,
  priceEntry,
  priceForApi,
  priceProblem,
  sanitizeAmount,
  smallestUnit,
  type NumpadKey,
  type OrderContext,
  type OrderDraft,
} from "./market.ts";

/** Types keys on the numpad from an empty amount (or price, with `decimals`). */
const typeKeys = (keys: NumpadKey[], start = "", decimals?: number) =>
  keys.reduce((value, key) => pressKey(value, key, decimals), start);

/* The numpad */

test("numpad: digits, one point, two decimals, no leading zeros", () => {
  assert.equal(typeKeys(["1", "2", "5", "0"]), "1250");
  assert.equal(typeKeys(["0", "0", "5"]), "5", "zeros never lead");
  assert.equal(typeKeys(["0", "0"]), "0");
  assert.equal(typeKeys(["."]), "0.", "a point first means zero point");
  assert.equal(typeKeys(["1", ".", ".", "5"]), "1.5", "a second point is ignored");
  assert.equal(typeKeys(["9", ".", "9", "9", "9"]), "9.99", "a third decimal is ignored");
  assert.equal(typeKeys(["0", ".", "0", "5"]), "0.05");
});

test("numpad: backspace removes one character; nine integer digits at most", () => {
  assert.equal(typeKeys(["1", ".", "5", "back"]), "1.");
  assert.equal(typeKeys(["back"], "0."), "0");
  assert.equal(typeKeys(["back"], ""), "");
  const nine = typeKeys(["9", "9", "9", "9", "9", "9", "9", "9", "9"]);
  assert.equal(typeKeys(["9"], nine), "999999999", "a tenth digit is ignored");
  assert.equal(typeKeys([".", "5"], nine), "999999999.5", "decimals still fit");
});

/* Typed and pasted text */

test("sanitizeAmount: separators and the sign go, invalid text is refused", () => {
  assert.equal(sanitizeAmount("$1,250.50"), "1250.50");
  assert.equal(sanitizeAmount(" 0005 "), "5");
  assert.equal(sanitizeAmount("00.5"), "0.5");
  assert.equal(sanitizeAmount("."), "0.");
  assert.equal(sanitizeAmount(""), "");
  assert.equal(sanitizeAmount("12.345"), null, "a third decimal");
  assert.equal(sanitizeAmount("1.2.3"), null, "a second point");
  assert.equal(sanitizeAmount("12a"), null);
  assert.equal(sanitizeAmount("-5"), null, "never negative");
  assert.equal(sanitizeAmount("1234567890"), null, "ten integer digits");
});

test("formatAmountInput: grouped as typed, the point and zeros kept", () => {
  assert.equal(formatAmountInput(""), "0");
  assert.equal(formatAmountInput("1250"), "1,250");
  assert.equal(formatAmountInput("1250."), "1,250.");
  assert.equal(formatAmountInput("1250.50"), "1,250.50");
  assert.equal(formatAmountInput("999999999.5"), "999,999,999.5");
  assert.equal(formatAmountInput("0.05"), "0.05");
});

/* Exact values */

test("amountUnits and amountForApi: exact, and always two decimals for the API", () => {
  assert.equal(amountUnits(""), 0n);
  assert.equal(amountUnits("12."), 1_200_000_000n);
  assert.equal(amountUnits("0.05"), 5_000_000n);
  assert.equal(amountForApi("12.5"), "12.50");
  assert.equal(amountForApi("1250"), "1250.00");
  assert.equal(amountForApi("0.05"), "0.05");
});

test("maxAmount: what is available, rounded down to the cent", () => {
  assert.equal(maxAmount("15000.12345678"), "15000.12");
  assert.equal(maxAmount("15000.00000000"), "15000");
  assert.equal(maxAmount("15000.10000000"), "15000.1");
  assert.equal(maxAmount("0.009"), "0", "less than a cent");
  assert.equal(maxAmount("0"), "");
});

test("estimateQuantity: rounded down, never more than the money buys", () => {
  assert.equal(estimateQuantity(amountUnits("1250"), "1612.40000000", 4), "0.7752");
  assert.equal(estimateQuantity(amountUnits("20000"), "1612.40", 4), "12.4038");
  assert.equal(estimateQuantity(amountUnits("100"), "9.95", 0), "10", "whole units");
  assert.equal(estimateQuantity(amountUnits("1612.40"), "1612.40", 4), "1.0000", "exactly one");
  assert.equal(estimateQuantity(0n, "1612.40", 4), "0.0000");
  assert.equal(estimateQuantity(amountUnits("5"), "0", 4), null);
});

test("estimateSale: Max sells the whole holding, as the backend fills it", () => {
  const max = amountUnits(maxAmount(positionValue("1.55040000", "1612.40000000")));
  assert.equal(max, amountUnits("2499.86"), "1.5504 at 1,612.40 is worth 2,499.86496");
  assert.equal(estimateSale(max, "1.55040000", "1612.40000000", 4), "1.5504", "all of it, no residue");
  assert.equal(estimateQuantity(max, "1612.40000000", 4), "1.5503", "what the amount alone covers");
  assert.equal(estimateSale(amountUnits("1000"), "1.55040000", "1612.40000000", 4), "0.6201", "part: rounded down");
  assert.equal(estimateSale(amountUnits("99.20"), "10.00000000", "9.92000000", 0), "10", "worth exactly 99.20");
  assert.equal(estimateSale(amountUnits("99.19"), "10.00000000", "9.92000000", 0), "9");
  assert.equal(estimateSale(amountUnits("0.01"), "0.00000100", "1.00000000", 8), "0.00000100", "worth less than a cent");
  assert.equal(estimateSale(0n, "1.55040000", "1612.40000000", 4), "0.0000", "nothing entered yet");
  assert.equal(estimateSale(amountUnits("5"), "0", "1612.40000000", 4), "0.0031", "no holding: the amount alone");
  assert.equal(estimateSale(amountUnits("5"), "1.00000000", "0", 4), null);
});

test("positionValue: quantity times price, exact", () => {
  assert.equal(positionValue("12.50000000", "1612.40000000"), "20155.00000000");
  assert.equal(positionValue("0.33333333", "3.00000000"), "0.99999999");
});

test("orderProblem: nothing entered, more than available, or fine", () => {
  assert.equal(orderProblem("", "100"), "empty");
  assert.equal(orderProblem("0.", "100"), "empty");
  assert.equal(orderProblem("100.01", "100"), "exceeds");
  assert.equal(orderProblem("100", "100.00000000"), null, "all of it");
  assert.equal(orderProblem("0.01", "100"), null);
});

/* Order types */

/** Gold after two buys (the smoke test's second pass): 1.5504 g held, 13,734.70 cash. */
const GOLD: OrderContext = { current: "1612.40000000", decimals: 4, cash: "13734.70000000", held: "1.55040000" };

const draft = (overrides: Partial<OrderDraft>): OrderDraft => ({
  type: "market",
  side: "buy",
  amount: "",
  price: "",
  frequency: "monthly",
  ...overrides,
});

test("prices: four decimals on the numpad and the keyboard; two to four for the API", () => {
  assert.equal(typeKeys(["9", ".", "9", "4", "3", "9", "1"], "", PRICE_DECIMALS), "9.9439", "a fifth decimal is refused");
  assert.equal(typeKeys(["9", ".", "9", "4", "3"]), "9.94", "amounts keep two");
  assert.equal(sanitizeAmount("$9.9439", PRICE_DECIMALS), "9.9439");
  assert.equal(sanitizeAmount("9.94391", PRICE_DECIMALS), null);
  assert.equal(sanitizeAmount("9.943"), null, "amounts keep two");
  const prices: [string, string][] = [
    ["1580", "1580.00"],
    ["1580.", "1580.00"],
    ["9.9", "9.90"],
    ["9.943", "9.943"],
    ["9.9439", "9.9439"],
    ["0.5", "0.50"],
  ];
  for (const [typed, sent] of prices) {
    assert.equal(priceForApi(typed), sent, typed);
  }
  assert.equal(priceEntry("1612.40000000"), "1612.4", "Use current");
  assert.equal(priceEntry("9.94390000"), "9.9439");
  assert.equal(priceEntry("9.94395555"), "9.9439", "rounded down to four decimals");
  assert.equal(formatPrice("1612.40000000"), "$1,612.40");
  assert.equal(formatPrice("9.94390000"), "$9.9439");
  assert.equal(formatPrice("1580.50000000"), "$1,580.50");
});

test("order types: a savings plan only buys, and only market orders are live", () => {
  assert.deepEqual(orderTypesFor("buy"), ["market", "limit", "stop", "savings_plan"]);
  assert.deepEqual(orderTypesFor("sell"), ["market", "limit", "stop"]);
  assert.deepEqual(LIVE_ORDER_TYPES, ["market"]);
  assert.equal(smallestUnit(4), "0.0001");
  assert.equal(smallestUnit(0), "1");
});

test("monthlyEquivalent: what a plan invests in an average month, half up to the cent", () => {
  assert.equal(monthlyEquivalent("500", "weekly"), "2166.67", "500 × 52 / 12");
  assert.equal(monthlyEquivalent("500", "biweekly"), "1083.33", "500 × 26 / 12");
  assert.equal(monthlyEquivalent("500", "monthly"), "500.00");
  assert.equal(monthlyEquivalent("0.03", "biweekly"), "0.07", "6.5 cents rounds up");
  assert.equal(monthlyEquivalent("", "weekly"), "0.00");
});

test("priceProblem: a price is needed; a stop the price has reached would trigger at once", () => {
  const now = GOLD.current;
  assert.equal(priceProblem("limit", "buy", "", now), "empty");
  assert.equal(priceProblem("limit", "buy", "0.", now), "empty");
  assert.equal(priceProblem("limit", "buy", "1580", now), null);
  assert.equal(priceProblem("limit", "buy", "1700", now), null, "a limit the price meets is allowed: it fills at once");
  assert.equal(priceProblem("stop", "buy", "1700", now), null, "to buy, a stop above the price");
  assert.equal(priceProblem("stop", "buy", "1612.40", now), "wrong_side", "at the price, it would trigger at once");
  assert.equal(priceProblem("stop", "buy", "1500", now), "wrong_side");
  assert.equal(priceProblem("stop", "sell", "1500", now), null, "a stop-loss below the price");
  assert.equal(priceProblem("stop", "sell", "1700", now), "wrong_side");
});

test("assessOrder: a market order, as before, and one too small to buy a unit", () => {
  const buy = assessOrder(draft({ amount: "1250" }), GOLD);
  assert.deepEqual(
    [buy.price, buy.available, buy.max, buy.quantity, buy.amountProblem, buy.priceProblem, buy.ready],
    ["1612.40000000", "13734.70000000", "13734.7", "0.7752", null, null, true],
  );
  assert.equal(assessOrder(draft({ amount: "" }), GOLD).amountProblem, "empty");
  assert.equal(assessOrder(draft({ amount: "13734.71" }), GOLD).amountProblem, "exceeds");
  assert.equal(assessOrder(draft({ amount: "0.16" }), GOLD).amountProblem, "too_small", "0.0001 g costs 0.16124");
  assert.equal(assessOrder(draft({ amount: "0.17" }), GOLD).amountProblem, null);

  const sell = assessOrder(draft({ side: "sell", amount: "2499.86" }), GOLD);
  assert.deepEqual([sell.available, sell.max, sell.quantity], ["2499.86496000", "2499.86", "1.5504"], "Max sells it all");
});

test("assessOrder: a limit or a stop estimates at its own price", () => {
  const limit = assessOrder(draft({ type: "limit", amount: "1250", price: "1580" }), GOLD);
  assert.deepEqual([limit.price, limit.quantity, limit.marketable, limit.ready], ["1580.00000000", "0.7911", false, true]);
  const unpriced = assessOrder(draft({ type: "limit", amount: "1250" }), GOLD);
  assert.deepEqual([unpriced.priceProblem, unpriced.quantity, unpriced.ready], ["empty", null, false]);
  assert.equal(assessOrder(draft({ type: "limit", amount: "1250", price: "1700" }), GOLD).marketable, true);
  assert.equal(assessOrder(draft({ type: "limit", side: "sell", amount: "1", price: "1500" }), GOLD).marketable, true);
  assert.equal(assessOrder(draft({ type: "stop", amount: "1250", price: "1700" }), GOLD).marketable, false, "only limits");
  assert.equal(assessOrder(draft({ type: "stop", amount: "1250", price: "1500" }), GOLD).ready, false, "a buy stop below");

  // A sale can take what the position is worth at the order's price, and Max there sells it all.
  const stopLoss = assessOrder(draft({ type: "stop", side: "sell", amount: "2325.6", price: "1500" }), GOLD);
  assert.deepEqual([stopLoss.available, stopLoss.max, stopLoss.quantity], ["2325.60000000", "2325.6", "1.5504"]);
  assert.equal(assessOrder(draft({ type: "stop", side: "sell", amount: "2325.61", price: "1500" }), GOLD).amountProblem, "exceeds");
  assert.equal(assessOrder(draft({ type: "limit", side: "sell", amount: "1", price: "1700" }), GOLD).available, "2635.68000000");
});

test("assessOrder: a savings plan draws on no balance today", () => {
  const plan = assessOrder(draft({ type: "savings_plan", amount: "500", frequency: "biweekly" }), { ...GOLD, cash: "0" });
  assert.deepEqual(
    [plan.price, plan.available, plan.max, plan.quantity, plan.amountProblem, plan.ready],
    ["1612.40000000", null, "", "0.3100", null, true],
  );
  assert.equal(assessOrder(draft({ type: "savings_plan" }), GOLD).amountProblem, "empty");
  const cetes = { ...GOLD, current: "9.94390000", decimals: 0 };
  assert.equal(assessOrder(draft({ type: "savings_plan", amount: "9.94" }), cetes).amountProblem, "too_small", "under one title");
  assert.equal(assessOrder(draft({ type: "savings_plan", amount: "9.95" }), cetes).quantity, "1");
});

test("buildOrder: one request per type, in the API's format", () => {
  const key = "0f8c2d4e-9a1b-4c3d-8e7f-6a5b4c3d2e1f";
  const base = { ticker: "AUG1", idempotency_key: key };
  assert.deepEqual(buildOrder(draft({ amount: "1250.5" }), "AUG1", key), {
    ...base,
    amount: "1250.50",
    order_type: "market",
    side: "buy",
  });
  assert.deepEqual(buildOrder(draft({ type: "limit", side: "sell", amount: "1000", price: "1700" }), "AUG1", key), {
    ...base,
    amount: "1000.00",
    order_type: "limit",
    side: "sell",
    limit_price: "1700.00",
  });
  assert.deepEqual(buildOrder(draft({ type: "stop", side: "sell", amount: "1000", price: "1500.5" }), "AUG1", key), {
    ...base,
    amount: "1000.00",
    order_type: "stop",
    side: "sell",
    stop_price: "1500.50",
  });
  assert.deepEqual(buildOrder(draft({ type: "savings_plan", amount: "500", frequency: "weekly" }), "AUG1", key), {
    ...base,
    amount: "500.00",
    order_type: "savings_plan",
    side: "buy",
    frequency: "weekly",
  });
});

test("draftSignature: the same order keeps its key; anything that changes it is a new order", () => {
  const same = (a: Partial<OrderDraft>, b: Partial<OrderDraft>) => draftSignature(draft(a)) === draftSignature(draft(b));
  assert.ok(same({ amount: "500" }, { amount: "500.00" }), "the same amount, typed differently");
  assert.ok(same({ amount: "500", frequency: "weekly" }, { amount: "500", price: "9" }), "a market order ignores price and frequency");
  assert.ok(same({ type: "savings_plan", amount: "500", price: "1" }, { type: "savings_plan", amount: "500", price: "2" }));
  assert.ok(!same({ amount: "500" }, { amount: "500.01" }));
  assert.ok(!same({ amount: "500" }, { amount: "500", side: "sell" }));
  assert.ok(!same({ amount: "500" }, { type: "limit", amount: "500", price: "1612.40" }));
  assert.ok(!same({ type: "limit", amount: "500", price: "1580" }, { type: "limit", amount: "500", price: "1581" }));
  assert.ok(!same({ type: "savings_plan", amount: "500", frequency: "weekly" }, { type: "savings_plan", amount: "500" }));
});
