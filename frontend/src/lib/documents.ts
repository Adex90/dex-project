/**
 * The Document Inbox: validating trade confirmations and account statements
 * from `/api/v1/wealth/documentos`, and the words and files the screens make
 * of them.
 *
 * - Documents are derived by the server from the hash-chained ledgers, so a
 *   confirmation and a closed month's statement come back identical every
 *   time; the reference hash proves it.
 * - Network data is validated before anything renders it: a malformed
 *   payload is rejected whole.
 * - No wording here may tip off a client under review: funds on hold appear
 *   only in the statement's closing breakdown, with the neutral copy.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { HOLD_COPY } from "./aml.ts";
import { isDecimalString, isNonNegative, isOneOf, isRecord, isText } from "./vault.ts";
import type {
  AccountStatement,
  DocumentKind,
  DocumentList,
  DocumentSummary,
  FinancialDocument,
  Holder,
  ReserveSeal,
  StatementEntry,
  StatementEntryKind,
  TradeConfirmation,
} from "../types/documents.ts";

/** The Document Inbox page. */
export const DOCUMENTS_PATH = "/dashboard/documents";

const KINDS: readonly DocumentKind[] = ["trade_confirmation", "account_statement"];
const ENTRY_KINDS: readonly StatementEntryKind[] = [
  "deposit",
  "withdrawal",
  "transfer_in",
  "transfer_out",
  "to_fixed_term",
  "from_fixed_term",
  "market_buy",
  "market_sell",
];
const MAX_DOCUMENTS = 1_000;
const MAX_ENTRIES = 10_000;
const HASH = /^[0-9a-f]{64}$/;
const ID = /^(co_[0-9a-f-]{36}|ec_\d{4}-\d{2})$/;

const isHash = (v: unknown): v is string => typeof v === "string" && HASH.test(v);
const isTimestamp = (v: unknown): v is string => isText(v, 64) && !Number.isNaN(Date.parse(v));
const isSequence = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const isTicker = (v: unknown): v is string => typeof v === "string" && /^[A-Z][A-Z0-9]{0,11}$/.test(v);
const orNull = <T>(v: unknown, check: (x: unknown) => x is T): T | null | undefined =>
  v === null || v === undefined ? null : check(v) ? v : undefined;

/** Whether an id names a document the API serves ("co_<uuid>", "ec_<YYYY-MM>"). */
export function isDocumentId(id: string): boolean {
  return ID.test(id);
}

function parseSummary(value: unknown): DocumentSummary | null {
  if (!isRecord(value)) return null;
  const { id, kind, folio, title, date, reference, amount, provisional } = value;
  if (
    typeof id !== "string" ||
    !isDocumentId(id) ||
    !isOneOf(KINDS, kind) ||
    (kind === "trade_confirmation") !== id.startsWith("co_") ||
    !isText(folio, 40) ||
    !isText(title, 120) ||
    !isTimestamp(date) ||
    !isHash(reference) ||
    !isDecimalString(amount) ||
    typeof provisional !== "boolean"
  ) {
    return null;
  }
  return { id, kind, folio, title, date, reference, amount, provisional };
}

/** `GET /api/v1/wealth/documentos`; null when malformed. */
export function parseDocumentList(value: unknown): DocumentList | null {
  if (!isRecord(value) || !Array.isArray(value.documents) || value.documents.length > MAX_DOCUMENTS) return null;
  if (typeof value.truncated !== "boolean") return null;
  const documents: DocumentSummary[] = [];
  for (const raw of value.documents) {
    const d = parseSummary(raw);
    if (!d) return null;
    documents.push(d);
  }
  return { documents, truncated: value.truncated };
}

function parseSeal(value: unknown): ReserveSeal | null {
  if (!isRecord(value)) return null;
  const s = value;
  const snapshot = orNull(s.snapshot_id, (x): x is string => isText(x, 64));
  const root = orNull(s.root, isHash);
  const taken = orNull(s.taken_at, isTimestamp);
  const ratio = orNull(s.reserve_ratio, isNonNegative);
  const ledger = orNull(s.ledger_anchor, isSequence);
  const market = orNull(s.market_anchor, isSequence);
  if (
    !isOneOf(["sealed", "pending"] as const, s.status) ||
    snapshot === undefined ||
    root === undefined ||
    taken === undefined ||
    ratio === undefined ||
    ledger === undefined ||
    market === undefined ||
    !isSequence(s.covers_ledger_sequence) ||
    !isSequence(s.covers_market_sequence)
  ) {
    return null;
  }
  // A seal names its snapshot (a cut taken before any trade has no market
  // anchor); a pending one names nothing.
  const named = [snapshot, root, taken, ratio, ledger];
  if (s.status === "sealed" ? named.includes(null) : [...named, market].some((x) => x !== null)) return null;
  return {
    status: s.status,
    snapshot_id: snapshot,
    root,
    taken_at: taken,
    reserve_ratio: ratio,
    ledger_anchor: ledger,
    market_anchor: market,
    covers_ledger_sequence: s.covers_ledger_sequence,
    covers_market_sequence: s.covers_market_sequence,
  };
}

