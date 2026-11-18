/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { proofVerdict, type CutAnchor, type LocalCheck, type VerdictInput } from "./proof-verdict.ts";

const cut: CutAnchor = {
  id: "c0000000-0000-4000-8000-000000000001",
  merkle_root: "ab".repeat(32),
  num_hojas: 3,
  total_pasivo: "1000.50000000",
  total_reserva: "1000.50000000",
  secuencia_ledger: 14,
  hash_ledger: "cd".repeat(32),
  secuencia_mercado: 2,
  hash_mercado: "ef".repeat(32),
  activos: [
    { ticker: "AGT1", merkle_root: "01".repeat(32), num_hojas: 1, total_cantidad: "12.00000000" },
    { ticker: "AUG1", merkle_root: "02".repeat(32), num_hojas: 2, total_cantidad: "2.53100000" },
  ],
};
const ACCOUNT = "a0000000-0000-4000-8000-000000000001";
const passed = (): LocalCheck => ({
  valid: true,
  rootMatches: true,
  totalMatches: true,
  leafHashMatches: true,
  shapeMatches: true,
});

const honest = (): VerdictInput => ({
  local: passed(),
  proof: { hoja: { cuenta_id: ACCOUNT }, corte: structuredClone(cut) },
  positions: [
    { ticker: "AGT1", cuenta_id: ACCOUNT, local: passed() },
    { ticker: "AUG1", cuenta_id: ACCOUNT, local: passed() },
  ],
  accountId: ACCOUNT,
  // The public copy, with the same amounts written differently and the assets in another order.
  publicCut: { ...structuredClone(cut), total_pasivo: "1000.5", activos: [...structuredClone(cut.activos!)].reverse() },
});

test("an honest proof for the customer's own account and positions verifies", () => {
  const v = proofVerdict(honest());
  assert.deepEqual(v, {
    ok: true,
    issues: [],
    positions: [
      { ticker: "AGT1", ok: true, issues: [] },
      { ticker: "AUG1", ok: true, issues: [] },
    ],
  });
});

test("a cut from before positions (no anchor, no asset roots) still verifies the cash", () => {
  const old: CutAnchor = { ...cut, secuencia_mercado: null, hash_mercado: null, activos: [] };
  const v = proofVerdict({
    ...honest(),
    proof: { hoja: { cuenta_id: ACCOUNT }, corte: old },
    positions: [],
    publicCut: old,
  });
  assert.deepEqual(v, { ok: true, issues: [], positions: [] });
});

test("another customer's genuine leaf is rejected", () => {
  const input = honest();
  input.proof.hoja.cuenta_id = "b0000000-0000-4000-8000-000000000002";
  assert.deepEqual(proofVerdict(input).issues, ["WRONG_ACCOUNT"]);
  assert.equal(proofVerdict({ ...honest(), accountId: null }).ok, false);
  // The same for a position.
  const position = honest();
  position.positions![1]!.cuenta_id = "b0000000-0000-4000-8000-000000000002";
  const v = proofVerdict(position);
  assert.equal(v.ok, false);
  assert.deepEqual(v.positions[1], { ticker: "AUG1", ok: false, issues: ["WRONG_ACCOUNT"] });
});

test("a private root that differs from the public cut is rejected", () => {
  const input = honest();
  input.proof.corte.merkle_root = "ef".repeat(32);
  assert.deepEqual(proofVerdict(input).issues, ["PUBLIC_CUT_MISMATCH"]);
  const total = honest();
  total.publicCut = { ...cut, total_pasivo: "1001.50000000" };
  assert.deepEqual(proofVerdict(total).issues, ["PUBLIC_CUT_MISMATCH"]);
});

test("a private asset root, a missing one or a different market anchor is rejected", () => {
  const root = honest();
  root.proof.corte.activos![1]!.merkle_root = "ff".repeat(32);
  assert.deepEqual(proofVerdict(root).issues, ["PUBLIC_CUT_MISMATCH"]);
  const total = honest();
  total.proof.corte.activos![0]!.total_cantidad = "11.00000000";
  assert.deepEqual(proofVerdict(total).issues, ["PUBLIC_CUT_MISMATCH"]);
  const dropped = honest();
  dropped.publicCut!.activos = dropped.publicCut!.activos!.slice(1);
  assert.deepEqual(proofVerdict(dropped).issues, ["PUBLIC_CUT_MISMATCH"]);
  const anchor = honest();
  anchor.publicCut!.secuencia_mercado = 1;
  assert.deepEqual(proofVerdict(anchor).issues, ["PUBLIC_CUT_MISMATCH"]);
});

test("a position whose asset has no root in the cut fails on its own", () => {
  const input = honest();
  input.positions!.push({
    ticker: "CETES28",
    cuenta_id: ACCOUNT,
    local: { ...passed(), valid: false, rootMatches: false },
  });
  const v = proofVerdict(input);
  assert.equal(v.ok, false);
  assert.deepEqual(v.issues, []);
  assert.deepEqual(v.positions[2], { ticker: "CETES28", ok: false, issues: ["ROOT_MISMATCH", "ASSET_ROOT_MISSING"] });
});

test("without the public cut the verdict cannot be positive", () => {
  const v = proofVerdict({ ...honest(), publicCut: null });
  assert.equal(v.ok, false);
  assert.deepEqual(v.issues, ["PUBLIC_CUT_UNAVAILABLE"]);
});

test("path, shape, leaf hash and reserve failures are reported", () => {
  const input = honest();
  input.local = { valid: false, rootMatches: false, totalMatches: false, leafHashMatches: false, shapeMatches: false };
  input.proof.corte.total_reserva = "1000.00000000";
  input.publicCut = structuredClone(input.proof.corte);
  assert.deepEqual(proofVerdict(input).issues, [
    "ROOT_MISMATCH",
    "TOTAL_MISMATCH",
    "LEAF_HASH_MISMATCH",
    "PATH_SHAPE_MISMATCH",
    "RESERVE_SHORT",
  ]);
  const position = honest();
  position.positions![0]!.local = { ...passed(), valid: false, shapeMatches: false };
  assert.deepEqual(proofVerdict(position).positions[0], { ticker: "AGT1", ok: false, issues: ["PATH_SHAPE_MISMATCH"] });
});
