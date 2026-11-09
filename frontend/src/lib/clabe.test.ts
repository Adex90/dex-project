/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { formatClabe, isValidClabe } from "./clabe.ts";

// Banxico's worked example, also used by backend/src/spei/clabe.rs: the products mod 10
// add up to 61, so the check digit is (10 − 1) mod 10 = 9.
const BANXICO_EXAMPLE = "032180000118359719";

test("a CLABE with the right check digit is valid", () => {
  assert.equal(isValidClabe(BANXICO_EXAMPLE), true);
  // 0 as the check digit: the products add up to a multiple of 10.
  assert.equal(isValidClabe("000000000000000000"), true);
});

test("any other check digit is refused", () => {
  for (let d = 0; d <= 9; d++) {
    if (d === 9) continue;
    assert.equal(isValidClabe(`${BANXICO_EXAMPLE.slice(0, 17)}${d}`), false, `check digit ${d}`);
  }
});

test("a single mistyped digit is caught", () => {
  // Swap the account's last digit: 1 -> 2.
  assert.equal(isValidClabe("032180000118359729"), false);
});

test("wrong length, spaces and letters are refused", () => {
  assert.equal(isValidClabe(BANXICO_EXAMPLE.slice(0, 17)), false);
  assert.equal(isValidClabe(`${BANXICO_EXAMPLE}0`), false);
  assert.equal(isValidClabe("032 180 00011835971 9"), false);
  assert.equal(isValidClabe("03218000011835971X"), false);
  assert.equal(isValidClabe(""), false);
});

test("a CLABE is grouped as bank, plaza, account and check digit", () => {
  assert.equal(formatClabe(BANXICO_EXAMPLE), "032 180 00011835971 9");
});

test("a malformed value is shown unchanged, never reshaped", () => {
  assert.equal(formatClabe("12345"), "12345");
  assert.equal(formatClabe("032 180 00011835971 9"), "032 180 00011835971 9");
});
