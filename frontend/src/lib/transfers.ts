/**
 * The SPEI tracker and its clarifications (roadmap item 17, migration 0025;
 * types/transfers.ts): validating what the API answers, and the words, the
 * settlement timeline and the clarification rules the Transfers page uses.
 *
 * - Every payload is validated whole, coherence included (a settled transfer
 *   has its settlement time and its CEP data, only an outgoing one that came
 *   back has a return reason, a closed clarification has its note): a
 *   malformed one is rejected, never half rendered.
 * - Neutral words only: a deposit the account could not receive "went back to
 *   the sending bank", with no cause; a clarification is "in review", never
 *   anything that names why an account could be watched (no tipping off).
 * - The CEP link may only point at Banxico's own site.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { formatAmount } from "./decimal.ts";
import { isOneOf, isPositive, isRecord, isText } from "./vault.ts";
import type {
  CepData,
  ClarificationReason,
  ClarificationStatus,
  ClarificationStep,
  ClarificationSummary,
  ReturnReason,
  SpeiClarification,
  SpeiClarificationRequest,
  SpeiClarifications,
  SpeiTransfers,
  TrackedTransfer,
  TransferBank,
  TransferCounterparty,
  TransferDirection,
  TransferKind,
  TransferStage,
  TransferStatus,
} from "../types/transfers.ts";

/** The Transfers page, in the Account group. */
export const TRANSFERS_PATH = "/dashboard/transfers";
/** Banxico's CEP lookup: the only site a CEP link may open. */
export const CEP_ORIGIN = "https://www.banxico.org.mx";
/** Calendar days after the transfer to open a clarification (`ifpe.plazo_aclaracion_dias()`). */
export const CLARIFICATION_WINDOW_DAYS = 90;
/** Open or in-review clarifications per client, at most (`ifpe.maximo_aclaraciones_activas()`). */
export const MAX_ACTIVE_CLARIFICATIONS = 10;
/** The description, at most; with "other" it is required, at least `MIN_OTHER_DESCRIPTION`. */
export const MAX_DESCRIPTION = 500;
export const MIN_OTHER_DESCRIPTION = 10;

const DIRECTIONS: readonly TransferDirection[] = ["incoming", "outgoing"];
const KINDS: readonly TransferKind[] = ["deposit", "withdrawal", "payment"];
const STATUSES: readonly TransferStatus[] = ["pending", "settled", "returned"];
const STAGES: readonly TransferStage[] = ["initiated", "sent", "settled", "returned", "rejected"];
const RETURN_REASONS: readonly ReturnReason[] = [
  "account_not_found",
  "account_blocked",
  "account_closed",
  "missing_information",
  "other",
];
const CLARIFICATION_STATUSES: readonly ClarificationStatus[] = ["open", "in_review", "resolved", "declined"];
export const CLARIFICATION_REASONS: readonly ClarificationReason[] = [
  "delayed",
  "not_received",
  "returned",
  "wrong_amount",
  "not_recognized",
  "other",
];
/** The stages each status allows. */
const STAGES_OF: Readonly<Record<TransferStatus, readonly TransferStage[]>> = {
  pending: ["initiated", "sent"],
  settled: ["settled"],
  returned: ["returned", "rejected"],
};

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** A SPEI tracking key: up to 30 capital letters and digits. */
const TRACKING_KEY_RE = /^[A-Z0-9]{1,30}$/;
const BANK_CODE_RE = /^[0-9]{3}$/;
const LAST4_RE = /^[0-9]{4}$/;
/** A CLABE (18 digits) or another account number SPEI carries (up to 20). */
const ACCOUNT_RE = /^[0-9]{1,20}$/;
const DATE_RE = /^\d{4}-\d{2}-\d{2}$/;
/** The backend's pages hold at most 100 transfers. */
const MAX_PAGE = 100;
/** The backend lists the client's 200 latest clarifications. */
const MAX_CLARIFICATIONS = 200;
/** Opened, in review, closed: a handful of steps at most. */
const MAX_HISTORY = 10;
const MAX_CASE_NUMBER = 999_999;
/** A deposit's sender and concept as the other bank sent them (migration 0005: 120 and 210 characters). */
const MAX_NAME = 120;
const MAX_CONCEPT = 210;
const DAY_MS = 86_400_000;

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

export function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID_RE.test(value);
}

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

