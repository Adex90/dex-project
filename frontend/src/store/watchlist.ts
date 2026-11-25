"use client";

/**
 * The watchlist (`GET /api/v1/market/watchlist`): the assets the client
 * follows, for the stars on Discover and an asset's page and the Vault's
 * Watchlist (components/markets/watchlist-section.tsx).
 *
 * - `load` validates the list whole; a failed refresh keeps what is shown.
 *   The Vault reloads it whenever the vault moves: following and unfollowing
 *   bump the vault's version, so another device's changes arrive live.
 * - `set` is optimistic and idempotent: the star shows the new state at once
 *   (`pending`) and the request asks for that state (`{"watched": bool}`), so
 *   a retry or a double click cannot flip it back. A failure restores the
 *   star and leaves a `notice`. A load that started before a change settled
 *   is dropped, so it cannot show the old state.
 *
 * Client-only; `reset()` on sign-out. The fetchers are injected, so
 * `node --test` runs it.
 */

import { create } from "zustand";

import { parseWatchlist, parseWatchState } from "../lib/watchlist.ts";
import type { MarketListing, WatchlistItem } from "../types/market.ts";

export type WatchlistFetcher = (signal: AbortSignal) => Promise<unknown>;
/** Asks for `watched` on `ticker` and resolves with the parsed JSON body (the state it stands in). */
export type WatchSetter = (ticker: string, watched: boolean) => Promise<unknown>;

export type WatchlistStatus = "idle" | "loading" | "ready" | "error";

export interface WatchlistFields {
  /** The assets followed, newest first, as the server last answered them. */
  items: WatchlistItem[];
  status: WatchlistStatus;
  error: Error | null;
  /** Changes in flight: the state each star shows until the server answers. */
  pending: Record<string, boolean>;
  /** The last change that failed, for the place that made it to say so. */
  notice: { ticker: string; watched: boolean; error: unknown } | null;
}

export interface WatchlistActions {
  load: (fetchWatchlist: WatchlistFetcher) => Promise<void>;
  /** Follows or unfollows `ticker`; its listing (`asset`) puts a new one in the list before the next load. */
  set: (ticker: string, watched: boolean, setWatched: WatchSetter, asset: MarketListing) => Promise<void>;
  dismissNotice: () => void;
  reset: () => void;
}

export type WatchlistState = WatchlistFields & WatchlistActions;

const INITIAL: WatchlistFields = { items: [], status: "idle", error: null, pending: {}, notice: null };

/** Whether the star for `ticker` shows it followed: the change in flight, else the list. */
export function isWatched(state: Pick<WatchlistFields, "items" | "pending">, ticker: string): boolean {
  return state.pending[ticker] ?? state.items.some((item) => item.ticker === ticker);
}

let inflight: AbortController | null = null;
/** Bumped each time a change settles: a load that started before is out of date. */
let epoch = 0;

export const useWatchlistStore = create<WatchlistState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchWatchlist) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    const started = epoch;
    if (get().status !== "ready") set({ status: "loading", error: null });
    try {
      const body = await fetchWatchlist(controller.signal);
      if (controller.signal.aborted || started !== epoch) return;
      const parsed = parseWatchlist(body);
      if (!parsed) throw new Error("The watchlist could not be read.");
      set({ items: parsed.items, status: "ready", error: null });
    } catch (cause) {
      if (controller.signal.aborted || started !== epoch) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().status === "ready" ? "ready" : "error", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  set: async (ticker, watched, setWatched, asset) => {
    set((state) => ({ pending: { ...state.pending, [ticker]: watched }, notice: null }));
    const settle = (fields: Partial<WatchlistFields>) => {
      epoch += 1;
      set((state) => {
        const pending = { ...state.pending };
        delete pending[ticker];
        return { ...fields, pending };
      });
    };
    try {
      const answer = parseWatchState(await setWatched(ticker, watched));
      // A 2xx means it stands as asked; an answer that cannot be read still is one.
      const addedAt = answer?.ticker === ticker && answer.watched ? answer.added_at : new Date().toISOString();
      const others = get().items.filter((item) => item.ticker !== ticker);
      const items = watched ? [{ ticker, added_at: addedAt ?? new Date().toISOString(), asset }, ...others] : others;
      settle({ items });
    } catch (error) {
      settle({ notice: { ticker, watched, error } });
    }
  },

  dismissNotice: () => set({ notice: null }),

  reset: () => {
    inflight?.abort();
    inflight = null;
    epoch += 1;
    set({ ...INITIAL, pending: {} });
  },
}));
