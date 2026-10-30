"use client";

import { create } from "zustand";

import { ApiError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { PlazoTasa, Tablero, TransaccionIfpe } from "@/lib/api/types";

const PAGE_SIZE = 25;

/**
 * Dashboard data: the summary (vault, investments, live reserve), the
 * immutable ledger feed and the term/rate table. Each section is fetched
 * from its own backend endpoint; the backend reads each under its own
 * PostgreSQL role (IFPE and SOFOM stay isolated even for reads).
 */
interface DashboardState {
  summary: Tablero | null;
  movements: TransaccionIfpe[];
  hasMoreMovements: boolean;
  rates: PlazoTasa[];
  loading: boolean;
  loadingMore: boolean;
  error: ApiError | Error | null;
  lastUpdated: number | null;
  refresh: (token: string) => Promise<void>;
  loadMoreMovements: (token: string) => Promise<void>;
  loadRates: () => Promise<void>;
  reset: () => void;
}

const INITIAL = {
  summary: null,
  movements: [],
  hasMoreMovements: false,
  rates: [],
  loading: false,
  loadingMore: false,
  error: null,
  lastUpdated: null,
};

let inflight: AbortController | null = null;

export const useDashboardStore = create<DashboardState>()((set, get) => ({
  ...INITIAL,

  refresh: async (token) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    set({ loading: get().summary === null });
    try {
      const [summary, latest] = await Promise.all([
        api.dashboard(token, controller.signal),
        api.movements(token, { limit: PAGE_SIZE }, controller.signal),
      ]);
      // Merge the newest page with older pages already loaded, without duplicates.
      const newestSeq = latest.at(-1)?.secuencia ?? Number.POSITIVE_INFINITY;
      const older = get().movements.filter((m) => m.secuencia < newestSeq);
      set({
        summary,
        movements: [...latest, ...older],
        hasMoreMovements: older.length > 0 ? get().hasMoreMovements : latest.length === PAGE_SIZE,
        loading: false,
        error: null,
        lastUpdated: Date.now(),
      });
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return;
      set({ loading: false, error: error as Error });
      throw error;
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  loadMoreMovements: async (token) => {
    const oldest = get().movements.at(-1);
    if (!oldest || get().loadingMore) return;
    set({ loadingMore: true });
    try {
      const page = await api.movements(token, { limit: PAGE_SIZE, before: oldest.secuencia });
      set((s) => ({
        movements: [...s.movements, ...page.filter((m) => m.secuencia < oldest.secuencia)],
        hasMoreMovements: page.length === PAGE_SIZE,
      }));
    } finally {
      set({ loadingMore: false });
    }
  },

  loadRates: async () => {
    if (get().rates.length) return;
    const rates = await api.rates();
    set({ rates });
  },

  reset: () => {
    inflight?.abort();
    set({ ...INITIAL });
  },
}));