/** A calendar date, "YYYY-MM-DD", that exists. */
function isIsoDate(value: unknown): value is string {
  if (typeof value !== "string" || !DATE_RE.test(value)) return false;
  const ms = Date.parse(`${value}T00:00:00Z`);
  return !Number.isNaN(ms) && new Date(ms).toISOString().slice(0, 10) === value;
}

function isNullable<T>(value: unknown, check: (v: unknown) => v is T): value is T | null {
  return value === null || check(value);
}

function isMatch(re: RegExp) {
  return (value: unknown): value is string => typeof value === "string" && re.test(value);
}

function isWholeNumber(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
}

/**
 * Free text as the other side sent it: null, or trimmed (blank becomes null);
 * undefined if malformed. At most `max` characters, counted as PostgreSQL and
 * the backend count them (code points, not UTF-16 units).
 */
function optionalText(value: unknown, max: number): string | null | undefined {
  if (value === null) return null;
  if (typeof value !== "string" || value.length > 2 * max || characterCount(value) > max) return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

/** Banxico's CEP page, and nothing else: https on Banxico's own host. */
export function isCepUrl(value: unknown): value is string {
  if (!isText(value, 200)) return false;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.origin === CEP_ORIGIN && url.username === "" && url.password === "";
  } catch {
    return false;
  }
}

function parseBank(value: unknown): TransferBank | null {
  if (!isRecord(value)) return null;
  const { code, name } = value;
  if (!isNullable(code, isMatch(BANK_CODE_RE)) || !isNullable(name, (v): v is string => isText(v, 80))) return null;
  // A name comes from the catalog of codes: no name without its code.
  if (name !== null && code === null) return null;
  return { code, name };
}

function parseCounterparty(value: unknown): TransferCounterparty | null {
  if (!isRecord(value)) return null;
  const name = optionalText(value.name, MAX_NAME);
  const bank = parseBank(value.bank);
  const { account_last4 } = value;
  if (name === undefined || !bank || !isNullable(account_last4, isMatch(LAST4_RE))) return null;
  return { name, bank, account_last4 };
}

function parseCep(
  value: unknown,
  transfer: Pick<TrackedTransfer, "direction" | "tracking_key" | "amount" | "simulated">,
) {
  if (!isRecord(value)) return null;
  const { available, operation_date, tracking_key, beneficiary_account, beneficiary_account_last4, amount, url } =
    value;
  const sender_bank = parseBank(value.sender_bank);
  const receiver_bank = parseBank(value.receiver_bank);
  if (
    typeof available !== "boolean" ||
    !isNullable(operation_date, isIsoDate) ||
    tracking_key !== transfer.tracking_key ||
    !sender_bank ||
    !receiver_bank ||
    !isNullable(beneficiary_account, isMatch(ACCOUNT_RE)) ||
    !isNullable(beneficiary_account_last4, isMatch(LAST4_RE)) ||
    amount !== transfer.amount ||
    !isCepUrl(url)
  ) {
    return null;
  }
  const coherent =
    // Banxico finds a CEP by its operation date; a simulated transfer has none there.
    (!available || (operation_date !== null && !transfer.simulated)) &&
    // The full account only when it is the client's own: a deposit's.
    (transfer.direction === "incoming") === (beneficiary_account !== null) &&
    (beneficiary_account === null ||
      beneficiary_account.length < 4 ||
      beneficiary_account_last4 === beneficiary_account.slice(-4));
  if (!coherent) return null;
  const cep: CepData = {
    available,
    operation_date,
    tracking_key,
    sender_bank,
    receiver_bank,
    beneficiary_account,
    beneficiary_account_last4,
    amount,
    url,
  };
  return cep;
}

/** A transfer's latest clarification: null when it has none; undefined when malformed. */
function parseSummary(value: unknown): ClarificationSummary | null | undefined {
  if (value === null) return null;
  if (!isRecord(value)) return undefined;
  const { clarification_id, number, status } = value;
  if (!isUuid(clarification_id) || !isWholeNumber(number, 1, MAX_CASE_NUMBER)) return undefined;
  if (!isOneOf(CLARIFICATION_STATUSES, status)) return undefined;
  return { clarification_id, number, status };
}

/**
 * One transfer; null if malformed or incoherent. Beyond each field's shape:
 * a deposit is the only incoming kind; the stage belongs to the status (and
 * only orders wait, travel or fail to go out); the times the database
 * guarantees are there (an order sent or settled has its sent time, a
 * settled one its settlement time, one that came back its return time); the
 * CEP data exist exactly when it settled; only an outgoing transfer that
 * came back has a return reason and a refund; an order always has its
 * charge, and a deposit its credit exactly when it was credited.
 */
