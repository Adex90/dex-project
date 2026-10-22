/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { proofVerdict, type CutAnchor, type VerdictInput } from "./proof-verdict.ts";

const cut: CutAnchor = {
  id: "c0000000-0000-4000-8000-000000000001",
  merkle_root: "ab".repeat(32),
  num_hojas: 3,
  total_pasivo: "1000.50000000",
  total_reserva: "1000.50000000",
  secuencia_ledger: 14,
  hash_ledger: "cd".repeat(32),
};
const ACCOUNT = "a0000000-0000-4000-8000-000000000001";

const honest = (): VerdictInput => ({
  local: { valid: true, rootMatches: true, totalMatches: true, leafHashMatches: true },
  proof: { hoja: { cuenta_id: ACCOUNT }, corte: { ...cut } },
  accountId: ACCOUNT,
  publicCut: { ...cut, total_pasivo: "1000.5" },
});

test("an honest proof for the customer's own account verifies", () => {
  assert.deepEqual(proofVerdict(honest()), { ok: true, issues: [] });
});

test("another customer's genuine leaf is rejected", () => {
  const input = honest();
  input.proof.hoja.cuenta_id = "b0000000-0000-4000-8000-000000000002";
  assert.deepEqual(proofVerdict(input), { ok: false, issues: ["WRONG_ACCOUNT"] });
  assert.equal(proofVerdict({ ...honest(), accountId: null }).ok, false);
});

test("a private root that differs from the public cut is rejected", () => {
  const input = honest();
  input.proof.corte.merkle_root = "ef".repeat(32);
  assert.deepEqual(proofVerdict(input).issues, ["PUBLIC_CUT_MISMATCH"]);
  const total = honest();
  total.publicCut = { ...cut, total_pasivo: "1001.50000000" };
  assert.deepEqual(proofVerdict(total).issues, ["PUBLIC_CUT_MISMATCH"]);
});

test("without the public cut the verdict cannot be positive", () => {
  assert.deepEqual(proofVerdict({ ...honest(), publicCut: null }), { ok: false, issues: ["PUBLIC_CUT_UNAVAILABLE"] });
});

test("path, leaf hash and reserve failures are reported", () => {
  const input = honest();
  input.local = { valid: false, rootMatches: false, totalMatches: false, leafHashMatches: false };
  input.proof.corte.total_reserva = "1000.00000000";
  input.publicCut = { ...input.proof.corte };
  assert.deepEqual(proofVerdict(input).issues, ["ROOT_MISMATCH", "TOTAL_MISMATCH", "LEAF_HASH_MISMATCH", "RESERVE_SHORT"]);
});
