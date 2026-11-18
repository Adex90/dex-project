/**
 * Pure helpers of the Solvency Explorer (the Reserve tab): the reserve ratio,
 * the balances a cash leaf commits to, and how the positions in a snapshot
 * compare with what the client holds now.
 */
import type { DesgloseSaldo } from "./api/types.ts";
import { HOLD_COPY } from "./aml.ts";
import { compareAmounts, isZero, toUnits } from "./decimal.ts";

/**
 * Custody over liabilities in percent, rounded DOWN to two decimals: a reserve
 * one cent short never reads 100.00. Null when there are no liabilities.
 */
export function reserveRatioPct(reserve: string, liabilities: string): string | null {
  const r = toUnits(reserve);
  const l = toUnits(liabilities);
  if (l <= 0n || r < 0n) return null;
  const basisPoints = (r * 10_000n) / l;
  return `${basisPoints / 100n}.${(basisPoints % 100n).toString().padStart(2, "0")}`;
}

/** Width of a bar against the larger of two amounts, in percent (for display only). */
export function barShare(amount: string, other: string): number {
  const a = toUnits(amount);
  const max = a > toUnits(other) ? a : toUnits(other);
  if (max <= 0n) return 0;
  return Number((a * 10_000n) / max) / 100;
}

export interface BalanceLine {
  key: "available" | "orders" | "hold";
  label: string;
  amount: string;
}

/**
 * What the cash leaf's balance was made of at the snapshot, line by line. The
 * leaf commits to the total; the database checked the breakdown when it was
 * published. Funds on hold use the neutral copy every client screen uses.
 * Empty for snapshots from before the breakdown existed.
 */
export function includedBalances(desglose: DesgloseSaldo | null): BalanceLine[] {
  if (!desglose) return [];
  const lines: BalanceLine[] = [{ key: "available", label: "Available cash", amount: desglose.disponible }];
  if (!isZero(desglose.comprometido)) {
    lines.push({ key: "orders", label: "Held for open orders", amount: desglose.comprometido });
  }
  if (!isZero(desglose.bloqueado)) {
    lines.push({ key: "hold", label: `Funds ${HOLD_COPY}`, amount: desglose.bloqueado });
  }
  return lines;
}

export interface PositionRow {
  ticker: string;
  name: string | null;
  /** Quantity committed in the snapshot; null when bought after it. */
  atSnapshot: string | null;
  /** Quantity held now; null when no longer held. */
  now: string | null;
  /** included: same as now; changed: different now (or sold); after: not in the snapshot yet. */
  status: "included" | "changed" | "after";
}

/** The snapshot's positions next to the client's holdings now, by ticker. */
export function positionRows(
  inSnapshot: readonly { hoja: { ticker: string; cantidad: string } }[],
  holdings: readonly { ticker: string; name: string; shares: string }[],
): PositionRow[] {
  const rows = new Map<string, PositionRow>();
  for (const p of inSnapshot) {
    rows.set(p.hoja.ticker, {
      ticker: p.hoja.ticker,
      name: null,
      atSnapshot: p.hoja.cantidad,
      now: null,
      status: "included",
    });
  }
  for (const h of holdings) {
    if (isZero(h.shares)) continue;
    const row = rows.get(h.ticker);
    if (row) {
      row.name = h.name;
      row.now = h.shares;
    } else {
      rows.set(h.ticker, { ticker: h.ticker, name: h.name, atSnapshot: null, now: h.shares, status: "after" });
    }
  }
  for (const row of rows.values()) {
    if (row.atSnapshot !== null && (row.now === null || compareAmounts(row.atSnapshot, row.now) !== 0)) {
      row.status = "changed";
    }
  }
  return [...rows.values()].sort((a, b) => a.ticker.localeCompare(b.ticker));
}

/** Units of an asset for display: up to 8 decimals, trailing zeros trimmed, at least 2. */
export function formatUnits(value: string): string {
  const [int = "0", frac = ""] = value.split(".");
  const trimmed = frac.replace(/0+$/, "");
  const grouped = Number(int).toLocaleString("en-US");
  return `${grouped}.${trimmed.padEnd(2, "0")}`;
}