export function parseTrackedTransfer(value: unknown): TrackedTransfer | null {
  if (!isRecord(value)) return null;
  const { transfer_id, direction, kind, status, stage, amount, tracking_key, operation_date, numeric_reference } =
    value;
  const { initiated_at, sent_at, settled_at, returned_at, return_reason, transaction_id, refund_transaction_id } =
    value;
  const { simulated } = value;
  const concept = optionalText(value.concept, MAX_CONCEPT);
  const counterparty = parseCounterparty(value.counterparty);
  const clarification = parseSummary(value.clarification);
  if (
    !isUuid(transfer_id) ||
    !isOneOf(DIRECTIONS, direction) ||
    !isOneOf(KINDS, kind) ||
    !isOneOf(STATUSES, status) ||
    !isOneOf(STAGES, stage) ||
    !isPositive(amount) ||
    !isMatch(TRACKING_KEY_RE)(tracking_key) ||
    !isNullable(operation_date, isIsoDate) ||
    concept === undefined ||
    !isNullable(numeric_reference, (v): v is number => isWholeNumber(v, 0, 9_999_999)) ||
    !counterparty ||
    !isInstant(initiated_at) ||
    !isNullable(sent_at, isInstant) ||
    !isNullable(settled_at, isInstant) ||
    !isNullable(returned_at, isInstant) ||
    !isNullable(return_reason, (v): v is ReturnReason => isOneOf(RETURN_REASONS, v)) ||
    !isNullable(transaction_id, isUuid) ||
    !isNullable(refund_transaction_id, isUuid) ||
    typeof simulated !== "boolean" ||
    clarification === undefined
  ) {
    return null;
  }
  const outgoing = direction === "outgoing";
  const cameBack = outgoing && status === "returned";
  const cep = value.cep === null ? null : parseCep(value.cep, { direction, tracking_key, amount, simulated });
  if (value.cep !== null && cep === null) return null;
  const coherent =
    (kind === "deposit") === !outgoing &&
    STAGES_OF[status].includes(stage) &&
    (outgoing || (stage !== "initiated" && stage !== "sent" && stage !== "rejected")) &&
    (outgoing ? stage === "initiated" || stage === "rejected" || sent_at !== null : sent_at === null) &&
    (outgoing
      ? (stage !== "settled" && stage !== "returned") || settled_at !== null
      : (stage === "settled") === (settled_at !== null)) &&
    (stage === "returned" || stage === "rejected") === (returned_at !== null) &&
    (cep !== null) === (settled_at !== null) &&
    (return_reason !== null) === cameBack &&
    (refund_transaction_id !== null) === cameBack &&
    (outgoing ? transaction_id !== null : (transaction_id !== null) === (status === "settled"));
  if (!coherent) return null;
  return {
    transfer_id,
    direction,
    kind,
    status,
    stage,
    amount,
    tracking_key,
    operation_date,
    concept,
    numeric_reference,
    counterparty,
    initiated_at,
    sent_at,
    settled_at,
    returned_at,
    return_reason,
    transaction_id,
    refund_transaction_id,
    simulated,
    cep,
    clarification,
  };
}

/** A page of `GET /api/v1/wealth/spei/transfers`; null if any transfer is malformed or repeats. */
export function parseSpeiTransfers(value: unknown): SpeiTransfers | null {
  if (!isRecord(value) || !Array.isArray(value.transfers) || value.transfers.length > MAX_PAGE) return null;
  const { next_cursor } = value;
  if (!isNullable(next_cursor, (v): v is string => isText(v, 200))) return null;
  const transfers: TrackedTransfer[] = [];
  const ids = new Set<string>();
  for (const raw of value.transfers) {
    const transfer = parseTrackedTransfer(raw);
    if (!transfer || ids.has(transfer.transfer_id)) return null;
    ids.add(transfer.transfer_id);
    transfers.push(transfer);
  }
  return { transfers, next_cursor };
}

function parseStep(value: unknown): ClarificationStep | null {
  if (!isRecord(value) || !isOneOf(CLARIFICATION_STATUSES, value.status) || !isInstant(value.at)) return null;
  return { status: value.status, at: value.at };
}