function parseHolder(value: unknown): Holder | null {
  if (!isRecord(value) || !isText(value.name, 200)) return null;
  const rfc = orNull(value.rfc, (x): x is string => isText(x, 13));
  return rfc === undefined ? null : { name: value.name, rfc };
}

function parseConfirmation(v: Record<string, unknown>): TradeConfirmation | null {
  const holder = parseHolder(v.holder);
  const seal = parseSeal(v.reserve_seal);
  const limit = orNull(v.limit_price, isNonNegative);
  const stop = orNull(v.stop_price, isNonNegative);
  const cost = orNull(v.cost_basis, isNonNegative);
  const pnl = orNull(v.realized_pnl, isDecimalString);
  const settlementSeq = orNull(v.settlement_sequence, isSequence);
  const settlementHash = orNull(v.settlement_hash, isHash);
  const last4 = orNull(v.account_clabe_last4, (x): x is string => typeof x === "string" && /^\d{4}$/.test(x));
  if (
    !holder ||
    !seal ||
    limit === undefined ||
    stop === undefined ||
    cost === undefined ||
    pnl === undefined ||
    settlementSeq === undefined ||
    settlementHash === undefined ||
    last4 === undefined ||
    typeof v.id !== "string" ||
    !v.id.startsWith("co_") ||
    !isText(v.folio, 40) ||
    !isTimestamp(v.executed_at) ||
    !isOneOf(["buy", "sell"] as const, v.side) ||
    !isTicker(v.ticker) ||
    !isText(v.asset_name, 120) ||
    !isOneOf(["market", "limit", "stop"] as const, v.order_type) ||
    !isNonNegative(v.requested_amount) ||
    !isNonNegative(v.quantity) ||
    !isNonNegative(v.price) ||
    !isNonNegative(v.gross_amount) ||
    !isNonNegative(v.commission) ||
    !isNonNegative(v.net_amount) ||
    !isOneOf(["simulated", "oracle"] as const, v.price_source) ||
    !isSequence(v.market_sequence) ||
    !isHash(v.reference) ||
    !isHash(v.previous_hash)
  ) {
    return null;
  }
  return {
    id: v.id,
    kind: "trade_confirmation",
    folio: v.folio,
    holder,
    executed_at: v.executed_at,
    side: v.side,
    ticker: v.ticker,
    asset_name: v.asset_name,
    order_type: v.order_type,
    limit_price: limit,
    stop_price: stop,
    requested_amount: v.requested_amount,
    quantity: v.quantity,
    price: v.price,
    gross_amount: v.gross_amount,
    commission: v.commission,
    net_amount: v.net_amount,
    cost_basis: cost,
    realized_pnl: pnl,
    price_source: v.price_source,
    market_sequence: v.market_sequence,
    reference: v.reference,
    previous_hash: v.previous_hash,
    settlement_sequence: settlementSeq,
    settlement_hash: settlementHash,
    account_clabe_last4: last4,
    reserve_seal: seal,
  };
}

function parseEntry(value: unknown): StatementEntry | null {
  if (!isRecord(value)) return null;
  const ticker = orNull(value.ticker, isTicker);
  const tracking = orNull(value.tracking_key, (x): x is string => isText(x, 40));
  if (
    ticker === undefined ||
    tracking === undefined ||
    !isTimestamp(value.date) ||
    !isOneOf(ENTRY_KINDS, value.kind) ||
    !isSequence(value.sequence) ||
    !isHash(value.hash) ||
    !isDecimalString(value.amount) ||
    !isDecimalString(value.balance)
  ) {
    return null;
  }
  return {
    date: value.date,
    kind: value.kind,
    ticker,
    tracking_key: tracking,
    sequence: value.sequence,
    hash: value.hash,
    amount: value.amount,
    balance: value.balance,
  };
}

