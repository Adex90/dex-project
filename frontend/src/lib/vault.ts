/**
 * Sovereign Vault helpers: display formatting, and the validation of what
 * arrives from the network (REST snapshots and WebSocket messages).
 *
 * Pure functions with no React and no DOM: the store and the components use
 * them, and `node --test` runs them directly (hence the relative `.ts`
 * imports, which Node resolves without the bundler's `@/` alias).
 */

import { formatAmount, toUnits } from "./decimal.ts";
import {
  RESERVE_PROOF_STATUSES,
  SPEI_DIRECTIONS,
  SPEI_TRANSFER_STATUSES,
  type DecimalString,
  type Holding,
  type ReserveProof,
  type SpeiTransfer,
  type VaultBalance,
  type VaultRealtimeMessage,
  type VaultSnapshot,
} from "../types/vault.ts";

/* ------------------------------------------------------------------------ */
/* Formatting                                                               */
/* ------------------------------------------------------------------------ */

export type Trend = "up" | "down" | "flat";

export interface SignedPercent {
  /** "+4.26%", "−1.40%" (with a true minus sign, U+2212) or "0.00%". */
  text: string;
  /** Direction of the value as displayed: anything that rounds to 0.00 is flat. */
  trend: Trend;
}

/**
 * A percentage with an explicit sign, rounded half-even to `decimals`
 * places from the exact value. The sign and the trend follow the rounded
 * figure, so the color never disagrees with the number next to it.
 */
export function signedPercent(value: DecimalString, decimals = 2): SignedPercent {
  const shown = formatAmount(value, decimals);
  if (shown.startsWith("-")) return { text: `−${shown.slice(1)}%`, trend: "down" };
  if (/[1-9]/.test(shown)) return { text: `+${shown}%`, trend: "up" };
  return { text: `${shown}%`, trend: "flat" };
}

export interface MoneyParts {
  negative: boolean;
  /** Integer part with thousands separators: "128,450". */
  integer: string;
  /** Decimal digits without the point: "75". Empty when `decimals` is 0. */
  fraction: string;
}

/** Splits an amount so the UI can style its parts: "128450.756" gives "128,450" and "76". */
export function moneyParts(value: DecimalString, decimals = 2): MoneyParts {
  const shown = formatAmount(value, decimals);
  const negative = shown.startsWith("-");
  const [integer = "0", fraction = ""] = (negative ? shown.slice(1) : shown).split(".");
  return { negative, integer, fraction };
}

/**
 * A quantity of shares or units: at most `maxDecimals` places (half-even),
 * without trailing zeros. "12.50000000" gives "12.5"; "7000" gives "7,000".
 */
export function formatShares(value: DecimalString, maxDecimals = 4): string {
  const shown = formatAmount(value, maxDecimals);
  return shown.includes(".") ? shown.replace(/\.?0+$/, "") : shown;
}

/** "0x3f9a2b…c21b": enough to recognize a hash at a glance. Show the full value on demand. */
export function shortHash(hash: string, head = 6, tail = 4): string {
  const prefix = hash.startsWith("0x") ? "0x" : "";
  const body = hash.slice(prefix.length);
  return body.length <= head + tail + 1 ? hash : `${prefix}${body.slice(0, head)}…${body.slice(-tail)}`;
}

/* ------------------------------------------------------------------------ */
/* Validation of network data                                              */
/* ------------------------------------------------------------------------ */
/*
 * Every parser returns a fresh object holding only the contract's fields, or
 * null when anything is missing, mistyped or out of range. A malformed
 * snapshot is rejected whole: a dashboard that shows money must not render
 * a partial or guessed state.
 */

type Fields = Record<string, unknown>;

const HEX_RE = /^(0x)?[0-9a-fA-F]+$/;
const MAX_HOLDINGS = 1_000;