/**
 * One clarification; null if malformed or incoherent: exactly the closed
 * ones (resolved or declined) have their closing time, and every closed one
 * has the note that closed it.
 */
export function parseClarification(value: unknown): SpeiClarification | null {
  if (!isRecord(value)) return null;
  const { clarification_id, number, transfer_id, direction, tracking_key, reason, status } = value;
  const { opened_at, updated_at, closed_at } = value;
  const description = optionalText(value.description, MAX_DESCRIPTION);
  const resolution_note = optionalText(value.resolution_note, MAX_DESCRIPTION);
  if (
    !isUuid(clarification_id) ||
    !isWholeNumber(number, 1, MAX_CASE_NUMBER) ||
    !isUuid(transfer_id) ||
    !isOneOf(DIRECTIONS, direction) ||
    !isMatch(TRACKING_KEY_RE)(tracking_key) ||
    !isOneOf(CLARIFICATION_REASONS, reason) ||
    description === undefined ||
    !isOneOf(CLARIFICATION_STATUSES, status) ||
    resolution_note === undefined ||
    !isInstant(opened_at) ||
    !isInstant(updated_at) ||
    !isNullable(closed_at, isInstant) ||
    !Array.isArray(value.history) ||
    value.history.length > MAX_HISTORY
  ) {
    return null;
  }
  const history: ClarificationStep[] = [];
  for (const raw of value.history) {
    const step = parseStep(raw);
    if (!step) return null;
    history.push(step);
  }
  const closed = isClosed(status);
  if (closed !== (closed_at !== null) || (closed && resolution_note === null)) return null;
  return {
    clarification_id,
    number,
    transfer_id,
    direction,
    tracking_key,
    reason,
    description,
    status,
    resolution_note,
    opened_at,
    updated_at,
    closed_at,
    history,
  };
}

/** `GET /api/v1/wealth/spei/disputes`; null if any clarification is malformed or repeats. */
export function parseSpeiClarifications(value: unknown): SpeiClarifications | null {
  if (!isRecord(value) || !Array.isArray(value.clarifications) || value.clarifications.length > MAX_CLARIFICATIONS) {
    return null;
  }
  const clarifications: SpeiClarification[] = [];
  const ids = new Set<string>();
  for (const raw of value.clarifications) {
    const clarification = parseClarification(raw);
    if (!clarification || ids.has(clarification.clarification_id)) return null;
    ids.add(clarification.clarification_id);
    clarifications.push(clarification);
  }
  return { clarifications };
}

// ---------------------------------------------------------------------------
// The page's address
// ---------------------------------------------------------------------------

export type TransfersTab = "transfers" | "clarifications";

/**
 * The Transfers page: on a tab, with a transfer open in the inspector.
 * "/dashboard/transfers?tab=clarifications&transfer=<id>". The id may be the
 * transfer's or a ledger movement's it made: the API finds either.
 */
export function transfersPath(options: { tab?: TransfersTab; transfer?: string | null } = {}): string {
  const params = new URLSearchParams();
  if (options.tab === "clarifications") params.set("tab", "clarifications");
  if (options.transfer) params.set("transfer", options.transfer);
  const query = params.toString();
  return query ? `${TRANSFERS_PATH}?${query}` : TRANSFERS_PATH;
}

/** The tab and the transfer a query string names; anything unknown is ignored. */
export function readTransfersQuery(search: string): { tab: TransfersTab; transfer: string | null } {
  const params = new URLSearchParams(search);
  const transfer = params.get("transfer");
  return {
    tab: params.get("tab") === "clarifications" ? "clarifications" : "transfers",
    transfer: isUuid(transfer) ? transfer.toLowerCase() : null,
  };
}

/** Whether `id` names this transfer: its own id, or the ledger movement it made or its refund. */
export function transferMatches(transfer: TrackedTransfer, id: string): boolean {
  const key = id.toLowerCase();
  return transfer.transfer_id === key || transfer.transaction_id === key || transfer.refund_transaction_id === key;
}

/**
 * Whether a movement of the IFPE ledger is a SPEI transfer the tracker knows:
 * a deposit credited by SPEI (`spei:`), the refund of an order that came back
 * (`spei-reverso:`) or an order's charge (every withdrawal but a correction).
 */
