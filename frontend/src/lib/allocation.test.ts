/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { MAX_PARTS, allocation, basisPoints, cashBreakdown, formatShare } from "./allocation.ts";
import { addUnits } from "./decimal.ts";
import type { Holding, VaultBalance } from "../types/vault.ts";

function balance(overrides: Partial<VaultBalance> = {}): VaultBalance {
  return {
    total_mxn: "100000.00000000",
    available_cash: "40000.00000000",
    held_cash: "0.00000000",
    invested_assets: "60000.00000000",
    unrealized_pnl_percentage: "0.00",
    ...overrides,
  };
}

function holding(ticker: string, value: string, name = `${ticker} name`): Holding {
  return {
    ticker,
    name,
    shares: "1.00000000",
    held_shares: "0.00000000",
    current_price: value,
    total_value: value,
    daily_change_pct: "0.00",
  };
}

test("allocation: cash first, then the investments largest first, shares adding up to 100.00 %", () => {
  const result = allocation(balance(), [
    holding("AUG1", "15000.00000000"),
    holding("STSLA", "45000.00000000", "Synthetic Tesla"),
  ]);
  assert.equal(result.total, "100000.00000000");
  assert.deepEqual(
    result.parts.map((p) => [p.key, p.kind, p.label, p.value, p.basisPoints]),
    [
      ["cash", "cash", "Cash", "40000.00000000", 4000],
      ["STSLA", "holding", "STSLA", "45000.00000000", 4500],
      ["AUG1", "holding", "AUG1", "15000.00000000", 1500],
    ],
  );
  assert.equal(result.parts[1]!.detail, "Synthetic Tesla");
  assert.deepEqual(result.parts[1]!.tickers, ["STSLA"]);
  assert.equal(result.cashBasisPoints, 4000);
});

test("allocation: cash includes what is held for orders and on hold; fixed term is invested beyond the holdings", () => {
  // 10,000 available + 1,250 held for orders + 500 on hold, 30,000 in AUG1 and 8,000 in SOFOM contracts.
  const vault = balance({
    total_mxn: "49750.00000000",
    available_cash: "10000.00000000",
    held_cash: "1250.00000000",
    invested_assets: "38000.00000000",
  });
  const result = allocation(vault, [holding("AUG1", "30000.00000000")]);
  assert.deepEqual(
    result.parts.map((p) => [p.label, p.value]),
    [
      ["Cash", "11750.00000000"],
      ["AUG1", "30000.00000000"],
      ["Fixed term", "8000.00000000"],
    ],
  );
  assert.equal(result.parts[2]!.detail, "SOFOM mutuum contracts");
  assert.equal(addUnits(...result.parts.map((p) => p.value)), vault.total_mxn, "the parts add up to the total");
  assert.deepEqual(cashBreakdown(vault), {
    available: "10000.00000000",
    held: "1250.00000000",
    onHold: "500.00000000",
  });
});

test("allocation: shares that do not divide evenly still add up to 10,000 basis points", () => {
  const vault = balance({ total_mxn: "3.00000000", available_cash: "1.00000000", invested_assets: "2.00000000" });
  const result = allocation(vault, [holding("AUG1", "1.00000000"), holding("STSLA", "1.00000000")]);
  assert.deepEqual(
    result.parts.map((p) => p.basisPoints),
    [3334, 3333, 3333],
    "the leftover basis point goes to the earliest of equal remainders",
  );
  assert.equal(
    result.parts.reduce((sum, p) => sum + p.basisPoints, 0),
    10_000,
  );
  assert.deepEqual(basisPoints([2n, 1n, 1n, 1n, 1n, 1n, 1n], 8n), [2500, 1250, 1250, 1250, 1250, 1250, 1250]);
  assert.deepEqual(basisPoints([1n, 2n], 3n), [3333, 6667], "the larger remainder first");
  assert.deepEqual(basisPoints([5n], 0n), [0]);
});

