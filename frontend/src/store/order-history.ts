"use client";

/**
 * Order history (`GET /api/v1/market/orders/history`) for the Orders page
 * (components/markets/order-history-list.tsx): the pages loaded so far,
 * newest first, and the cursor for the next.
 *
 * - `load` fetches the first page. Again later (the vault moved: an order
 *   filled, one was cancelled), it puts the first page's new orders on top and
 *   keeps the older pages already loaded.
 * - `loadMore` fetches the page after the last one shown.
 * - Every page is validated whole; a failure keeps what is shown.
 *
 * Client-only; `reset()` on sign-out. The fetcher is injected, so
 * `node --test` runs it.
 */

import { create } from "zustand";

import { appendPage, parseOrderHistory } from "../lib/order-history.ts";
import type { OrderHistoryEntry } from "../types/market.ts";

/** Fetches the page after `cursor` (the first one for null) and resolves with the parsed JSON body. */
export type HistoryFetcher = (cursor: string | null, signal: AbortSignal) => Promise<unknown>;

export type HistoryStatus = "idle" | "loading" | "ready" | "error";

export interface HistoryFields {
  orders: OrderHistoryEntry[];
  /** For the next page; null when the last one is shown. */
  cursor: string | null;
  status: HistoryStatus;
  loadingMore: boolean;
  error: Error | null;
}

export interface HistoryActions {
  load: (fetchPage: HistoryFetcher) => Promise<void>;
  loadMore: (fetchPage: HistoryFetcher) => Promise<void>;
  reset: () => void;
}

export type OrderHistoryState = HistoryFields & HistoryActions;

const INITIAL: HistoryFields = { orders: [], cursor: null, status: "idle", loadingMore: false, error: null };

let inflight: AbortController | null = null;

export const useOrderHistoryStore = create<OrderHistoryState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchPage) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    const first = get().status !== "ready";
    if (first) set({ status: "loading", error: null });
    try {
      const page = parseOrderHistory(await fetchPage(null, controller.signal));
      if (controller.signal.aborted) return;
      if (!page) throw new Error("The order history could not be read.");
      // A first load takes the page and its cursor; a refresh puts new orders on top of the pages shown.
      if (first) set({ orders: page.orders, cursor: page.next_cursor, status: "ready", error: null });
      else set((state) => ({ orders: appendPage(page.orders, state.orders), status: "ready", error: null }));
    } catch (cause) {
      if (controller.signal.aborted) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().status === "ready" ? "ready" : "error", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  loadMore: async (fetchPage) => {
    const { cursor, loadingMore, status } = get();
    if (cursor === null || loadingMore || status !== "ready") return;
    set({ loadingMore: true, error: null });
    try {
      const page = parseOrderHistory(await fetchPage(cursor, new AbortController().signal));
      if (!page) throw new Error("The order history could not be read.");
      set((state) => ({ orders: appendPage(state.orders, page.orders), cursor: page.next_cursor, loadingMore: false }));
    } catch (cause) {
      set({ loadingMore: false, error: cause instanceof Error ? cause : new Error(String(cause)) });
    }
  },

  reset: () => {
    inflight?.abort();
    inflight = null;
    set({ ...INITIAL });
  },
}));