export function isSpeiMovement(movement: { tipo: string; clave_idempotencia: string }): boolean {
  const key = movement.clave_idempotencia;
  if (movement.tipo === "DEPOSITO") return key.startsWith("spei:") || key.startsWith("spei-reverso:");
  return movement.tipo === "RETIRO" && !key.startsWith("spei-correccion:");
}

// ---------------------------------------------------------------------------
// Words for the screens
// ---------------------------------------------------------------------------

export const KIND_LABELS: Readonly<Record<TransferKind, string>> = {
  deposit: "Deposit",
  withdrawal: "Withdrawal",
  payment: "Payment",
};

/** "Processing", "Settled", "Returned"; an order that never went out is "Not completed". */
export function transferStatusLabel(transfer: Pick<TrackedTransfer, "status" | "stage">): string {
  if (transfer.stage === "rejected") return "Not completed";
  switch (transfer.status) {
    case "pending":
      return "Processing";
    case "settled":
      return "Settled";
    case "returned":
      return "Returned";
  }
}

/** The bank's name; "Bank 137" when the catalog does not have it; null when not even its code is known. */
export function bankLabel(bank: TransferBank): string | null {
  return bank.name ?? (bank.code !== null ? `Bank ${bank.code}` : null);
}

/** An account as the screens show it: "•••• 1234". */
export function maskedAccount(last4: string): string {
  return `•••• ${last4}`;
}

/** The bank and the account's last digits: "BBVA México •••• 1234" (either may be missing). */
export function accountLabel(bank: TransferBank, last4: string | null): string | null {
  const parts = [bankLabel(bank), last4 !== null ? maskedAccount(last4) : null].filter((p) => p !== null);
  return parts.length > 0 ? parts.join(" ") : null;
}

/**
 * Who is on the other side, in a few words: a deposit's sender ("María López",
 * else the bank), a withdrawal's own account ("BBVA México •••• 1234"), a
 * payment's beneficiary.
 */
export function counterpartyLabel(transfer: Pick<TrackedTransfer, "kind" | "counterparty" | "simulated">): string {
  const { name, bank, account_last4 } = transfer.counterparty;
  switch (transfer.kind) {
    case "deposit":
      return name ?? bankLabel(bank) ?? (transfer.simulated ? "Demo deposit" : "Another bank");
    case "withdrawal":
      return accountLabel(bank, account_last4) ?? "Your linked account";
    case "payment":
      return name ?? accountLabel(bank, account_last4) ?? "Another account";
  }
}

/** "Deposit from María López", "Withdrawal to BBVA México •••• 1234", "Payment to Juan Pérez". */
export function transferTitle(transfer: Pick<TrackedTransfer, "kind" | "counterparty" | "simulated">): string {
  const who = counterpartyLabel(transfer);
  if (transfer.kind === "deposit") return who === "Demo deposit" ? who : `Deposit from ${who}`;
  return `${KIND_LABELS[transfer.kind]} to ${who}`;
}

/** Why the receiving bank sent an outgoing transfer back, in plain words. */
export function returnReasonText(reason: ReturnReason | null): string {
  switch (reason) {
    case "account_not_found":
      return "The receiving bank says the account does not exist.";
    case "account_blocked":
      return "The receiving bank says the account cannot receive transfers right now.";
    case "account_closed":
      return "The receiving bank says the account is closed.";
    case "missing_information":
      return "The receiving bank says some required details were missing.";
    default:
      return "The receiving bank sent it back.";
  }
}

const OPERATION_DATE = new Intl.DateTimeFormat("en-US", {
  timeZone: "UTC",
  month: "short",
  day: "numeric",
  year: "numeric",
});

/** A calendar date ("2026-11-24") as "Nov 24, 2026". */
export function formatOperationDate(isoDate: string): string {
  return OPERATION_DATE.format(Date.parse(`${isoDate}T00:00:00Z`));
}

// ---------------------------------------------------------------------------
// The settlement timeline
// ---------------------------------------------------------------------------

/**
 * - done: it happened.
 * - current: what the transfer waits for now.
 * - upcoming: later.
 * - skipped: it never happened, and never will (an order that failed before going out).
 * - returned: the transfer's end when it came back.
 */
export type StepState = "done" | "current" | "upcoming" | "skipped" | "returned";

export interface TimelineStep {
  key: "initiated" | "sent" | "settled" | "returned" | "not_completed";
  title: string;
  detail: string;
  /** When, to the minute (ISO 8601); null when not known or not yet. */
  at: string | null;
  /** A calendar day when only that is known (a deposit's operation date). */
  date: string | null;
  state: StepState;
}

