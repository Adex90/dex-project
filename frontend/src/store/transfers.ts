"use client";

/**
 * The SPEI tracker (`GET /api/v1/wealth/spei/transfers`) and the client's
 * clarifications (`GET /api/v1/wealth/spei/disputes`), for the Transfers page
 * (app/dashboard/transfers/page.tsx).
 *
 * - `load` fetches the first page of transfers. Again later (the vault moved:
 *   a deposit arrived, a withdrawal settled or came back), it puts the first
 *   page as it is now on top and keeps the older pages already loaded.
 * - `loadMore` fetches the page after the last one shown.
 * - `inspect` opens one transfer in the inspector: from the pages shown when
 *   it is there, else by its id (or the id of a ledger movement it made).
 * - `loadClarifications` fetches the clarifications; `clarificationOpened`
 *   adds one the client just opened, to the list and to its transfer.
 * - Every answer is validated whole; a failure keeps what is shown.
 *
 * Client-only; `reset()` on sign-out. The fetchers are injected, so
 * `node --test` runs it.
 */

import { create } from "zustand";

import {
  appendTransfers,
  mergeFirstPage,
  parseSpeiClarifications,
  parseSpeiTransfers,
  parseTrackedTransfer,
  transferMatches,
  upsertClarification,
  withClarification,
} from "../lib/transfers.ts";
import type { SpeiClarification, TrackedTransfer } from "../types/transfers.ts";

/** Fetches the page after `cursor` (the first one for null) and resolves with the parsed JSON body. */
export type TransfersFetcher = (cursor: string | null, signal: AbortSignal) => Promise<unknown>;
/** Fetches one transfer by its id or by a ledger movement's. */
export type TransferFetcher = (id: string, signal: AbortSignal) => Promise<unknown>;
export type ClarificationsFetcher = (signal: AbortSignal) => Promise<unknown>;

export type LoadStatus = "idle" | "loading" | "ready" | "error";

/** The transfer open in the inspector. */
export interface Inspected {
  /** What was asked for: the transfer's id, or a ledger movement's it made. */
  key: string;
  transfer: TrackedTransfer | null;
  status: "loading" | "ready" | "error";
  error: Error | null;
}

export interface TransfersFields {
  transfers: TrackedTransfer[];
  /** For the next page; null when the last one is shown. */
  cursor: string | null;
  status: LoadStatus;
  loadingMore: boolean;
  error: Error | null;
  clarifications: SpeiClarification[];
  clarificationsStatus: LoadStatus;
  clarificationsError: Error | null;
  inspected: Inspected | null;
}

export interface TransfersActions {
  load: (fetchPage: TransfersFetcher) => Promise<void>;
  loadMore: (fetchPage: TransfersFetcher) => Promise<void>;
  loadClarifications: (fetchList: ClarificationsFetcher) => Promise<void>;
  inspect: (fetchOne: TransferFetcher, key: string) => Promise<void>;
  /** The open transfer again from the API (the vault moved); a failure keeps what is shown. */
  refreshInspected: (fetchOne: TransferFetcher) => Promise<void>;
  closeInspector: () => void;
  clarificationOpened: (clarification: SpeiClarification) => void;
  reset: () => void;
}

export type TransfersState = TransfersFields & TransfersActions;

const INITIAL: TransfersFields = {
  transfers: [],
  cursor: null,
  status: "idle",
  loadingMore: false,
  error: null,
  clarifications: [],
  clarificationsStatus: "idle",
  clarificationsError: null,
  inspected: null,
};

const UNREADABLE_PAGE = "Your transfers could not be read.";
const UNREADABLE_TRANSFER = "The transfer could not be read.";
const UNREADABLE_CLARIFICATIONS = "Your clarifications could not be read.";

function asError(cause: unknown): Error {
  return cause instanceof Error ? cause : new Error(String(cause));
}

let pageInflight: AbortController | null = null;
let clarificationsInflight: AbortController | null = null;
let inspectInflight: AbortController | null = null;

