/**
 * Cross-implementation vectors. The expected hashes were produced by an
 * independent Python implementation of the spec in backend/src/crypto/merkle.rs;
 * the Rust tests and the database (tests/db/0018_reserva_posiciones.sql) assert
 * the same values.
 *
 * Run with: npm test   (Node >= 22.18 strips TypeScript types natively)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import { divRoundHalfEven, formatAmount, formatRate, interestAct360, parseAmountInput, toUnits } from "./decimal.ts";
import {
  bytesToHex,
  combine,
  expectedPathSides,
  hashLeaf,
  hashPositionLeaf,
  isTicker,
  sha256,
  sha256Js,
  verifyInclusion,
  verifyPositionInclusion,
  type ProofLeaf,
  type ProofPositionLeaf,
  type ProofStep,
} from "./merkle-verifier.ts";

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

const gold: ProofPositionLeaf[] = [
  { ticker: "AUG1", cuenta_id: uuid("11"), cantidad: "2.50000000", sal: "dd".repeat(32) },
  { ticker: "AUG1", cuenta_id: uuid("22"), cantidad: "0.00000001", sal: "ee".repeat(32) },
];
const GOLD_ROOT = "d6028011a2ceeea4ed7e14a9861ea1aa06312f6a95676d9bbc0ac481f63a84c3";
const GOLD_TOTAL = "2.50000001";

/** Leaf 0's path in the three-leaf cash tree. */
async function pathOfLeaf0(): Promise<ProofStep[]> {
  const b = await hashLeaf(leaves[1]!);
  const c = await hashLeaf(leaves[2]!);
  return [
    { lado: "DERECHA", hash: bytesToHex(b.hash), suma: "0.50000000" },
    { lado: "DERECHA", hash: bytesToHex(c.hash), suma: "0.00000000" },
  ];
}