const REFUNDED = "The full amount went back to your available cash.";

/**
 * Initiated → Sent to Banxico via STP → Settled, and Returned (or Not
 * completed) when it came back. For a deposit, the other bank initiated it
 * and it reached the client's CLABE through Banxico and STP.
 */
export function transferTimeline(transfer: TrackedTransfer): TimelineStep[] {
  const { stage } = transfer;
  if (transfer.direction === "incoming") {
    const sender = bankLabel(transfer.counterparty.bank);
    const steps: TimelineStep[] = [
      {
        key: "initiated",
        title: "Initiated",
        detail: transfer.simulated
          ? "A simulated deposit: no bank sent it."
          : `${sender ?? "The sending bank"} sent it by SPEI.`,
        at: null,
        date: transfer.operation_date,
        state: "done",
      },
      {
        key: "sent",
        title: "Received from Banxico via STP",
        detail: "It reached your CLABE through Banxico's SPEI network.",
        at: transfer.initiated_at,
        date: null,
        state: "done",
      },
    ];
    steps.push(
      stage === "settled"
        ? {
            key: "settled",
            title: "Settled",
            detail: "Credited to your available cash.",
            at: transfer.settled_at,
            date: null,
            state: "done",
          }
        : {
            key: "returned",
            title: "Returned to the sender",
            detail: "Your account could not receive it, so it went back to the sending bank. Nothing was credited.",
            at: transfer.returned_at,
            date: null,
            state: "returned",
          },
    );
    return steps;
  }

  const receiver = bankLabel(transfer.counterparty.bank) ?? "The receiving bank";
  const steps: TimelineStep[] = [
    {
      key: "initiated",
      title: "Initiated",
      detail:
        transfer.kind === "withdrawal"
          ? "You asked for the withdrawal, and the amount left your available cash."
          : "You sent the payment, and the amount left your available cash.",
      at: transfer.initiated_at,
      date: null,
      state: "done",
    },
  ];
  if (stage === "initiated") {
    steps.push({
      key: "sent",
      title: "Sent to Banxico via STP",
      detail: "Waiting for our settlement provider, STP, to send it through Banxico's SPEI network.",
      at: null,
      date: null,
      state: "current",
    });
  } else if (stage === "rejected" && transfer.sent_at === null) {
    steps.push({
      key: "sent",
      title: "Sent to Banxico via STP",
      detail: "It was not sent.",
      at: null,
      date: null,
      state: "skipped",
    });
  } else {
    steps.push({
      key: "sent",
      title: "Sent to Banxico via STP",
      detail: "Our settlement provider, STP, sent it through Banxico's SPEI network.",
      at: transfer.sent_at,
      date: null,
      state: "done",
    });
  }
  if (stage === "initiated" || stage === "sent") {
    steps.push({
      key: "settled",
      title: "Settled",
      detail: `${receiver} credits it once SPEI settles it, usually within seconds.`,
      at: null,
      date: null,
      state: stage === "sent" ? "current" : "upcoming",
    });
  } else if (stage === "rejected") {
    steps.push({
      key: "not_completed",
      title: "Not completed",
      detail: `It could not be completed and never settled. ${REFUNDED}`,
      at: transfer.returned_at,
      date: null,
      state: "returned",
    });
  } else {
    steps.push({
      key: "settled",
      title: "Settled",
      detail: `${receiver} received it.`,
      at: transfer.settled_at,
      date: null,
      state: "done",
    });
    if (stage === "returned") {
      steps.push({
        key: "returned",
        title: "Returned",
        detail: `${returnReasonText(transfer.return_reason)} ${REFUNDED}`,
        at: transfer.returned_at,
        date: null,
        state: "returned",
      });
    }
  }
  return steps;
}

// ---------------------------------------------------------------------------
// The CEP
// ---------------------------------------------------------------------------

/** Why a transfer has no CEP to look up, or what to know about it; null when it can be looked up. */
export function cepNotice(transfer: TrackedTransfer): string | null {
  const { cep } = transfer;
  if (cep === null) {
    if (transfer.status === "pending") return "Banxico issues the CEP once the transfer settles.";
    return transfer.direction === "incoming"
      ? "It was never credited to your account, so there is no CEP for it."
      : "It never settled, so Banxico has no CEP for it.";
  }
  if (transfer.simulated)
    return "A simulated transfer: Banxico has no CEP for it. These are the details it would ask for.";
  if (!cep.available) return "Banxico has not assigned its operation date yet. Try again in a few minutes.";
  return null;
}

