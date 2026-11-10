/**
 * The Discover tab's catalog logic: what "trending" means and how the search
 * matches. Pure functions with relative `.ts` imports, so `node --test` runs
 * them directly.
 */

import { toUnits } from "./decimal.ts";
import type { MarketListing } from "../types/market.ts";

/** Lowercase and single-spaced, without accents: "CETES  días" reads "cetes dias". */
export function normalize(text: string): string {
  return text
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/\s+/g, " ")
    .trim();
}

/** 24-hour volume in exact units; an unknown volume sorts below any known one. */
function volume(asset: MarketListing): bigint {
  return asset.volume_24h === null ? -1n : toUnits(asset.volume_24h);
}

/** Trending: the most traded over the last 24 hours first, then by ticker. */
export function trending(assets: readonly MarketListing[]): MarketListing[] {
  return [...assets].sort((a, b) => {
    const [va, vb] = [volume(a), volume(b)];
    if (va !== vb) return va > vb ? -1 : 1;
    return a.ticker < b.ticker ? -1 : a.ticker > b.ticker ? 1 : 0;
  });
}

/**
 * How well `asset` answers the query's words, lower is better; null when a
 * word is missing. Every word must appear in the ticker, the name or the
 * asset class. A ticker that is the query ranks first, then one that starts
 * with it, then a word of the name that starts with the first word, then any
 * other match.
 */
function rank(asset: MarketListing, words: readonly string[]): number | null {
  const ticker = asset.ticker.toLowerCase();
  const name = normalize(asset.name);
  const fields = `${ticker} ${name} ${normalize(asset.asset_class)}`;
  if (!words.every((word) => fields.includes(word))) return null;
  const query = words.join(" ");
  const [first = query] = words;
  if (ticker === query) return 0;
  if (ticker.startsWith(query)) return 1;
  if (name.split(/[^a-z0-9]+/).some((part) => part.startsWith(first))) return 2;
  return 3;
}

/**
 * The assets that match `query`, best match first; equal matches keep the
 * trending order. An empty query is the whole catalog, trending. Accents and
 * case never matter: "tesla", "TSLA" and "sTSLA" all find STSLA.
 */
export function searchCatalog(assets: readonly MarketListing[], query: string): MarketListing[] {
  const ordered = trending(assets);
  const words = normalize(query).split(" ").filter(Boolean);
  if (words.length === 0) return ordered;
  return ordered
    .map((asset, order) => ({ asset, order, rank: rank(asset, words) }))
    .filter((hit): hit is { asset: MarketListing; order: number; rank: number } => hit.rank !== null)
    .sort((a, b) => a.rank - b.rank || a.order - b.order)
    .map((hit) => hit.asset);
}
