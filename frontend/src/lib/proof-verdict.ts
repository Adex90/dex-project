/**
 * Verdict of the in-browser Proof of Reserve audit, as a pure function so it
 * can be unit-tested without a browser.
 *
 * Recomputing the Merkle path is not enough on its own. More checks keep a
 * dishonest server from fooling the customer:
 *
 * 1. **The leaf is this customer's.** The account id is hashed into every
 *    leaf. Without comparing it to the logged-in account, a bank that left
 *    this customer out could show them another customer's genuine leaf with
 *    the same balance.
 * 2. **The roots are the public ones.** The cut in the authenticated proof,
 *    its asset roots included, must match the copy anyone can fetch without
 *    logging in (`GET /reserva/cortes/{id}`). Otherwise the bank could build a
 *    private tree and root just for this customer.
 * 3. **The path has the leaf's shape.** Its left and right turns are the ones
 *    the leaf's index dictates in a tree of the published size.
 *
 * Positions (migration 0018) are judged one by one, each against its asset's
 * root; the audit passes only if the cash and every position pass.
 */
import { compareAmounts } from "./decimal.ts";

export type VerdictIssue =
  | "ROOT_MISMATCH"
  | "TOTAL_MISMATCH"
  | "LEAF_HASH_MISMATCH"
  | "PATH_SHAPE_MISMATCH"
  | "WRONG_ACCOUNT"
  | "RESERVE_SHORT"
  | "ASSET_ROOT_MISSING"
  | "PUBLIC_CUT_MISMATCH"
  | "PUBLIC_CUT_UNAVAILABLE";

export interface AssetAnchor {
  ticker: string;
  merkle_root: string;
  num_hojas: number;
  total_cantidad: string;
}

export interface CutAnchor {
  id: string;
  merkle_root: string;
  num_hojas: number;
  total_pasivo: string;
  total_reserva: string;
  secuencia_ledger: number;
  hash_ledger: string;
  /** Market ledger anchor and asset roots (0018); absent or empty before. */
  secuencia_mercado?: number | null;
  hash_mercado?: string | null;
  activos?: AssetAnchor[];
}

/** What the browser computed for one tree. */
export interface LocalCheck {
  valid: boolean;
  rootMatches: boolean;
  totalMatches: boolean;
  leafHashMatches: boolean | null;
  shapeMatches?: boolean | null;
}

export interface PositionInput {
  ticker: string;
  cuenta_id: string;
  local: LocalCheck;
}

export interface VerdictInput {
  local: LocalCheck;
  proof: { hoja: { cuenta_id: string }; corte: CutAnchor };
  positions?: PositionInput[];
  /** The logged-in customer's IFPE account id, from their own dashboard. */
  accountId: string | null;
  /** The same cut fetched from the public, unauthenticated endpoint. */
  publicCut: CutAnchor | null;
}

export interface PositionVerdict {
  ticker: string;
  ok: boolean;
  issues: VerdictIssue[];
}

export interface Verdict {
  ok: boolean;
  /** The cash leaf and the cut as a whole. */
  issues: VerdictIssue[];
  positions: PositionVerdict[];
}

function sameAssets(a: AssetAnchor[] = [], b: AssetAnchor[] = []): boolean {
  if (a.length !== b.length) return false;
  const byTicker = new Map(b.map((x) => [x.ticker, x]));
  return a.every((x) => {
    const y = byTicker.get(x.ticker);
    return (
      y !== undefined &&
      x.merkle_root === y.merkle_root &&
      x.num_hojas === y.num_hojas &&
      compareAmounts(x.total_cantidad, y.total_cantidad) === 0
    );
  });
}

function sameCut(a: CutAnchor, b: CutAnchor): boolean {
  return (
    a.id === b.id &&
    a.merkle_root === b.merkle_root &&
    a.num_hojas === b.num_hojas &&
    compareAmounts(a.total_pasivo, b.total_pasivo) === 0 &&
    compareAmounts(a.total_reserva, b.total_reserva) === 0 &&
    a.secuencia_ledger === b.secuencia_ledger &&
    a.hash_ledger === b.hash_ledger &&
    (a.secuencia_mercado ?? null) === (b.secuencia_mercado ?? null) &&
    (a.hash_mercado ?? null) === (b.hash_mercado ?? null) &&
    sameAssets(a.activos, b.activos)
  );
}

function treeIssues(local: LocalCheck, cuentaId: string, accountId: string | null): VerdictIssue[] {
  const issues: VerdictIssue[] = [];
  if (!local.rootMatches) issues.push("ROOT_MISMATCH");
  if (!local.totalMatches) issues.push("TOTAL_MISMATCH");
  if (local.leafHashMatches === false) issues.push("LEAF_HASH_MISMATCH");
  if (local.shapeMatches === false) issues.push("PATH_SHAPE_MISMATCH");
  if (!accountId || cuentaId.toLowerCase() !== accountId.toLowerCase()) issues.push("WRONG_ACCOUNT");
  return issues;
}

export function proofVerdict({ local, proof, positions = [], accountId, publicCut }: VerdictInput): Verdict {
  const issues = treeIssues(local, proof.hoja.cuenta_id, accountId);
  if (compareAmounts(proof.corte.total_reserva, proof.corte.total_pasivo) < 0) issues.push("RESERVE_SHORT");
  if (!publicCut) issues.push("PUBLIC_CUT_UNAVAILABLE");
  else if (!sameCut(proof.corte, publicCut)) issues.push("PUBLIC_CUT_MISMATCH");

  const roots = new Set((proof.corte.activos ?? []).map((a) => a.ticker));
  const perPosition = positions.map((p) => {
    const found = treeIssues(p.local, p.cuenta_id, accountId);
    if (!roots.has(p.ticker)) found.push("ASSET_ROOT_MISSING");
    return { ticker: p.ticker, ok: p.local.valid && found.length === 0, issues: found };
  });
  return { ok: local.valid && issues.length === 0 && perPosition.every((p) => p.ok), issues, positions: perPosition };
}

/** Customer-facing explanation of each failed check. */
export const ISSUE_MESSAGES: Record<VerdictIssue, string> = {
  ROOT_MISMATCH: "The computed root does not match the published root.",
  TOTAL_MISMATCH: "The tree's total does not match the published total.",
  LEAF_HASH_MISMATCH: "Your leaf does not hash to the value the bank sent.",
  PATH_SHAPE_MISMATCH: "The path's left and right turns do not match your leaf's position in the tree.",
  WRONG_ACCOUNT: "The proof is for a different account than yours.",
  RESERVE_SHORT: "The published cash in custody is less than total liabilities.",
  ASSET_ROOT_MISSING: "The snapshot publishes no root for this asset.",
  PUBLIC_CUT_MISMATCH: "The snapshot in your proof differs from the bank's public copy of it.",
  PUBLIC_CUT_UNAVAILABLE:
    "The bank's public copy of this snapshot could not be fetched, so the root cannot be confirmed.",
};
