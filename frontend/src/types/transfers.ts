/**
 * Wire types of the SPEI tracker and its clarifications (roadmap item 17,
 * migration 0025; backend/src/patrimonio/transferencias.rs and
 * aclaraciones.rs):
 *
 * - `GET /api/v1/wealth/spei/transfers`: the client's SPEI transfers, in and
 *   out, newest first, by pages (`SpeiTransfers`).
 * - `GET /api/v1/wealth/spei/transfers/{id}`: one, by its id or by the id of
 *   a ledger movement it made (`TrackedTransfer`).
 * - `GET /api/v1/wealth/spei/disputes`: the client's clarifications
 *   (`SpeiClarifications`); `POST` opens one (`SpeiClarificationRequest`).
 *
 * Validated whole by lib/transfers.ts before anything renders them.
 */

import type { DecimalString } from "./vault";

export type TransferDirection = "incoming" | "outgoing";

/** A deposit to the client's CLABE, a withdrawal to their linked account, or a payment to someone else. */
export type TransferKind = "deposit" | "withdrawal" | "payment";

/** Where the money is: still on its way, settled, or back where it came from. */
export type TransferStatus = "pending" | "settled" | "returned";

/**
 * How far it got, for the timeline:
 * - initiated: an order registered, waiting to be sent (outgoing only).
 * - sent: the settlement provider (STP) has it, on Banxico's SPEI network (outgoing only).
 * - settled: the receiving bank credited it (for a deposit: credited to the vault).
 * - returned: it settled and came back: the receiving bank returned an order,
 *   or a deposit the account could not receive went back to the sender.
 * - rejected: an order that never settled (outgoing only); the money is back in the vault.
 */
export type TransferStage = "initiated" | "sent" | "settled" | "returned" | "rejected";

/** Why the receiving bank returned an outgoing transfer (Banxico's return causes 1, 2, 3 and 14; the rest are "other"). */
export type ReturnReason = "account_not_found" | "account_blocked" | "account_closed" | "missing_information" | "other";

/** A bank: the 3-digit code of its CLABEs and its name when the backend's short catalog has it. */
export interface TransferBank {
  code: string | null;
  name: string | null;
}

/** The other side: for a deposit, who sent it; for a withdrawal or a payment, who received it. */
export interface TransferCounterparty {
  name: string | null;
  bank: TransferBank;
  /** Another person's full account number never leaves the API: only its last four digits. */
  account_last4: string | null;
}

/**
 * What Banxico's CEP (Comprobante Electrónico de Pago) lookup asks for, for a
 * transfer that settled. A simulated transfer (demo mode) has no CEP at
 * Banxico: `available` is false.
 */
export interface CepData {
  available: boolean;
  /** YYYY-MM-DD: the operation date the payment system assigned. */
  operation_date: string | null;
  tracking_key: string;
  sender_bank: TransferBank;
  receiver_bank: TransferBank;
  /** The client's own CLABE, in full, when they are the beneficiary (a deposit); null otherwise. */
  beneficiary_account: string | null;
  beneficiary_account_last4: string | null;
  amount: DecimalString;
  /** Banxico's CEP page. */
  url: string;
}

/** Where a clarification is: received, being reviewed, or closed (resolved or declined). */
export type ClarificationStatus = "open" | "in_review" | "resolved" | "declined";

/** The latest clarification of a transfer, on the transfer. */
export interface ClarificationSummary {
  clarification_id: string;
  /** Its case number: the client quotes it when talking about it. */
  number: number;
  status: ClarificationStatus;
}

/** One SPEI transfer of the client, in or out, with its settlement steps. */
export interface TrackedTransfer {
  transfer_id: string;
  direction: TransferDirection;
  kind: TransferKind;
  status: TransferStatus;
  stage: TransferStage;
  /** MXN, to the cent. */
  amount: DecimalString;
  /** The clave de rastreo: up to 30 capital letters and digits. */
  tracking_key: string;
  /** YYYY-MM-DD; null until the payment system assigns it (and for a demo deposit). */
  operation_date: string | null;
  concept: string | null;
  numeric_reference: number | null;
  counterparty: TransferCounterparty;
  /** ISO 8601: when the order was placed, or when the deposit arrived. */
  initiated_at: string;
  sent_at: string | null;
  settled_at: string | null;
  returned_at: string | null;
  /** Outgoing transfers that came back only. */
  return_reason: ReturnReason | null;
  /** The ledger movement it made: an order's charge, a deposit's credit. */
  transaction_id: string | null;
  /** The refund of an order that came back. */
  refund_transaction_id: string | null;
  /** Demo mode: no real bank or Banxico took part. */
  simulated: boolean;
  /** Once it settled; null before (and for a deposit that was never credited). */
  cep: CepData | null;
  clarification: ClarificationSummary | null;
}

export interface SpeiTransfers {
  transfers: TrackedTransfer[];
  /** For the next page; null on the last one. */
  next_cursor: string | null;
}

/** Why the client opens a clarification. */
export type ClarificationReason = "delayed" | "not_received" | "returned" | "wrong_amount" | "not_recognized" | "other";

/** A step of a clarification's history, from the immutable security log. */
export interface ClarificationStep {
  status: ClarificationStatus;
  /** ISO 8601. */
  at: string;
}

/** A clarification as its client sees it. */
export interface SpeiClarification {
  clarification_id: string;
  number: number;
  transfer_id: string;
  direction: TransferDirection;
  tracking_key: string;
  reason: ClarificationReason;
  description: string | null;
  status: ClarificationStatus;
  /** Written by the team that closes it (the database keeps it neutral). */
  resolution_note: string | null;
  opened_at: string;
  updated_at: string;
  closed_at: string | null;
  /** Opened, in review, closed: when, oldest first. */
  history: ClarificationStep[];
}

export interface SpeiClarifications {
  /** Newest first. */
  clarifications: SpeiClarification[];
}

/** `POST /api/v1/wealth/spei/disputes`. */
export interface SpeiClarificationRequest {
  transfer_id: string;
  direction: TransferDirection;
  reason: ClarificationReason;
  /** Optional, except with "other" (10 to 500 characters). */
  description?: string;
  idempotency_key: string;
}