export const useTransfersStore = create<TransfersState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchPage) => {
    pageInflight?.abort();
    const controller = new AbortController();
    pageInflight = controller;
    const first = get().status !== "ready";
    if (first) set({ status: "loading", error: null });
    try {
      const page = parseSpeiTransfers(await fetchPage(null, controller.signal));
      if (controller.signal.aborted) return;
      if (!page) throw new Error(UNREADABLE_PAGE);
      set((state) => {
        // The open transfer as the fresh page has it.
        const open = state.inspected?.transfer ?? null;
        const fresher = open ? page.transfers.find((t) => t.transfer_id === open.transfer_id) : undefined;
        const inspected = fresher && state.inspected ? { ...state.inspected, transfer: fresher } : state.inspected;
        return first
          ? { transfers: page.transfers, cursor: page.next_cursor, status: "ready", error: null, inspected }
          : { transfers: mergeFirstPage(state.transfers, page.transfers), status: "ready", error: null, inspected };
      });
    } catch (cause) {
      if (controller.signal.aborted) return;
      set({ status: get().status === "ready" ? "ready" : "error", error: asError(cause) });
    } finally {
      if (pageInflight === controller) pageInflight = null;
    }
  },

  loadMore: async (fetchPage) => {
    const { cursor, loadingMore, status } = get();
    if (cursor === null || loadingMore || status !== "ready") return;
    set({ loadingMore: true, error: null });
    try {
      const page = parseSpeiTransfers(await fetchPage(cursor, new AbortController().signal));
      if (!page) throw new Error(UNREADABLE_PAGE);
      set((state) => ({
        transfers: appendTransfers(state.transfers, page.transfers),
        cursor: page.next_cursor,
        loadingMore: false,
      }));
    } catch (cause) {
      set({ loadingMore: false, error: asError(cause) });
    }
  },

  loadClarifications: async (fetchList) => {
    clarificationsInflight?.abort();
    const controller = new AbortController();
    clarificationsInflight = controller;
    const first = get().clarificationsStatus !== "ready";
    if (first) set({ clarificationsStatus: "loading", clarificationsError: null });
    try {
      const list = parseSpeiClarifications(await fetchList(controller.signal));
      if (controller.signal.aborted) return;
      if (!list) throw new Error(UNREADABLE_CLARIFICATIONS);
      set({ clarifications: list.clarifications, clarificationsStatus: "ready", clarificationsError: null });
    } catch (cause) {
      if (controller.signal.aborted) return;
      set({
        clarificationsStatus: get().clarificationsStatus === "ready" ? "ready" : "error",
        clarificationsError: asError(cause),
      });
    } finally {
      if (clarificationsInflight === controller) clarificationsInflight = null;
    }
  },

  inspect: async (fetchOne, key) => {
    inspectInflight?.abort();
    inspectInflight = null;
    const shown = get().transfers.find((t) => transferMatches(t, key));
    if (shown) {
      set({ inspected: { key, transfer: shown, status: "ready", error: null } });
      return;
    }
    const controller = new AbortController();
    inspectInflight = controller;
    set({ inspected: { key, transfer: null, status: "loading", error: null } });
    try {
      const transfer = parseTrackedTransfer(await fetchOne(key, controller.signal));
      if (controller.signal.aborted) return;
      if (!transfer) throw new Error(UNREADABLE_TRANSFER);
      set({ inspected: { key, transfer, status: "ready", error: null } });
    } catch (cause) {
      if (controller.signal.aborted) return;
      set({ inspected: { key, transfer: null, status: "error", error: asError(cause) } });
    } finally {
      if (inspectInflight === controller) inspectInflight = null;
    }
  },

  refreshInspected: async (fetchOne) => {
    const current = get().inspected;
    if (!current?.transfer || inspectInflight) return;
    const id = current.transfer.transfer_id;
    const controller = new AbortController();
    inspectInflight = controller;
    try {
      const transfer = parseTrackedTransfer(await fetchOne(id, controller.signal));
      if (controller.signal.aborted || !transfer) return;
      set((state) =>
        state.inspected?.transfer?.transfer_id === id ? { inspected: { ...state.inspected, transfer } } : {},
      );
    } catch {
      // What is shown stays: the next move of the vault tries again.
    } finally {
      if (inspectInflight === controller) inspectInflight = null;
    }
  },

  closeInspector: () => {
    inspectInflight?.abort();
    inspectInflight = null;
    set({ inspected: null });
  },

  clarificationOpened: (clarification) => {
    set((state) => ({
      clarifications: upsertClarification(state.clarifications, clarification),
      transfers: state.transfers.map((t) => withClarification(t, clarification)),
      inspected:
        state.inspected?.transfer != null
          ? { ...state.inspected, transfer: withClarification(state.inspected.transfer, clarification) }
          : state.inspected,
    }));
  },

  reset: () => {
    pageInflight?.abort();
    clarificationsInflight?.abort();
    inspectInflight?.abort();
    pageInflight = null;
    clarificationsInflight = null;
    inspectInflight = null;
    set({ ...INITIAL });
  },
}));
