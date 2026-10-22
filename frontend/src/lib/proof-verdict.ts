/**
 * Verdict of the in-browser Proof of Reserve audit, as a pure function so it
 * can be unit-tested without a browser.
 *
 * Recomputing the Merkle path is not enough on its own. Two more checks keep a
 * dishonest server from fooling the customer:
 *
 * 1. **The leaf is this customer's.** The account id is hashed into the leaf.
 *    Without comparing it to the logged-in account, a bank that left this
 *    customer out could show them another customer's genuine leaf with the
 *    same balance.
 * 2. **The root is the public one.** The cut in the authenticated proof must
 *    match the copy anyone can fetch without logging in
 *    (`GET /reserva/cortes/{id}`). Otherwise the bank could build a private
 *    tree and root just for this customer.
 */
import { compareAmounts } from "./decimal.ts";

export type VerdictIssue =
  | "ROOT_MISMATCH"
  | "TOTAL_MISMATCH"
  | "LEAF_HASH_MISMATCH"
  | "WRONG_ACCOUNT"
  | "RESERVE_SHORT"
  | "PUBLIC_CUT_MISMATCH"
  | "PUBLIC_CUT_UNAVAILABLE";

export interface CutAnchor {
  id: string;
  merkle_root: string;
  num_hojas: number;
  total_pasivo: string;
  total_reserva: string;
  secuencia_ledger: number;
  hash_ledger: string;
}

export interface VerdictInput {
  local: { valid: boolean; rootMatches: boolean; totalMatches: boolean; leafHashMatches: boolean | null };
  proof: { hoja: { cuenta_id: string }; corte: CutAnchor };
  /** The logged-in customer's IFPE account id, from their own dashboard. */
  accountId: string | null;
  /** The same cut fetched from the public, unauthenticated endpoint. */
  publicCut: CutAnchor | null;
}

export interface Verdict {
  ok: boolean;
  issues: VerdictIssue[];
}

function sameCut(a: CutAnchor, b: CutAnchor): boolean {
  return (
    a.id === b.id &&
    a.merkle_root === b.merkle_root &&
    a.num_hojas === b.num_hojas &&
    compareAmounts(a.total_pasivo, b.total_pasivo) === 0 &&
    compareAmounts(a.total_reserva, b.total_reserva) === 0 &&
    a.secuencia_ledger === b.secuencia_ledger &&
    a.hash_ledger === b.hash_ledger
  );
}

export function proofVerdict({ local, proof, accountId, publicCut }: VerdictInput): Verdict {
  const issues: VerdictIssue[] = [];
  if (!local.rootMatches) issues.push("ROOT_MISMATCH");
  if (!local.totalMatches) issues.push("TOTAL_MISMATCH");
  if (local.leafHashMatches === false) issues.push("LEAF_HASH_MISMATCH");
  if (!accountId || proof.hoja.cuenta_id.toLowerCase() !== accountId.toLowerCase()) issues.push("WRONG_ACCOUNT");
  if (compareAmounts(proof.corte.total_reserva, proof.corte.total_pasivo) < 0) issues.push("RESERVE_SHORT");
  if (!publicCut) issues.push("PUBLIC_CUT_UNAVAILABLE");
  else if (!sameCut(proof.corte, publicCut)) issues.push("PUBLIC_CUT_MISMATCH");
  return { ok: local.valid && issues.length === 0, issues };
}

/** Customer-facing explanation of each failed check. */
export const ISSUE_MESSAGES: Record<VerdictIssue, string> = {
  ROOT_MISMATCH: "The computed root does not match the published root.",
  TOTAL_MISMATCH: "The tree's total does not match the published liabilities.",
  LEAF_HASH_MISMATCH: "Your leaf does not hash to the value the bank sent.",
  WRONG_ACCOUNT: "The proof is for a different account than yours.",
  RESERVE_SHORT: "The published cash in custody is less than total liabilities.",
  PUBLIC_CUT_MISMATCH: "The snapshot in your proof differs from the bank's public copy of it.",
  PUBLIC_CUT_UNAVAILABLE: "The bank's public copy of this snapshot could not be fetched, so the root cannot be confirmed.",
};
