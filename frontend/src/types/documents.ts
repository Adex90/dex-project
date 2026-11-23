/**
 * The Document Inbox (roadmap item 24): trade confirmations and account
 * statements from `GET /api/v1/wealth/documentos` (English names, like the
 * rest of `/wealth`). `lib/documents.ts` validates them.
 */

import type { DecimalString } from "./vault.ts";

export type DocumentKind = "trade_confirmation" | "account_statement";

/** A row of the inbox. */
export interface DocumentSummary {
  /** "co_<order id>" or "ec_<YYYY-MM>". */
  id: string;
  kind: DocumentKind;
  /** "CO-00000042", "EC-2026-11-0017". */
  folio: string;
  title: string;
  /** Execution time, or the end of the period (now, while it runs). */
  date: string;
  /** The order's hash in the market ledger, or the statement's content hash. */
  reference: string;
  /** The order's amount, or the statement's closing balance. */
  amount: DecimalString;
  /** A statement of the month in progress: its figures change until it closes. */
  provisional: boolean;
}

export interface DocumentList {
  /** Newest first. */
  documents: DocumentSummary[];
  /** Older trade confirmations exist beyond the list. */
  truncated: boolean;
}

/** The first published Proof of Reserve snapshot that covers a document. */
export interface ReserveSeal {
  status: "sealed" | "pending";
  snapshot_id: string | null;
  root: string | null;
  taken_at: string | null;
  /** "100.00". */
  reserve_ratio: string | null;
  ledger_anchor: number | null;
  market_anchor: number | null;
  covers_ledger_sequence: number;
  covers_market_sequence: number;
}

export interface Holder {
  name: string;
  rfc: string | null;
}

export interface TradeConfirmation {
  id: string;
  kind: "trade_confirmation";
  folio: string;
  holder: Holder;
  executed_at: string;
  side: "buy" | "sell";
  ticker: string;
  asset_name: string;
  order_type: "market" | "limit" | "stop";
  limit_price: DecimalString | null;
  stop_price: DecimalString | null;
  requested_amount: DecimalString;
  quantity: DecimalString;
  price: DecimalString;
  gross_amount: DecimalString;
  commission: DecimalString;
  net_amount: DecimalString;
  cost_basis: DecimalString | null;
  realized_pnl: DecimalString | null;
  price_source: "simulated" | "oracle";
  market_sequence: number;
  reference: string;
  previous_hash: string;
  settlement_sequence: number | null;
  settlement_hash: string | null;
  account_clabe_last4: string | null;
  reserve_seal: ReserveSeal;
}

export type StatementEntryKind =
  | "deposit"
  | "withdrawal"
  | "transfer_in"
  | "transfer_out"
  | "to_fixed_term"
  | "from_fixed_term"
  | "market_buy"
  | "market_sell";

export interface StatementEntry {
  date: string;
  kind: StatementEntryKind;
  ticker: string | null;
  tracking_key: string | null;
  sequence: number;
  hash: string;
  /** Signed: positive credits, negative debits. */
  amount: DecimalString;
  /** Total cash after the movement. */
  balance: DecimalString;
}

export interface AccountStatement {
  id: string;
  kind: "account_statement";
  folio: string;
  /** "2026-11". */
  period: string;
  period_start: string;
  period_end: string;
  provisional: boolean;
  holder: Holder;
  account_clabe: string;
  opening_balance: DecimalString;
  credits: DecimalString;
  debits: DecimalString;
  closing_balance: DecimalString;
  closing_breakdown: { available: DecimalString; on_hold: DecimalString; held_for_orders: DecimalString };
  entries: StatementEntry[];
  trades: { buys: number; bought: DecimalString; sells: number; sold: DecimalString; commission: DecimalString };
  positions: { ticker: string; asset_name: string; quantity: DecimalString }[];
  reference: string;
  reserve_seal: ReserveSeal;
}

export type FinancialDocument = TradeConfirmation | AccountStatement;
