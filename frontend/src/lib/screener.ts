/**
 * The Discover tab's screener: what narrows the catalog (the asset class and
 * the direction of the 24-hour change), how many assets each choice would
 * leave, the thematic collections that preset it, and the day's top movers.
 * All of it client-side, from `GET /api/v1/market/assets` (`MarketListing`).
 *
 * - Within a dimension the choices add up (Commodities or Fixed income);
 *   across dimensions they narrow (Commodities and Positive). No choice in a
 *   dimension means all of it.
 * - A direction is the one the list shows: the 24-hour change rounded to two
 *   decimals, as `DailyChange` prints it (`signedPercent`). "0.00%" is
 *   neither positive nor negative, so a filter never disagrees with a color.
 * - A collection is a preset of asset classes: picking one sets the
 *   screener's classes, so the cards and the toggles always agree. It is
 *   active while the classes are exactly its own.
 * - Exact: changes are compared and averaged as decimals (BigInt units of
 *   10^-8, lib/decimal.ts).
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { divRoundHalfEven, fromUnits, toUnits } from "./decimal.ts";
import { trending } from "./discover.ts";
import { signedPercent, type Trend } from "./vault.ts";
import type { MarketListing } from "../types/market.ts";
import type { DecimalString } from "../types/vault.ts";

export type Direction = "up" | "down";

export interface ScreenerFilters {
  /** Asset classes as the API names them ("Commodity"); none for every class. */
  classes: readonly string[];
  /** Directions of the 24-hour change; none for any. */
  directions: readonly Direction[];
}

export const NO_FILTERS: ScreenerFilters = { classes: [], directions: [] };

/** Each class the API sends (`asset_class`) and its name in the screener, in the screener's order. */
const CLASS_LABELS: readonly (readonly [string, string])[] = [
  ["Equity", "Equities"],
  ["Commodity", "Commodities"],
  ["Fixed income", "Fixed income"],
  ["Fund", "Funds"],
];
/** Offered even when the catalog lists none of them. */
const CORE_CLASSES: readonly string[] = ["Equity", "Commodity", "Fixed income"];

export const DIRECTION_OPTIONS: readonly { value: Direction; label: string }[] = [
  { value: "up", label: "Positive" },
  { value: "down", label: "Negative" },
];

export interface ClassOption {
  /** The class as the API names it. */
  value: string;
  label: string;
}

/**
 * The classes to offer: equities, commodities and fixed income always, then
 * funds and any class this build does not know yet, when the catalog lists
 * them (an unknown one under the API's own name).
 */
export function classOptions(assets: readonly MarketListing[]): ClassOption[] {
  const listed = new Set(assets.map((asset) => asset.asset_class));
  const known = CLASS_LABELS.filter(([value]) => CORE_CLASSES.includes(value) || listed.has(value));
  const unknown = [...listed].filter((value) => !CLASS_LABELS.some(([known]) => known === value)).sort();
  return [...known.map(([value, label]) => ({ value, label })), ...unknown.map((value) => ({ value, label: value }))];
}

/** A class's name in the screener: "Commodities" for "Commodity"; an unknown class keeps the API's name. */
export function classLabel(value: string): string {
  return CLASS_LABELS.find(([known]) => known === value)?.[1] ?? value;
}

/** Where a class sorts: the screener's order, unknown classes last. */
function classOrder(value: string): number {
  const index = CLASS_LABELS.findIndex(([known]) => known === value);
  return index === -1 ? CLASS_LABELS.length : index;
}

/** The direction of an asset's 24-hour change as the list prints it: up, down, or flat at 0.00 %. */
export function direction(asset: Pick<MarketListing, "change_24h_pct">): Trend {
  return signedPercent(asset.change_24h_pct).trend;
}

export function isFiltered(filters: ScreenerFilters): boolean {
  return filters.classes.length > 0 || filters.directions.length > 0;
}

function inClasses(asset: MarketListing, classes: readonly string[]): boolean {
  return classes.length === 0 || classes.includes(asset.asset_class);
}

function inDirections(asset: MarketListing, directions: readonly Direction[]): boolean {
  if (directions.length === 0) return true;
  const trend = direction(asset);
  return trend !== "flat" && directions.includes(trend);
}

/** The assets that pass the filters, in the order given. */
export function screen(assets: readonly MarketListing[], filters: ScreenerFilters): MarketListing[] {
  return assets.filter((asset) => inClasses(asset, filters.classes) && inDirections(asset, filters.directions));
}

export interface FacetCounts {
  /** By class (as the API names it): how many the list would show with that class and the directions chosen. */
  classes: Readonly<Record<string, number>>;
  /** By direction: how many the list would show with that direction and the classes chosen. */
  directions: Readonly<Record<Direction, number>>;
}

/**
 * What each choice would leave of `assets` (the catalog as searched), given
 * the choices in the other dimension: the screener's counts, so a choice
 * that leads nowhere can say so before it is made.
 */
