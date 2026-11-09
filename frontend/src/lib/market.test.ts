/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  amountForApi,
  amountUnits,
  estimateQuantity,
  estimateSale,
  formatAmountInput,
  maxAmount,
  orderProblem,
  positionValue,
  pressKey,
  sanitizeAmount,
  type NumpadKey,
} from "./market.ts";

/** Types keys on the numpad from an empty amount. */
const typeKeys = (keys: NumpadKey[], start = "") => keys.reduce(pressKey, start);

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
