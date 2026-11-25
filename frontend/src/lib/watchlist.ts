/**
 * The watchlist (migration 0024): the assets a client follows, held or not.
 * The star on Discover's rows and on an asset's page adds or removes one; the
 * Vault lists those the client follows without holding them (a held asset is
 * already among the Assets).
 *
 * Network data is validated whole before anything renders it. Pure functions
 * with relative `.ts` imports, so `node --test` runs them.
 */

import { toUnits } from "./decimal.ts";
import { parseMarketListing } from "./market-data.ts";
import { isRecord, isText } from "./vault.ts";
import type { Watchlist, WatchlistItem, WatchState } from "../types/market.ts";
import type { Holding } from "../types/vault.ts";

/** The backend keeps at most 50 per client; far beyond that is not a list. */
const MAX_ITEMS = 500;

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

/** `GET /api/v1/market/watchlist`; null if any item is malformed or repeats. */
export function parseWatchlist(value: unknown): Watchlist | null {
  if (!isRecord(value) || !Array.isArray(value.items) || value.items.length > MAX_ITEMS) return null;
  const items: WatchlistItem[] = [];
  const tickers = new Set<string>();
  for (const raw of value.items) {
    if (!isRecord(raw)) return null;
    const asset = parseMarketListing(raw.asset);
    const { ticker, added_at } = raw;
    if (!asset || ticker !== asset.ticker || !isInstant(added_at) || tickers.has(asset.ticker)) return null;
    tickers.add(asset.ticker);
    items.push({ ticker: asset.ticker, added_at, asset });
  }
  return { items };
}

/** `POST /api/v1/market/watchlist/{ticker}/toggle`; null if malformed or incoherent. */
export function parseWatchState(value: unknown): WatchState | null {
  if (!isRecord(value)) return null;
  const { ticker, watched, added_at } = value;
  if (!isText(ticker, 12) || typeof watched !== "boolean") return null;
  if (watched ? !isInstant(added_at) : added_at !== null) return null;
  return { ticker, watched, added_at: watched ? (added_at as string) : null };
}

/** What the Vault's Watchlist shows: the assets followed and not held, newest first. */
export function notHeld(items: readonly WatchlistItem[], holdings: readonly Pick<Holding, "ticker" | "shares">[]) {
  const held = new Set(holdings.filter((h) => toUnits(h.shares) > 0n).map((h) => h.ticker));
  return items.filter((item) => !held.has(item.ticker));
}

/** The star's label: what pressing it does. */
export function watchLabel(ticker: string, watched: boolean): string {
  return watched ? `Remove ${ticker} from your watchlist` : `Add ${ticker} to your watchlist`;
}
