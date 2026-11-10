"use client";

/**
 * Market store: the catalog the Discover tab lists and the search typed
 * there. Both outlive the page, so coming back from an asset shows the same
 * results at once while the prices refresh behind them.
 *
 * The catalog is validated whole before it is stored (lib/market-data.ts).
 * The dashboard shell calls `reset()` when it unmounts (sign-out), so the
 * next person in this tab starts from an empty search.
 */

import { create } from "zustand";

import { parseMarketCatalog } from "../lib/market-data.ts";
import type { MarketCatalog } from "../types/market.ts";

/** Fetches the raw catalog (`api.marketAssets`); injected so the store stays testable. */
export type CatalogFetcher = (signal: AbortSignal) => Promise<unknown>;

/** `error` only while there is nothing to show: a failed refresh stays `ready` and sets `error`. */
export type CatalogStatus = "idle" | "loading" | "ready" | "error";

export interface MarketFields {
  catalog: MarketCatalog | null;
  status: CatalogStatus;
  error: Error | null;
  /** What is typed in Discover's search, as typed. */
  query: string;
}

export interface MarketActions {
  /** Loads the catalog over REST. Cancels an earlier load still in flight. */
  load: (fetchCatalog: CatalogFetcher) => Promise<void>;
  setQuery: (query: string) => void;
  /** Clears everything (sign-out) and cancels a load in flight. */
  reset: () => void;
}

export type MarketState = MarketFields & MarketActions;

export const UNREADABLE_CATALOG = "The market catalog could not be read. Please try again.";

const INITIAL: MarketFields = { catalog: null, status: "idle", error: null, query: "" };

let inflight: AbortController | null = null;

export const useMarketStore = create<MarketState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchCatalog) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    // Keep the list on screen during a refresh: the loading state is for the first load only.
    if (get().catalog === null) set({ status: "loading", error: null });
    try {
      const body = await fetchCatalog(controller.signal);
      if (controller.signal.aborted) return;
      const catalog = parseMarketCatalog(body);
      if (!catalog) throw new Error(UNREADABLE_CATALOG);
      set({ catalog, status: "ready", error: null });
    } catch (cause) {
      if (controller.signal.aborted) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().catalog === null ? "error" : "ready", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  setQuery: (query) => set({ query }),

  reset: () => {
    inflight?.abort();
    inflight = null;
    set({ ...INITIAL });
  },
}));