test("allocation: past four investments the smallest fold into Other, which names them", () => {
  const holdings = ["A", "B", "C", "D", "E", "F"].map((t, i) => holding(`T${t}`, `${(6 - i) * 1000}.00000000`));
  const vault = balance({
    total_mxn: "31000.00000000",
    available_cash: "10000.00000000",
    invested_assets: "21000.00000000",
  });
  const result = allocation(vault, holdings);
  assert.equal(result.parts.length, MAX_PARTS);
  assert.deepEqual(
    result.parts.map((p) => [p.label, p.value]),
    [
      ["Cash", "10000.00000000"],
      ["TA", "6000.00000000"],
      ["TB", "5000.00000000"],
      ["TC", "4000.00000000"],
      ["Other", "6000.00000000"],
    ],
  );
  const other = result.parts.at(-1)!;
  assert.equal(other.kind, "other");
  assert.equal(other.detail, "TD, TE, TF");
  assert.deepEqual(other.tickers, ["TD", "TE", "TF"]);
  assert.equal(addUnits(...result.parts.map((p) => p.value)), "31000.00000000");

  // Four investments and cash fit in five parts: nothing folds.
  const four = allocation(
    balance({ total_mxn: "28000.00000000", invested_assets: "18000.00000000", available_cash: "10000.00000000" }),
    holdings.slice(0, 4),
  );
  assert.equal(four.parts.length, 5);
  assert.ok(four.parts.every((p) => p.kind !== "other"));

  // Without cash the investments still take four places at most, fixed term folding like any other.
  const invested = balance({
    total_mxn: "20500.00000000",
    available_cash: "0.00000000",
    invested_assets: "20500.00000000",
  });
  const noCash = allocation(invested, holdings.slice(0, 4));
  assert.deepEqual(
    noCash.parts.map((p) => [p.label, p.value]),
    [
      ["TA", "6000.00000000"],
      ["TB", "5000.00000000"],
      ["TC", "4000.00000000"],
      ["Other", "5500.00000000"],
    ],
  );
  assert.equal(noCash.parts.at(-1)!.detail, "TD, Fixed term", "fixed term (2,500) folds after TD (3,000)");
  assert.deepEqual(noCash.parts.at(-1)!.tickers, ["TD"]);
});

test("allocation: nothing worth anything is left out; an empty vault has no parts", () => {
  const allInvested = allocation(
    balance({ total_mxn: "5000.00000000", available_cash: "0.00000000", invested_assets: "5000.00000000" }),
    [holding("AUG1", "5000.00000000"), holding("STSLA", "0.00000000")],
  );
  assert.deepEqual(
    allInvested.parts.map((p) => [p.label, p.basisPoints]),
    [["AUG1", 10_000]],
  );
  assert.equal(allInvested.cashBasisPoints, 0);
  const empty = allocation(
    balance({ total_mxn: "0.00000000", available_cash: "0.00000000", invested_assets: "0.00000000" }),
    [],
  );
  assert.deepEqual(empty, { total: "0.00000000", parts: [], cashBasisPoints: 0 });
  const allCash = allocation(
    balance({ total_mxn: "250.00000000", available_cash: "250.00000000", invested_assets: "0.00000000" }),
    [],
  );
  assert.deepEqual(
    allCash.parts.map((p) => [p.label, p.basisPoints]),
    [["Cash", 10_000]],
  );
});

test("allocation: holdings worth more than the invested total (a vault caught mid-update) never make a negative part", () => {
  const result = allocation(
    balance({ total_mxn: "1000.00000000", available_cash: "500.00000000", invested_assets: "500.00000000" }),
    [holding("AUG1", "700.00000000")],
  );
  assert.deepEqual(
    result.parts.map((p) => [p.label, p.value]),
    [
      ["Cash", "500.00000000"],
      ["AUG1", "700.00000000"],
    ],
  );
  assert.equal(result.total, "1200.00000000", "the shares are of what the parts add up to");
  assert.equal(
    result.parts.reduce((sum, p) => sum + p.basisPoints, 0),
    10_000,
  );
});

test("formatShare: two decimals, and a sliver of a part reads <0.01%", () => {
  assert.equal(formatShare(4620), "46.20%");
  assert.equal(formatShare(10_000), "100.00%");
  assert.equal(formatShare(5), "0.05%");
  assert.equal(formatShare(0, "0.01000000"), "<0.01%");
  assert.equal(formatShare(0, "0"), "0.00%");
  assert.equal(formatShare(0), "0.00%");
});