export function isRecord(value: unknown): value is Fields {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A string of at most `max` characters, possibly empty. */
function isString(value: unknown, max = 200): value is string {
  return typeof value === "string" && value.length <= max;
}

/** A non-blank string of at most `max` characters. */
export function isText(value: unknown, max = 200): value is string {
  return isString(value, max) && value.trim().length > 0;
}

export function isOneOf<T extends string>(options: readonly T[], value: unknown): value is T {
  return typeof value === "string" && (options as readonly string[]).includes(value);
}

/**
 * Stream position: the account's version, a non-negative safe integer. A new
 * account starts at 0 (migration 0012) and its first snapshot carries it.
 */
function isSeq(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** A decimal string the money helpers accept: optional minus, digits, at most 8 decimals, no spaces. */
export function isDecimalString(value: unknown): value is DecimalString {
  if (typeof value !== "string" || value !== value.trim()) return false;
  try {
    toUnits(value);
    return true;
  } catch {
    return false;
  }
}

export function isNonNegative(value: unknown): value is DecimalString {
  return isDecimalString(value) && toUnits(value) >= 0n;
}

export function isPositive(value: unknown): value is DecimalString {
  return isDecimalString(value) && toUnits(value) > 0n;
}

export function parseVaultBalance(value: unknown): VaultBalance | null {
  if (!isRecord(value)) return null;
  const { total_mxn, available_cash, invested_assets, unrealized_pnl_percentage } = value;
  if (
    !isDecimalString(total_mxn) ||
    !isDecimalString(available_cash) ||
    !isNonNegative(invested_assets) ||
    !isDecimalString(unrealized_pnl_percentage)
  ) {
    return null;
  }
  return { total_mxn, available_cash, invested_assets, unrealized_pnl_percentage };
}

export function parseReserveProof(value: unknown): ReserveProof | null {
  if (!isRecord(value)) return null;
  const { status, zk_root_hash } = value;
  if (!isOneOf(RESERVE_PROOF_STATUSES, status) || !isText(zk_root_hash, 130) || !HEX_RE.test(zk_root_hash)) {
    return null;
  }
  return { status, zk_root_hash };
}

export function parseHolding(value: unknown): Holding | null {
  if (!isRecord(value)) return null;
  const { ticker, name, shares, current_price, total_value, daily_change_pct } = value;
  if (
    !isText(ticker, 32) ||
    !isText(name) ||
    !isNonNegative(shares) ||
    !isNonNegative(current_price) ||
    !isNonNegative(total_value) ||
    !isDecimalString(daily_change_pct)
  ) {
    return null;
  }
  return { ticker, name, shares, current_price, total_value, daily_change_pct };
}

export function parseVaultSnapshot(value: unknown): VaultSnapshot | null {
  if (!isRecord(value)) return null;
  const { user_id, clabe, bank_name } = value;
  if (!isText(user_id, 64) || !isText(clabe, 32) || !isText(bank_name)) return null;

  const balance = parseVaultBalance(value.balance);
  const reserve_proof = parseReserveProof(value.reserve_proof);
  if (!balance || !reserve_proof) return null;

  if (!Array.isArray(value.holdings) || value.holdings.length > MAX_HOLDINGS) return null;
  const holdings: Holding[] = [];
  const tickers = new Set<string>();
  for (const raw of value.holdings) {
    const holding = parseHolding(raw);
    // One position per ticker: a duplicate would double-count it.
    if (!holding || tickers.has(holding.ticker)) return null;
    tickers.add(holding.ticker);
    holdings.push(holding);
  }
  // The CLABE is kept as sent even if malformed: the deposit card flags a bad check digit
  // instead of the whole dashboard failing.
  return { user_id, clabe, bank_name, balance, reserve_proof, holdings };
}

export function parseSpeiTransfer(value: unknown): SpeiTransfer | null {
  if (!isRecord(value)) return null;
  const { tracking_key, direction, status, amount, counterparty_name, counterparty_bank, occurred_at } = value;
  if (
    !isText(tracking_key, 30) ||
    !isOneOf(SPEI_DIRECTIONS, direction) ||
    !isOneOf(SPEI_TRANSFER_STATUSES, status) ||
    !isPositive(amount) ||
    !isString(counterparty_name) ||
    !isString(counterparty_bank) ||
    !isText(occurred_at, 40) ||
    Number.isNaN(Date.parse(occurred_at))
  ) {
    return null;
  }
  return { tracking_key, direction, status, amount, counterparty_name, counterparty_bank, occurred_at };
}

/**
 * Validates a real-time message: a WebSocket text frame (JSON string) or an
 * already-parsed object. Binary frames, unknown types and malformed payloads
 * give null.
 */
export function parseRealtimeMessage(raw: unknown): VaultRealtimeMessage | null {
  let value: unknown = raw;
  if (typeof raw === "string") {
    try {
      value = JSON.parse(raw);
    } catch {
      return null;
    }
  }
  if (!isRecord(value) || !isSeq(value.seq)) return null;
  const { seq } = value;

  switch (value.type) {
    case "vault.snapshot": {
      const vault = parseVaultSnapshot(value.vault);
      return vault ? { type: "vault.snapshot", seq, vault } : null;
    }
    case "spei.transfer": {
      const transfer = parseSpeiTransfer(value.transfer);
      const balance = parseVaultBalance(value.balance);
      return transfer && balance ? { type: "spei.transfer", seq, transfer, balance } : null;
    }
    case "holding.updated": {
      const holding = parseHolding(value.holding);
      const balance = parseVaultBalance(value.balance);
      return holding && balance ? { type: "holding.updated", seq, holding, balance } : null;
    }
    case "reserve.updated": {
      const reserve_proof = parseReserveProof(value.reserve_proof);
      return reserve_proof ? { type: "reserve.updated", seq, reserve_proof } : null;
    }
    default:
      return null;
  }
}
