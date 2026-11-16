/**
 * Withdrawals to a linked bank account (types/wealth.ts): validating what the
 * API answers, the amount a numpad builds against the available cash, and
 * the request it becomes.
 *
 * - Only the available cash can leave: cash held by open limit and stop
 *   orders cannot (the backend checks `saldo_disponible`, never
 *   `saldo_comprometido`). "Max" is the available cash rounded down to the
 *   cent, since SPEI settles in cents.
 * - One idempotency key per withdrawal: a retry of the same account and
 *   amount reuses it (`withdrawalSignature`), so a lost answer can never send
 *   the money twice.
 * - Network data is validated before anything renders it: a malformed
 *   payload is rejected whole.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { fromUnits, toUnits } from "./decimal.ts";
import { amountForApi, maxAmount, orderProblem, type OrderProblem } from "./market.ts";
import { isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type { DecimalString } from "../types/vault.ts";
import type {
  LinkedAccount,
  LinkedAccounts,
  Withdrawal,
  WithdrawalReceipt,
  WithdrawalRequest,
  WithdrawalStatus,
} from "../types/wealth.ts";

const STATUSES: readonly WithdrawalStatus[] = ["processing", "settled", "returned"];
/** A client links a handful of accounts at most; anything far beyond that is not a list. */
const MAX_ACCOUNTS = 20;
/** A SPEI tracking key: up to 30 capital letters and digits. */
const TRACKING_KEY_RE = /^[A-Z0-9]{1,30}$/;

export const STATUS_LABELS: Record<WithdrawalStatus, string> = {
  processing: "Processing",
  settled: "Settled",
  returned: "Returned",
};

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

/** One linked account; null if malformed. */
export function parseLinkedAccount(value: unknown): LinkedAccount | null {
  if (!isRecord(value)) return null;
  const { id, bank_code, bank_name, clabe_last4, holder_name, linked_at } = value;
  if (
    !isText(id, 64) ||
    typeof bank_code !== "string" ||
    !/^\d{3}$/.test(bank_code) ||
    !(bank_name === null || isText(bank_name, 80)) ||
    typeof clabe_last4 !== "string" ||
    !/^\d{4}$/.test(clabe_last4) ||
    !isText(holder_name, 120) ||
    !isInstant(linked_at)
  ) {
    return null;
  }
  return { id, bank_code, bank_name, clabe_last4, holder_name, linked_at };
}

/** `GET /api/v1/wealth/linked-accounts`; null if any account is malformed. */
export function parseLinkedAccounts(value: unknown): LinkedAccounts | null {
  if (!isRecord(value) || !Array.isArray(value.accounts) || value.accounts.length > MAX_ACCOUNTS) return null;
  const accounts: LinkedAccount[] = [];
  for (const raw of value.accounts) {
    const account = parseLinkedAccount(raw);
    if (!account) return null;
    accounts.push(account);
  }
  return { accounts };
}

/** `POST /api/v1/wealth/withdraw`; null if malformed. */
export function parseWithdrawalReceipt(value: unknown): WithdrawalReceipt | null {
  if (!isRecord(value) || !isRecord(value.withdrawal)) return null;
  const { id, status, amount, tracking_key, requested_at } = value.withdrawal;
  const linked_account = parseLinkedAccount(value.withdrawal.linked_account);
  if (
    !isText(id, 64) ||
    !isOneOf(STATUSES, status) ||
    !isPositive(amount) ||
    !linked_account ||
    typeof tracking_key !== "string" ||
    !TRACKING_KEY_RE.test(tracking_key) ||
    !isInstant(requested_at)
  ) {
    return null;
  }
  return { withdrawal: { id, status, amount, linked_account, tracking_key, requested_at } };
}

/** The bank's name, or its code when the catalog does not have it: "BBVA México", "Bank 137". */
export function bankName(account: Pick<LinkedAccount, "bank_code" | "bank_name">): string {
  return account.bank_name ?? `Bank ${account.bank_code}`;
}

/** The CLABE as the screens show it: "•••• 1234". */
export function maskedClabe(account: Pick<LinkedAccount, "clabe_last4">): string {
  return `•••• ${account.clabe_last4}`;
}

/** "BBVA México •••• 1234". */
export function accountLabel(account: Pick<LinkedAccount, "bank_code" | "bank_name" | "clabe_last4">): string {
  return `${bankName(account)} ${maskedClabe(account)}`;
}

export interface WithdrawalAssessment {
  /** Why it cannot be sent: nothing entered, or more than the available cash; null when it can. */
  problem: Exclude<OrderProblem, "too_small"> | null;
  /** The available cash rounded down to the cent, in the entry format ("" with nothing available). */
  max: string;
  /** The amount as the API takes it: "1250.50". */
  amount: DecimalString;
  /** The available cash after it leaves, never below zero. */
  remaining: DecimalString;
  ready: boolean;
}

/** An amount as typed ("1250.5") against the available cash. */
export function assessWithdrawal(amount: string, available: DecimalString): WithdrawalAssessment {
  const problem = orderProblem(amount, available);
  const forApi = amountForApi(amount);
  const left = toUnits(available) - toUnits(forApi);
  return {
    problem: problem === "too_small" ? null : problem,
    max: maxAmount(available),
    amount: forApi,
    remaining: fromUnits(left > 0n ? left : 0n),
    ready: problem === null,
  };
}

/** What makes two attempts the same withdrawal: the account and the amount to the cent. */
export function withdrawalSignature(accountId: string, amount: string): string {
  return `${accountId}|${amountForApi(amount)}`;
}

export function withdrawalRequest(accountId: string, amount: string, idempotencyKey: string): WithdrawalRequest {
  return { linked_account_id: accountId, amount: amountForApi(amount), idempotency_key: idempotencyKey };
}

/** The receipt's headline for each status. */
export function receiptTitle(withdrawal: Pick<Withdrawal, "status">): string {
  switch (withdrawal.status) {
    case "processing":
      return "Withdrawal processing";
    case "settled":
      return "Withdrawal settled";
    case "returned":
      return "Withdrawal returned";
  }
}
