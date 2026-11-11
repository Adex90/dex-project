/**
 * Allocation: how the vault's net worth splits between its cash, each market
 * holding and its fixed-term investments, for the Vault's portfolio
 * analytics (components/vault/portfolio-analytics.tsx).
 *
 * - The parts. Cash is everything in the vault's account (available, held
 *   for open orders, on hold): the total minus what is invested. Each holding
 *   counts at its market value. Fixed term is what is invested beyond the
 *   holdings (SOFOM mutuum contracts, at cost). Together they are the vault's
 *   total, as the backend adds it up (backend/src/boveda/consulta.rs).
 * - Exact: amounts in BigInt units of 10^-8; shares in basis points that add
 *   up to exactly 10,000 (100.00 %) by the largest remainder, so the bar, the
 *   list and the total never disagree.
 * - At most `maxParts` parts: cash first, then at most `maxParts` − 1
 *   investments, largest first; past that, the smallest fold into one Other
 *   part that names them. A part worth nothing is left out.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { fromUnits, toUnits } from "./decimal.ts";
import type { DecimalString, Holding, VaultBalance } from "../types/vault.ts";

export type AllocationKind = "cash" | "holding" | "fixed_term" | "other";

export interface AllocationPart {
  /** Unique within the allocation: "cash", the ticker, "fixed-term" or "other". */
  key: string;
  kind: AllocationKind;
  /** "Cash", "STSLA", "Fixed term", "Other". */
  label: string;
  /** What it is: the asset's name, "SOFOM mutuum contracts", or what Other folds ("AGT1, SCT28"). */
  detail: string;
  value: DecimalString;
  /** Its share of the total in basis points (4620 is 46.20 %). */
  basisPoints: number;
  /** The tickers it stands for: a holding's own, those Other folds. */
  tickers: string[];
}

export interface Allocation {
  /** The sum of the parts: the vault's total. */
  total: DecimalString;
  /** Cash first, then the investments largest first, Other last. */
  parts: AllocationPart[];
  /** Cash's share in basis points; the rest (10,000 minus it, with any parts) is invested. */
  cashBasisPoints: number;
}

/**
 * Cash and four investments (or three and Other). The bar colors them gold
 * and a four-step zinc scale: a fifth step would sit too close to its
 * neighbour on the charcoal surface to tell apart.
 */
export const MAX_PARTS = 5;

type Draft = Omit<AllocationPart, "value" | "basisPoints"> & { units: bigint };

/** The vault's allocation (`useVaultStore`'s balance and holdings). */
export function allocation(balance: VaultBalance, holdings: readonly Holding[], maxParts = MAX_PARTS): Allocation {
  const invested = toUnits(balance.invested_assets);
  const cash = positive(toUnits(balance.total_mxn) - invested);
  const held = holdings
    .map((holding) => ({ holding, units: toUnits(holding.total_value) }))
    .filter(({ units }) => units > 0n);
  const fixedTerm = positive(invested - held.reduce((sum, { units }) => sum + units, 0n));

  const investments: Draft[] = held.map(({ holding, units }) => ({
    key: holding.ticker,
    kind: "holding",
    label: holding.ticker,
    detail: holding.name,
    units,
    tickers: [holding.ticker],
  }));
  if (fixedTerm > 0n) {
    investments.push({
      key: "fixed-term",
      kind: "fixed_term",
      label: "Fixed term",
      detail: "SOFOM mutuum contracts",
      units: fixedTerm,
      tickers: [],
    });
  }
  investments.sort((a, b) => (a.units === b.units ? a.label.localeCompare(b.label) : a.units > b.units ? -1 : 1));

  const room = Math.max(2, maxParts - 1);
  let shown = investments;
  if (investments.length > room) {
    const folded = investments.slice(room - 1);
    shown = [
      ...investments.slice(0, room - 1),
      {
        key: "other",
        kind: "other",
        label: "Other",
        detail: folded.map((part) => part.label).join(", "),
        units: folded.reduce((sum, part) => sum + part.units, 0n),
        tickers: folded.flatMap((part) => part.tickers),
      },
    ];
  }

  const drafts: Draft[] =
    cash > 0n
      ? [
          { key: "cash", kind: "cash", label: "Cash", detail: "In your vault account", units: cash, tickers: [] },
          ...shown,
        ]
      : shown;
  const total = drafts.reduce((sum, part) => sum + part.units, 0n);
  const shares = basisPoints(
    drafts.map((part) => part.units),
    total,
  );
  const parts = drafts.map(({ units, ...part }, i) => ({ ...part, value: fromUnits(units), basisPoints: shares[i]! }));
  return {
    total: fromUnits(total),
    parts,
    cashBasisPoints: parts.find((part) => part.kind === "cash")?.basisPoints ?? 0,
  };
}

/**
 * Each value's share of `total` in basis points, adding up to exactly 10,000:
 * each rounded down, then the basis points left over go one each to the
 * largest remainders (the larger value first on a tie, then the earlier).
 * All zeros for a total that is not positive.
 */
export function basisPoints(values: readonly bigint[], total: bigint): number[] {
  if (total <= 0n) return values.map(() => 0);
  const floors = values.map((value) => (value * 10_000n) / total);
  const remainders = values.map((value) => (value * 10_000n) % total);
  let left = 10_000n - floors.reduce((sum, floor) => sum + floor, 0n);
  const order = values
    .map((_, i) => i)
    .sort((a, b) => {
      if (remainders[a] !== remainders[b]) return remainders[a]! > remainders[b]! ? -1 : 1;
      if (values[a] !== values[b]) return values[a]! > values[b]! ? -1 : 1;
      return a - b;
    });
  const shares = floors.map(Number);
  for (const i of order) {
    if (left <= 0n) break;
    shares[i] = shares[i]! + 1;
    left -= 1n;
  }
  return shares;
}

/** "46.20%"; a part too small to show a hundredth reads "<0.01%". */
export function formatShare(basisPoints: number, value?: DecimalString): string {
  if (basisPoints === 0 && value !== undefined && toUnits(value) > 0n) return "<0.01%";
  return `${Math.floor(basisPoints / 100)}.${String(basisPoints % 100).padStart(2, "0")}%`;
}

/** What makes up the vault's cash: available, held for open orders, and on hold (whatever else the total has). */
export function cashBreakdown(balance: VaultBalance): {
  available: DecimalString;
  held: DecimalString;
  onHold: DecimalString;
} {
  const cash = positive(toUnits(balance.total_mxn) - toUnits(balance.invested_assets));
  const available = toUnits(balance.available_cash);
  const held = toUnits(balance.held_cash);
  return {
    available: fromUnits(available),
    held: fromUnits(held),
    onHold: fromUnits(positive(cash - available - held)),
  };
}

function positive(units: bigint): bigint {
  return units > 0n ? units : 0n;
}
