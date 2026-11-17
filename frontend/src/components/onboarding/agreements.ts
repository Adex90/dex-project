/**
 * The agreements a customer accepts when submitting the verification
 * (migration 0017): the account and custody agreement with Synthetic Capital,
 * S.A.P.I. de C.V., the privacy notice and the sworn statement. The backend
 * records the version in force (`contratos_vigentes` of the file) and sets
 * the instant itself, next to the IP and browser of the submission: a simple
 * electronic signature.
 *
 * Plain-language summaries for the review step; the legal texts are the
 * documents of that version. Client screens never use the words of an AML
 * hold (e2e/aml.mjs TIP_OFF).
 *
 * Pure data and functions with relative `.ts` imports, so `node --test` runs them.
 */

import type { AceptacionContratos, DocumentoContrato } from "../../lib/api/types.ts";

export interface Agreement {
  id: DocumentoContrato;
  title: string;
  /** The checkbox's label. */
  accept: string;
  /** What it says, in a few lines. */
  points: readonly string[];
}

export const AGREEMENTS: readonly Agreement[] = [
  {
    id: "CONTRATO_CUENTA",
    title: "Account and custody agreement",
    accept: "I accept the account and custody agreement with Synthetic Capital, S.A.P.I. de C.V.",
    points: [
      "Your vault is an electronic payment funds account (IFPE) held with Synthetic Capital, S.A.P.I. de C.V.",
      "Every peso is held 1:1 in custody and never lent. You can check the reserve at any time (Proof of Reserve).",
      "E-money balances earn no interest. Investing is a separate contract you choose each time.",
      "Withdrawals go only to bank accounts in your own name, by SPEI.",
      "Statements and notices reach you electronically, in the app.",
    ],
  },
  {
    id: "AVISO_PRIVACIDAD",
    title: "Privacy notice",
    accept: "I have read the privacy notice",
    points: [
      "Your identity details, the photos of your ID and the frames of your liveness check are used only to verify who you are and to meet Mexico's financial regulation.",
      "The law requires us to keep identification records for at least 10 years after the relationship ends.",
      "You can exercise your ARCO rights (access, rectification, cancellation and opposition) at any time.",
    ],
  },
  {
    id: "DECLARACION_VERACIDAD",
    title: "Sworn statement",
    accept: "I declare, under oath, that this information is true and that I act on my own behalf",
    points: [
      "The details and documents you provided are true and are your own.",
      "You act on your own behalf: the money in this account is yours, not anyone else's.",
      "You will tell us if any of it changes.",
    ],
  },
];

/** What POST /kyc/verificar takes, once every agreement is accepted; null until then. */
export function acceptanceFor(version: string, accepted: ReadonlySet<DocumentoContrato>): AceptacionContratos | null {
  if (AGREEMENTS.some((a) => !accepted.has(a.id))) return null;
  return { version, documentos: AGREEMENTS.map((a) => a.id) };
}