function parseStatement(v: Record<string, unknown>): AccountStatement | null {
  const holder = parseHolder(v.holder);
  const seal = parseSeal(v.reserve_seal);
  const b = v.closing_breakdown;
  const t = v.trades;
  if (
    !holder ||
    !seal ||
    !isRecord(b) ||
    !isRecord(t) ||
    !Array.isArray(v.entries) ||
    v.entries.length > MAX_ENTRIES ||
    !Array.isArray(v.positions) ||
    typeof v.id !== "string" ||
    !v.id.startsWith("ec_") ||
    !isText(v.folio, 40) ||
    typeof v.period !== "string" ||
    !/^\d{4}-\d{2}$/.test(v.period) ||
    v.id !== `ec_${v.period}` ||
    !isTimestamp(v.period_start) ||
    !isTimestamp(v.period_end) ||
    typeof v.provisional !== "boolean" ||
    typeof v.account_clabe !== "string" ||
    !/^\d{18}$/.test(v.account_clabe) ||
    !isDecimalString(v.opening_balance) ||
    !isNonNegative(v.credits) ||
    !isNonNegative(v.debits) ||
    !isDecimalString(v.closing_balance) ||
    !isDecimalString(b.available) ||
    !isNonNegative(b.on_hold) ||
    !isNonNegative(b.held_for_orders) ||
    typeof t.buys !== "number" ||
    typeof t.sells !== "number" ||
    !isNonNegative(t.bought) ||
    !isNonNegative(t.sold) ||
    !isNonNegative(t.commission) ||
    !isHash(v.reference)
  ) {
    return null;
  }
  const entries: StatementEntry[] = [];
  for (const raw of v.entries) {
    const e = parseEntry(raw);
    if (!e) return null;
    entries.push(e);
  }
  const positions: AccountStatement["positions"] = [];
  for (const raw of v.positions) {
    if (!isRecord(raw) || !isTicker(raw.ticker) || !isText(raw.asset_name, 120) || !isNonNegative(raw.quantity)) {
      return null;
    }
    positions.push({ ticker: raw.ticker, asset_name: raw.asset_name, quantity: raw.quantity });
  }
  return {
    id: v.id,
    kind: "account_statement",
    folio: v.folio,
    period: v.period,
    period_start: v.period_start,
    period_end: v.period_end,
    provisional: v.provisional,
    holder,
    account_clabe: v.account_clabe,
    opening_balance: v.opening_balance,
    credits: v.credits,
    debits: v.debits,
    closing_balance: v.closing_balance,
    closing_breakdown: { available: b.available, on_hold: b.on_hold, held_for_orders: b.held_for_orders },
    entries,
    trades: { buys: t.buys, bought: t.bought, sells: t.sells, sold: t.sold, commission: t.commission },
    positions,
    reference: v.reference,
    reserve_seal: seal,
  };
}

/** `GET /api/v1/wealth/documentos/{id}`; null when malformed. */
export function parseDocument(value: unknown): FinancialDocument | null {
  if (!isRecord(value)) return null;
  if (value.kind === "trade_confirmation") return parseConfirmation(value);
  if (value.kind === "account_statement") return parseStatement(value);
  return null;
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

export const ENTRY_LABELS: Record<StatementEntryKind, string> = {
  deposit: "Deposit",
  withdrawal: "Withdrawal to your bank",
  transfer_in: "Transfer received",
  transfer_out: "Transfer sent",
  to_fixed_term: "To fixed-term contract",
  from_fixed_term: "From fixed-term contract",
  market_buy: "Market buy",
  market_sell: "Market sell",
};

export const ORDER_TYPE_LABELS: Record<TradeConfirmation["order_type"], string> = {
  market: "Market order",
  limit: "Limit order",
  stop: "Stop order",
};

/** The statement's hold line, in the neutral copy every client screen uses. */
export const STATEMENT_HOLD_LABEL = `Funds ${HOLD_COPY}`;

/** "November 2026". */
export function periodLabel(period: string): string {
  const [year, month] = period.split("-").map(Number);
  return new Intl.DateTimeFormat("en-US", { month: "long", year: "numeric", timeZone: "UTC" }).format(
    new Date(Date.UTC(year ?? 1970, (month ?? 1) - 1, 1)),
  );
}

export type InboxFilter = "all" | DocumentKind;

export function filterDocuments(documents: readonly DocumentSummary[], filter: InboxFilter): DocumentSummary[] {
  return filter === "all" ? [...documents] : documents.filter((d) => d.kind === filter);
}

/** The file name of a downloaded document: "synthetic-capital-CO-00000042.json". */
export function downloadName(document: FinancialDocument): string {
  return `synthetic-capital-${document.folio}.json`;
}

/** The downloaded file: the document exactly as the API returned it, pretty-printed. */
export function downloadContent(document: FinancialDocument): string {
  return `${JSON.stringify(document, null, 2)}\n`;
}

export const SIDE_LABELS: Record<TradeConfirmation["side"], string> = { buy: "Buy", sell: "Sell" };

export const PRICE_SOURCE_LABELS: Record<TradeConfirmation["price_source"], string> = {
  simulated: "Simulated price feed",
  oracle: "Price oracle",
};

const MEXICO_CITY_TIME = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/Mexico_City",
  year: "numeric",
  month: "short",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});
const MEXICO_CITY_DATE = new Intl.DateTimeFormat("en-US", {
  timeZone: "America/Mexico_City",
  year: "numeric",
  month: "short",
  day: "2-digit",
});

/**
 * A document's time, in Mexico City as the ledgers' days and months are:
 * "Nov 23, 2026, 06:00:00 Mexico City".
 */
export function documentTime(iso: string): string {
  return `${MEXICO_CITY_TIME.format(new Date(iso))} Mexico City`;
}

/** A document's day in Mexico City: "Nov 23, 2026". */
export function documentDate(iso: string): string {
  return MEXICO_CITY_DATE.format(new Date(iso));
}
