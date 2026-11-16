/**
 * Wire types of the wealth routes (backend/src/routes/patrimonio.rs): the
 * bank accounts linked for withdrawals, and a withdrawal.
 */

import type { DecimalString } from "./vault";

/**
 * A bank account linked for withdrawals: always in the client's own name
 * (`GET /api/v1/wealth/linked-accounts`). The full CLABE never leaves the
 * API; its last four digits are enough to recognize it.
 */
export interface LinkedAccount {
  id: string;
  /** The bank's 3-digit code, the CLABE's prefix: "012". */
  bank_code: string;
  /** The bank's name when the backend's short catalog has it ("BBVA México"), else null. */
  bank_name: string | null;
  clabe_last4: string;
  /** The account holder: the client, with the name of their verified identity. */
  holder_name: string;
  /** ISO 8601. */
  linked_at: string;
}

export interface LinkedAccounts {
  /** Active accounts, oldest first. */
  accounts: LinkedAccount[];
}

/** `POST /api/v1/wealth/withdraw`. */
export interface WithdrawalRequest {
  linked_account_id: string;
  /** MXN, to the cent: "1250.50". */
  amount: DecimalString;
  idempotency_key: string;
}

/**
 * - processing: the amount left the available cash and travels by SPEI
 *   (the order is pending or sent).
 * - settled: it reached the bank.
 * - returned: the bank rejected or returned it, and it is back in the vault.
 */
export type WithdrawalStatus = "processing" | "settled" | "returned";

export interface Withdrawal {
  /** The SPEI order. */
  id: string;
  status: WithdrawalStatus;
  amount: DecimalString;
  linked_account: LinkedAccount;
  /** The SPEI tracking key (clave de rastreo): with it the receiving bank finds the payment. */
  tracking_key: string;
  /** ISO 8601. */
  requested_at: string;
}

/** The answer to a withdrawal: 201 when new, 200 when its idempotency key repeats it. */
export interface WithdrawalReceipt {
  withdrawal: Withdrawal;
}