test("pure-JS SHA-256 matches the FIPS test vectors and WebCrypto", async () => {
  const enc = new TextEncoder();
  assert.equal(
    bytesToHex(sha256Js(enc.encode(""))),
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  );
  assert.equal(
    bytesToHex(sha256Js(enc.encode("abc"))),
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
  );
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

test("position leaves match the reference implementation, with both engines", async () => {
  for (const engine of ["webcrypto", "js"] as const) {
    const [a, b] = await Promise.all(gold.map((l) => hashPositionLeaf(l, engine)));
    assert.equal(bytesToHex(a!.hash), "1c07aca64ba5dc119b5f606c9c9f7fca0c43285f44405805d40f8990b7721ac2", engine);
    assert.equal(bytesToHex(b!.hash), "af1a6c50dd4af636162a3cb32fd6b55d41492b2e1ea77e2a4ba26ac8ab2dbc9f", engine);
    const root = await combine(a!, b!, engine);
    assert.equal(bytesToHex(root.hash), GOLD_ROOT, engine);
    assert.equal(root.sum, 250_000_001n);
  }
  // The same account, quantity and salt in the silver tree hash differently, and
  // differently from a cash leaf with the same numbers.
  const silver = await hashPositionLeaf({ ...gold[0]!, ticker: "AGT1" });
  assert.equal(bytesToHex(silver.hash), "40d20497109c13fff04fd2bee899b18a25ee7f63137f6adc72b8c3cddddc3290");
  const cash = await hashLeaf({ cuenta_id: gold[0]!.cuenta_id, saldo: gold[0]!.cantidad, sal: gold[0]!.sal });
  assert.notEqual(bytesToHex(cash.hash), "1c07aca64ba5dc119b5f606c9c9f7fca0c43285f44405805d40f8990b7721ac2");
});

test("inclusion proofs verify with both hash engines; the odd leaf is promoted", async () => {
  const a = await hashLeaf(leaves[0]!);
  const b = await hashLeaf(leaves[1]!);
  const ab = await combine(a, b);
  for (const engine of ["webcrypto", "js"] as const) {
    const proof0 = await verifyInclusion(leaves[0]!, await pathOfLeaf0(), ROOT, TOTAL, { engine });
    assert.ok(proof0.valid, engine);
    assert.equal(proof0.steps.length, 2);
    // Each step names its two children in hashing order: the climbing node is left here.
    assert.equal(proof0.steps[0]!.leftHash, proof0.leafHash);
    assert.equal(proof0.steps[1]!.leftHash, proof0.steps[0]!.nodeHash);
    assert.equal(proof0.steps[1]!.nodeHash, ROOT);
    // Leaf 2 has no sibling at level 1 (odd count): a single step.
    const proof2 = await verifyInclusion(
      leaves[2]!,
      [{ lado: "IZQUIERDA", hash: bytesToHex(ab.hash), suma: "1000.50000000" }],
      ROOT,
      TOTAL,
      { engine, shape: { index: 2, leafCount: 3 } },
    );
    assert.ok(proof2.valid && proof2.shapeMatches === true, engine);
    assert.equal(proof2.steps.length, 1);
    assert.equal(proof2.steps[0]!.rightHash, proof2.leafHash);
  }
});

test("a position proves inclusion in its asset's tree, and only there", async () => {
  const b = await hashPositionLeaf(gold[1]!);
  const path: ProofStep[] = [{ lado: "DERECHA", hash: bytesToHex(b.hash), suma: "0.00000001" }];
  const shape = { index: 0, leafCount: 2 };
  const ok = await verifyPositionInclusion(gold[0]!, path, GOLD_ROOT, GOLD_TOTAL, { shape });
  assert.ok(ok.valid && ok.shapeMatches === true && ok.leafSum === "2.50000000");
  // Moved to the silver tree, or one unit heavier: the root no longer matches.
  assert.ok(!(await verifyPositionInclusion({ ...gold[0]!, ticker: "AGT1" }, path, GOLD_ROOT, GOLD_TOTAL)).valid);
  assert.ok(
    !(await verifyPositionInclusion({ ...gold[0]!, cantidad: "2.50000001" }, path, GOLD_ROOT, GOLD_TOTAL)).valid,
  );
  // A ticker that could break the encoding is refused outright.
  await assert.rejects(verifyPositionInclusion({ ...gold[0]!, ticker: "AUG1\u0000" }, path, GOLD_ROOT, GOLD_TOTAL));
  for (const t of ["AUG1", "CETES28", "A", "ABCDEFGHIJKL"]) assert.ok(isTicker(t), t);
  for (const t of ["", "aug1", "1AUG", "AUG-1", "ABCDEFGHIJKLM"]) assert.ok(!isTicker(t), t);
});

test("the expected path shape follows the backend's tree", () => {
  assert.deepEqual(expectedPathSides(0, 1), []);
  assert.deepEqual(expectedPathSides(0, 3), ["DERECHA", "DERECHA"]);
  assert.deepEqual(expectedPathSides(1, 3), ["IZQUIERDA", "DERECHA"]);
  assert.deepEqual(expectedPathSides(2, 3), ["IZQUIERDA"]);
  // The last of five leaves moves up twice without a sibling.
  assert.deepEqual(expectedPathSides(4, 5), ["IZQUIERDA"]);
  assert.deepEqual(expectedPathSides(5, 8), ["IZQUIERDA", "DERECHA", "IZQUIERDA"]);
  assert.throws(() => expectedPathSides(3, 3));
  assert.throws(() => expectedPathSides(-1, 3));
});

test("tampering is detected", async () => {
  const path = await pathOfLeaf0();
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
  // Right root and total, but a path that does not fit the leaf's claimed position.
  const shape = await verifyInclusion(leaves[0]!, path, ROOT, TOTAL, { shape: { index: 1, leafCount: 3 } });
  assert.ok(shape.rootMatches && shape.totalMatches && shape.shapeMatches === false && !shape.valid);
  // The bank's own leaf hash is compared too.
  const sent = await verifyInclusion({ ...leaves[0]!, hash: "00".repeat(32) }, path, ROOT, TOTAL);
  assert.equal(sent.leafHashMatches, false);
});

test("malformed proofs are refused, not rounded", async () => {
  const path = await pathOfLeaf0();
  await assert.rejects(verifyInclusion({ ...leaves[0]!, saldo: "1000.000000001" }, path, ROOT, TOTAL));
  await assert.rejects(verifyInclusion({ ...leaves[0]!, saldo: "-1.00000000" }, path, ROOT, TOTAL));
  await assert.rejects(verifyInclusion(leaves[0]!, [{ ...path[0]!, suma: "-0.00000001" }, path[1]!], ROOT, TOTAL));
  await assert.rejects(verifyInclusion(leaves[0]!, [{ ...path[0]!, hash: "zz" }, path[1]!], ROOT, TOTAL));
  await assert.rejects(verifyInclusion({ ...leaves[0]!, sal: "aa" }, path, ROOT, TOTAL));
  await assert.rejects(verifyInclusion({ ...leaves[0]!, cuenta_id: "not-a-uuid" }, path, ROOT, TOTAL));
  const long = Array.from({ length: 65 }, () => path[0]!);
  await assert.rejects(verifyInclusion(leaves[0]!, long, ROOT, TOTAL));
  // A sum that overflows u128 cannot be hashed.
  const max = { hash: new Uint8Array(32), sum: (1n << 128n) - 1n };
  await assert.rejects(combine(max, { hash: new Uint8Array(32), sum: 1n }));
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