export function facetCounts(assets: readonly MarketListing[], filters: ScreenerFilters): FacetCounts {
  const classes: Record<string, number> = {};
  const directions: Record<Direction, number> = { up: 0, down: 0 };
  for (const asset of assets) {
    if (inDirections(asset, filters.directions)) classes[asset.asset_class] = (classes[asset.asset_class] ?? 0) + 1;
    const trend = direction(asset);
    if (trend !== "flat" && inClasses(asset, filters.classes)) directions[trend] += 1;
  }
  return { classes, directions };
}

/** Turns a class on, or off when it was on. */
export function toggleClass(filters: ScreenerFilters, value: string): ScreenerFilters {
  const on = filters.classes.includes(value);
  return { ...filters, classes: on ? filters.classes.filter((c) => c !== value) : [...filters.classes, value] };
}

/** Turns a direction on, or off when it was on. */
export function toggleDirection(filters: ScreenerFilters, value: Direction): ScreenerFilters {
  const on = filters.directions.includes(value);
  return {
    ...filters,
    directions: on ? filters.directions.filter((d) => d !== value) : [...filters.directions, value],
  };
}

/** The filters in words, in the screener's order: "Commodities, Fixed income · Positive 24h". Empty without filters. */
export function describeFilters(filters: ScreenerFilters): string {
  const classes = [...filters.classes]
    .sort((a, b) => classOrder(a) - classOrder(b) || (a < b ? -1 : a > b ? 1 : 0))
    .map(classLabel)
    .join(", ");
  const directions = DIRECTION_OPTIONS.filter((option) => filters.directions.includes(option.value))
    .map((option) => option.label)
    .join(" or ");
  return [classes, directions ? `${directions} 24h` : ""].filter(Boolean).join(" · ");
}

/* ------------------------------------------------------------------------ */
/* Collections                                                              */
/* ------------------------------------------------------------------------ */

export interface Collection {
  id: string;
  title: string;
  description: string;
  /** The asset classes it presets in the screener. */
  classes: readonly string[];
}

/** Discover's thematic collections, each a preset of the screener's classes. */
export const COLLECTIONS: readonly Collection[] = [
  {
    id: "hard-assets",
    title: "Hard Assets",
    description: "Tokenized commodities, such as gold and silver.",
    classes: ["Commodity"],
  },
  {
    id: "synthetic-equities",
    title: "Synthetic Equities",
    description: "Synthetic shares that track listed companies.",
    classes: ["Equity"],
  },
  {
    id: "fixed-income",
    title: "Fixed Income",
    description: "Government bills and T-bills that pay a yield.",
    classes: ["Fixed income"],
  },
];

/** A collection is active while the screener's classes are exactly its own (whatever the direction). */
export function isCollectionActive(filters: ScreenerFilters, collection: Collection): boolean {
  return (
    filters.classes.length === collection.classes.length && collection.classes.every((c) => filters.classes.includes(c))
  );
}

/** Picking a collection: its classes replace the screener's, the direction stays; picking the active one clears them. */
export function applyCollection(filters: ScreenerFilters, collection: Collection): ScreenerFilters {
  return { ...filters, classes: isCollectionActive(filters, collection) ? [] : [...collection.classes] };
}

export interface CollectionStats {
  /** Its assets in the catalog, most traded first. */
  members: MarketListing[];
  /**
   * Its equal-weight 24-hour change: the mean of its assets' changes, in
   * percent, exact to 10^-8 (half-even). What a basket holding the same
   * amount of each did over the day. Null when it has no assets.
   */
  change: DecimalString | null;
}

export function collectionStats(assets: readonly MarketListing[], collection: Collection): CollectionStats {
  const members = trending(assets.filter((asset) => collection.classes.includes(asset.asset_class)));
  if (members.length === 0) return { members, change: null };
  const sum = members.reduce((total, asset) => total + toUnits(asset.change_24h_pct), 0n);
  return { members, change: fromUnits(divRoundHalfEven(sum, BigInt(members.length))) };
}

/* ------------------------------------------------------------------------ */
/* Top movers                                                               */
/* ------------------------------------------------------------------------ */

export interface TopMovers {
  /** The largest 24-hour rise; null when nothing rose. */
  gainer: MarketListing | null;
  /** The largest 24-hour fall; null when nothing fell. */
  loser: MarketListing | null;
}

/**
 * The day's top movers in the whole catalog, compared exactly. Only a change
 * the list shows as positive (or negative) counts; on a tie the more traded
 * asset wins, then the ticker (the trending order).
 */
export function topMovers(assets: readonly MarketListing[]): TopMovers {
  let gainer: MarketListing | null = null;
  let loser: MarketListing | null = null;
  for (const asset of trending(assets)) {
    const trend = direction(asset);
    const change = toUnits(asset.change_24h_pct);
    if (trend === "up" && (gainer === null || change > toUnits(gainer.change_24h_pct))) gainer = asset;
    if (trend === "down" && (loser === null || change < toUnits(loser.change_24h_pct))) loser = asset;
  }
  return { gainer, loser };
}
