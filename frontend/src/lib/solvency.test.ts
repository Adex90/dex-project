/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { barShare, formatUnits, includedBalances, positionRows, reserveRatioPct } from "./solvency.ts";

test("the reserve ratio rounds down, so a cent short never reads 100.00", () => {
  assert.equal(reserveRatioPct("1000.50000000", "1000.5"), "100.00");
  assert.equal(reserveRatioPct("1000.49", "1000.50"), "99.99");
  assert.equal(reserveRatioPct("15000", "10000"), "150.00");
  assert.equal(reserveRatioPct("0.00000001", "3"), "0.00");
  assert.equal(reserveRatioPct("0", "0"), null);
  assert.equal(barShare("100", "100"), 100);
  assert.equal(barShare("50", "100"), 50);
  assert.equal(barShare("0", "0"), 0);
});

test("the leaf's balance lines: held and on-hold funds only when there are any", () => {
  assert.deepEqual(includedBalances(null), []);
  assert.deepEqual(
    includedBalances({ disponible: "15000.00000000", bloqueado: "0.00000000", comprometido: "0.00000000" }),
    [{ key: "available", label: "Available cash", amount: "15000.00000000" }],
  );
  const all = includedBalances({ disponible: "10.00000000", bloqueado: "5.00000000", comprometido: "100.00000000" });
  assert.deepEqual(
    all.map((l) => l.key),
    ["available", "orders", "hold"],
  );
  // Funds on hold read with the neutral copy, never with compliance words.
  assert.equal(all[2]!.label, "Funds on hold (still fully backed)");
  assert.doesNotMatch(all.map((l) => l.label).join(" "), /\b(AML|PLD|frozen|freeze|suspicious|compliance)\b/i);
});

test("positions in the snapshot next to the holdings now", () => {
  const rows = positionRows(
    [
      { hoja: { ticker: "AUG1", cantidad: "0.77520000" } },
      { hoja: { ticker: "AGT1", cantidad: "3.00000000" } },
      { hoja: { ticker: "SCT28", cantidad: "1.00000000" } },
    ],
    [
      { ticker: "AUG1", name: "Tokenized Gold", shares: "0.7752" },
      { ticker: "AGT1", name: "Tokenized Silver", shares: "1.5" },
      { ticker: "STSLA", name: "Synthetic Tesla", shares: "0.25" },
      { ticker: "CETES28", name: "CETES 28", shares: "0" },
    ],
  );
  assert.deepEqual(rows, [
    { ticker: "AGT1", name: "Tokenized Silver", atSnapshot: "3.00000000", now: "1.5", status: "changed" },
    { ticker: "AUG1", name: "Tokenized Gold", atSnapshot: "0.77520000", now: "0.7752", status: "included" },
    { ticker: "SCT28", name: null, atSnapshot: "1.00000000", now: null, status: "changed" },
    { ticker: "STSLA", name: "Synthetic Tesla", atSnapshot: null, now: "0.25", status: "after" },
  ]);
});

test("asset units display without noise", () => {
  assert.equal(formatUnits("0.77520000"), "0.7752");
  assert.equal(formatUnits("2.50000000"), "2.50");
  assert.equal(formatUnits("1234.00000001"), "1,234.00000001");
  assert.equal(formatUnits("3.00000000"), "3.00");
});
