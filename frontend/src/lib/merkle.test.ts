/**
 * Cross-implementation vectors. The expected hashes were produced by an
 * independent Python implementation of the spec in backend/src/crypto/merkle.rs,
 * and the same root is asserted by the Rust test suite.
 *
 * Run with: npm test   (Node >= 22.18 strips TypeScript types natively)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import { divRoundHalfEven, formatAmount, formatRate, interestAct360, parseAmountInput, toUnits } from "./decimal.ts";
import { combine, hashLeaf, sha256, sha256Js, bytesToHex, verifyInclusion, type ProofLeaf } from "./merkle.ts";

const uuid = (byte: string) => {
  const hex = byte.repeat(16);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
};

const leaves: ProofLeaf[] = [
  { cuenta_id: uuid("11"), saldo: "1000.00000000", sal: "aa".repeat(32) },
  { cuenta_id: uuid("22"), saldo: "0.50000000", sal: "bb".repeat(32) },
  { cuenta_id: uuid("33"), saldo: "0.00000000", sal: "cc".repeat(32) },
];
const ROOT = "9b089a89d1de37751f63a455c8a9e8cd15540a028aa8654f96fd18b03ef9c3f0";
const TOTAL = "1000.50000000";

test("pure-JS SHA-256 matches the FIPS test vectors and WebCrypto", async () => {
  const enc = new TextEncoder();
  assert.equal(bytesToHex(sha256Js(enc.encode(""))), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
  assert.equal(bytesToHex(sha256Js(enc.encode("abc"))), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  const long = enc.encode("a".repeat(1000));
  assert.equal(bytesToHex(sha256Js(long)), bytesToHex(await sha256(long, "webcrypto")));
});

test("leaf and node hashes match the reference implementation", async () => {
  const [a, b, c] = await Promise.all(leaves.map((l) => hashLeaf(l)));
  assert.equal(bytesToHex(a!.hash), "892eb7b69d676ea2338d32d883b66a29a64aa8a7808f3ae2bcbf64e0987536f9");
  assert.equal(bytesToHex(b!.hash), "b16d717b123a2f503c6a03043a64f5cab30b6e511f14d83377e179fb142ec036");
  assert.equal(bytesToHex(c!.hash), "af52a7e97b3b63f1f60d4439215f5918199ee5e6000d2720a6a62f7aeeb255f0");
  const ab = await combine(a!, b!);
  assert.equal(bytesToHex(ab.hash), "99cc3e6d58053dddc683b16fbb0e8e8e30096429b9cd94dc4b9d120993e26b62");
  const root = await combine(ab, c!);
  assert.equal(bytesToHex(root.hash), ROOT);
  assert.equal(root.sum, 100_050_000_000n);
});

test("inclusion proofs verify with both hash engines; the odd leaf is promoted", async () => {
  const a = await hashLeaf(leaves[0]!);
  const b = await hashLeaf(leaves[1]!);
  const c = await hashLeaf(leaves[2]!);
  const ab = await combine(a, b);
  for (const engine of ["webcrypto", "js"] as const) {
    const proof0 = await verifyInclusion(
      leaves[0]!,
      [
        { lado: "DERECHA", hash: bytesToHex(b.hash), suma: "0.50000000" },
        { lado: "DERECHA", hash: bytesToHex(c.hash), suma: "0.00000000" },
      ],
      ROOT,
      TOTAL,
      engine,
    );
    assert.ok(proof0.valid, engine);
    // Leaf 2 has no sibling at level 1 (odd count): a single step.
    const proof2 = await verifyInclusion(
      leaves[2]!,
      [{ lado: "IZQUIERDA", hash: bytesToHex(ab.hash), suma: "1000.50000000" }],
      ROOT,
      TOTAL,
      engine,
    );
    assert.ok(proof2.valid, engine);
    assert.equal(proof2.steps.length, 1);
  }
});

test("tampering is detected", async () => {
  const b = await hashLeaf(leaves[1]!);
  const c = await hashLeaf(leaves[2]!);
  const path = [
    { lado: "DERECHA" as const, hash: bytesToHex(b.hash), suma: "0.50000000" },
    { lado: "DERECHA" as const, hash: bytesToHex(c.hash), suma: "0.00000000" },
  ];
  // Inflated balance.
  assert.ok(!(await verifyInclusion({ ...leaves[0]!, saldo: "1000.00000001" }, path, ROOT, TOTAL)).valid);
  // Under-reported sibling sum (the classic sum-tree attack).
  const hidden = [{ ...path[0]!, suma: "0.40000000" }, path[1]!];
  assert.ok(!(await verifyInclusion(leaves[0]!, hidden, ROOT, TOTAL)).valid);
  // Wrong side.
  assert.ok(!(await verifyInclusion(leaves[0]!, [{ ...path[0]!, lado: "IZQUIERDA" }, path[1]!], ROOT, TOTAL)).valid);
  // Right root, wrong published total.
  const r = await verifyInclusion(leaves[0]!, path, ROOT, "1000.00000000");
  assert.ok(r.rootMatches && !r.totalMatches && !r.valid);
});

test("decimal helpers are exact", () => {
  assert.equal(toUnits("1234.5"), 123_450_000_000n);
  assert.equal(formatAmount("1234567.895"), "1,234,567.90");
  assert.equal(formatAmount("0.005"), "0.00"); // half-even
  assert.equal(formatAmount("0.015"), "0.02");
  assert.equal(divRoundHalfEven(5n, 2n), 2n);
  assert.equal(divRoundHalfEven(7n, 2n), 4n);
  assert.equal(formatRate("0.072500"), "7.25%");
  // Same values as backend/src/sofom/tasas.rs tests.
  assert.equal(interestAct360("10000", "0.085", 364), "859.44000000");
  assert.equal(interestAct360("1000", "0.045", 28), "3.50000000");
  assert.equal(interestAct360("10000", "0.0725", 182), "366.53000000");
  const bad = parseAmountInput("1.123456789");
  assert.ok(!bad.ok);
  const good = parseAmountInput("1,500.5");
  assert.ok(good.ok && good.value === "1500.50000000");
  assert.ok(!parseAmountInput("0").ok);
  assert.ok(!parseAmountInput("10000000000").ok);
});