/** An amount as Banxico's CEP form takes it: to the cent, no separators ("2500.00"). */
export function cepAmount(amount: string): string {
  return formatAmount(amount).replaceAll(",", "");
}

/** The CEP's details as text, one per line, to paste into Banxico's form or a message. */
export function cepDetailsText(cep: CepData): string {
  // "BBVA México (012)"; "Bank 137" when the catalog does not have it.
  const bank = (b: TransferBank) => (b.name !== null ? `${b.name} (${b.code})` : (bankLabel(b) ?? "—"));
  const account =
    cep.beneficiary_account ??
    (cep.beneficiary_account_last4 !== null ? maskedAccount(cep.beneficiary_account_last4) : "—");
  return [
    `Operation date: ${cep.operation_date ?? "—"}`,
    `Tracking key: ${cep.tracking_key}`,
    `Sender: ${bank(cep.sender_bank)}`,
    `Receiver: ${bank(cep.receiver_bank)}`,
    `Beneficiary account: ${account}`,
    `Amount: ${cepAmount(cep.amount)} MXN`,
  ].join("\n");
}

// ---------------------------------------------------------------------------
// Clarifications
// ---------------------------------------------------------------------------

export function isClosed(status: ClarificationStatus): boolean {
  return status === "resolved" || status === "declined";
}

export const CLARIFICATION_STATUS_LABELS: Readonly<Record<ClarificationStatus, string>> = {
  open: "Open",
  in_review: "In review",
  resolved: "Resolved",
  declined: "Declined",
};

/** Where it is, in a sentence. */
export function clarificationStatusText(status: ClarificationStatus): string {
  switch (status) {
    case "open":
      return "We received it. Our team will review it and reply here.";
    case "in_review":
      return "Our team is reviewing it. You will see the outcome here.";
    case "resolved":
      return "Resolved. The note from our team explains the outcome.";
    case "declined":
      return "Closed without a change. The note from our team explains why.";
  }
}

/** The history's steps: "Opened", "In review", "Resolved", "Declined". */
export const STEP_LABELS: Readonly<Record<ClarificationStatus, string>> = {
  open: "Opened",
  in_review: "In review",
  resolved: "Resolved",
  declined: "Declined",
};

export const REASON_LABELS: Readonly<Record<ClarificationReason, string>> = {
  delayed: "It's taking too long",
  not_received: "It settled, but it didn't arrive",
  returned: "It came back",
  wrong_amount: "The amount is wrong",
  not_recognized: "I don't recognize it",
  other: "Something else",
};

/** The reasons that fit where the transfer is; "Something else" always. */
export function reasonsFor(transfer: Pick<TrackedTransfer, "direction" | "status">): ClarificationReason[] {
  if (transfer.status === "returned") return ["returned", "other"];
  if (transfer.direction === "outgoing") {
    return transfer.status === "pending" ? ["delayed", "other"] : ["not_received", "wrong_amount", "other"];
  }
  return ["wrong_amount", "not_recognized", "other"];
}

export type ClarificationBlock = { kind: "active"; number: number } | { kind: "expired" } | { kind: "limit" };

/**
 * Why a clarification cannot be opened on this transfer now; null when it
 * can. The backend enforces the same rules (one active per transfer,
 * `CLARIFICATION_WINDOW_DAYS` after it, `MAX_ACTIVE_CLARIFICATIONS` open).
 */
export function clarificationBlock(
  transfer: Pick<TrackedTransfer, "initiated_at" | "clarification">,
  activeCount: number,
  now = Date.now(),
): ClarificationBlock | null {
  const latest = transfer.clarification;
  if (latest !== null && !isClosed(latest.status)) return { kind: "active", number: latest.number };
  if (Date.parse(transfer.initiated_at) < now - CLARIFICATION_WINDOW_DAYS * DAY_MS) return { kind: "expired" };
  if (activeCount >= MAX_ACTIVE_CLARIFICATIONS) return { kind: "limit" };
  return null;
}

export function clarificationBlockText(block: ClarificationBlock): string {
  switch (block.kind) {
    case "active":
      return `Clarification #${block.number} is already open for this transfer. Follow it in Clarifications.`;
    case "expired":
      return `Clarifications can be opened within ${CLARIFICATION_WINDOW_DAYS} days of the transfer.`;
    case "limit":
      return `You have ${MAX_ACTIVE_CLARIFICATIONS} open clarifications. Once one is closed you can open another.`;
  }
}

/**
 * The description as it is sent: line breaks kept (\n), tabs as spaces,
 * every other control character dropped, trimmed. The backend refuses
 * control characters other than \n.
 */
export function cleanDescription(raw: string): string {
  return raw
    .replace(/\r\n?/g, "\n")
    .replace(/\t/g, " ")
    .replace(/[\u0000-\u0009\u000B-\u001F\u007F-\u009F]/g, "")
    .trim();
}

/** Characters as the backend counts them: Unicode scalar values, not UTF-16 units. */
export function characterCount(text: string): number {
  return Array.from(text).length;
}

/** What stops the form from being sent; null when it can be. */
export function clarificationProblem(reason: ClarificationReason | null, rawDescription: string): string | null {
  if (reason === null) return "Choose what happened.";
  const length = characterCount(cleanDescription(rawDescription));
  if (length > MAX_DESCRIPTION) return `Keep the description to ${MAX_DESCRIPTION} characters.`;
  if (reason === "other" && length < MIN_OTHER_DESCRIPTION) {
    return `Tell us what happened, in at least ${MIN_OTHER_DESCRIPTION} characters.`;
  }
  return null;
}

/** One draft's terms: the same terms retried keep their idempotency key, other terms get a new one. */
export function clarificationSignature(
  transferId: string,
  reason: ClarificationReason,
  rawDescription: string,
): string {
  return JSON.stringify([transferId, reason, cleanDescription(rawDescription)]);
}

/** The body of `POST /api/v1/wealth/spei/disputes`: no description when there is none. */
export function clarificationRequest(
  transfer: Pick<TrackedTransfer, "transfer_id" | "direction">,
  reason: ClarificationReason,
  rawDescription: string,
  idempotencyKey: string,
): SpeiClarificationRequest {
  const description = cleanDescription(rawDescription);
  return {
    transfer_id: transfer.transfer_id,
    direction: transfer.direction,
    reason,
    ...(description !== "" ? { description } : {}),
    idempotency_key: idempotencyKey,
  };
}

/** The clarification's summary as its transfer carries it. */
export function summaryOf(clarification: SpeiClarification): ClarificationSummary {
  return {
    clarification_id: clarification.clarification_id,
    number: clarification.number,
    status: clarification.status,
  };
}

/** How many are open or in review. */
export function activeClarifications(clarifications: readonly SpeiClarification[]): number {
  return clarifications.filter((c) => !isClosed(c.status)).length;
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/**
 * The first page again (the vault moved): its transfers as they are now, on
 * top, then the older pages already shown. A transfer changes (settles,
 * comes back), so the fresh copy replaces the one shown.
 */
export function mergeFirstPage(shown: readonly TrackedTransfer[], page: readonly TrackedTransfer[]): TrackedTransfer[] {
  const fresh = new Set(page.map((t) => t.transfer_id));
  return [...page, ...shown.filter((t) => !fresh.has(t.transfer_id))];
}

/** The next page after the ones shown, each transfer once. */
export function appendTransfers(
  shown: readonly TrackedTransfer[],
  page: readonly TrackedTransfer[],
): TrackedTransfer[] {
  const seen = new Set(shown.map((t) => t.transfer_id));
  return [...shown, ...page.filter((t) => !seen.has(t.transfer_id))];
}

/** A clarification just opened (or answered again): in its place if listed, else on top. */
export function upsertClarification(
  list: readonly SpeiClarification[],
  clarification: SpeiClarification,
): SpeiClarification[] {
  const at = list.findIndex((c) => c.clarification_id === clarification.clarification_id);
  if (at === -1) return [clarification, ...list];
  return list.map((c, i) => (i === at ? clarification : c));
}

/** The transfer with its newest clarification, as the list will show it after the next reload. */
export function withClarification(transfer: TrackedTransfer, clarification: SpeiClarification): TrackedTransfer {
  if (transfer.transfer_id !== clarification.transfer_id || transfer.direction !== clarification.direction) {
    return transfer;
  }
  const latest = transfer.clarification;
  // The newest by case number: an older one answered again does not replace it.
  if (latest !== null && latest.number > clarification.number) return transfer;
  return { ...transfer, clarification: summaryOf(clarification) };
}
