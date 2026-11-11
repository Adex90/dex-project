"use client";

/**
 * Open orders: the client's limit and stop orders still waiting for their
 * price (`GET /api/v1/market/orders/pending`), for the Vault's Open orders
 * list (components/markets/open-orders-list.tsx).
 *
 * - `load` validates the list whole before anything renders it. A failed
 *   refresh keeps what is on screen and sets `error`. The list reloads
 *   whenever the vault moves (the WebSocket's new version): an order placed
 *   elsewhere appears, a filled one disappears.
 * - `cancel` is optimistic: the order leaves the list at once (`cancelling`
 *   hides it) and the cancellation goes to the server. On success it is gone
 *   for good, even from a load that started before; if the server says it is
 *   no longer open (it filled or was rejected meanwhile), it is gone too; any
 *   other failure brings it back. Each outcome leaves a `notice`.
 * - Money is never computed here: the cash and units an order held come
 *   back to the vault through its WebSocket (`useVaultStore`).
 *
 * Client-only, like the vault store: never load during server rendering, and
 * `reset()` on sign-out. The fetchers are injected, so `node --test` runs it.
 */

import { create } from "zustand";

import { parsePendingOrder, parsePendingOrders } from "../lib/market-data.ts";
import type { PendingOrder } from "../types/market.ts";

/** Fetches the pending list and resolves with the parsed JSON body; the store validates it. */
export type OrdersFetcher = (signal: AbortSignal) => Promise<unknown>;
/** Cancels one order and resolves with the parsed JSON body (the cancelled order). */
export type OrderCanceller = (orderId: string) => Promise<unknown>;

export type OrdersStatus = "idle" | "loading" | "ready" | "error";

/** What the last cancellation did, for the list to say. `order` is the order as it was while pending. */
export type OrdersNotice =
  | { kind: "cancelled"; order: PendingOrder }
  | { kind: "gone"; order: PendingOrder }
  | { kind: "failed"; order: PendingOrder; error: unknown };

export interface OrdersFields {
  /** The pending orders, newest first, as the server listed them (minus any known to be closed). */
  orders: PendingOrder[];
  /** `error` only while there is nothing to show; a failed refresh keeps `ready` and sets `error`. */
  status: OrdersStatus;
  error: Error | null;
  /** Orders whose cancellation is in flight: hidden from the list until the server answers. */
  cancelling: string[];
  notice: OrdersNotice | null;
}

export interface OrdersActions {
  /** Loads the list. Cancels an earlier load still in flight. */
  load: (fetchOrders: OrdersFetcher) => Promise<void>;
  /** Cancels an order on the list, optimistically. Does nothing for one unknown or already being cancelled. */
  cancel: (orderId: string, cancelOrder: OrderCanceller) => Promise<void>;
  dismissNotice: () => void;
  /** Clears everything (sign-out, user switch) and cancels a load in flight. */
  reset: () => void;
}

export type OrdersState = OrdersFields & OrdersActions;

const INITIAL: OrdersFields = { orders: [], status: "idle", error: null, cancelling: [], notice: null };

/** The rows to show: the pending orders but those being cancelled. */
export function visibleOrders(state: Pick<OrdersFields, "orders" | "cancelling">): PendingOrder[] {
  return state.cancelling.length === 0
    ? state.orders
    : state.orders.filter((order) => !state.cancelling.includes(order.order_id));
}

/** The server says the order is not open any more: it filled or was rejected (409), or it is not there (404). */
function isGone(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const code = (error as { code?: unknown }).code;
  return code === "ORDEN_NO_PENDIENTE" || code === "NO_ENCONTRADO";
}

let inflight: AbortController | null = null;
/** Orders this session closed (or learned were closed): a load that started before must not bring them back. */
const closed = new Set<string>();

export const useOrdersStore = create<OrdersState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchOrders) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    // Keep what is on screen during a refresh: the loading state is for the first load only.
    if (get().status !== "ready") set({ status: "loading", error: null });
    try {
      const body = await fetchOrders(controller.signal);
      if (controller.signal.aborted) return;
      const parsed = parsePendingOrders(body);
      if (!parsed) throw new Error("The open orders could not be read.");
      const orders = parsed.orders.filter((order) => order.status === "pending" && !closed.has(order.order_id));
      set({ orders, status: "ready", error: null });
    } catch (cause) {
      if (controller.signal.aborted) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().status === "ready" ? "ready" : "error", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  cancel: async (orderId, cancelOrder) => {
    const order = get().orders.find((o) => o.order_id === orderId);
    if (!order || get().cancelling.includes(orderId)) return;
    set((state) => ({ cancelling: [...state.cancelling, orderId], notice: null }));
    const settle = (notice: OrdersNotice, remove: boolean) => {
      if (remove) closed.add(orderId);
      set((state) => ({
        orders: remove ? state.orders.filter((o) => o.order_id !== orderId) : state.orders,
        cancelling: state.cancelling.filter((id) => id !== orderId),
        notice,
      }));
    };
    try {
      const body = await cancelOrder(orderId);
      // A 2xx means it is cancelled (a repeat answers the same cancelled order);
      // an answer that cannot be read still is one, so the order goes either way.
      const answer = parsePendingOrder(body);
      if (answer && answer.order_id === orderId && answer.status !== "cancelled") {
        settle({ kind: "gone", order }, true);
        return;
      }
      settle({ kind: "cancelled", order }, true);
    } catch (error) {
      if (isGone(error)) settle({ kind: "gone", order }, true);
      else settle({ kind: "failed", order, error }, false);
    }
  },

  dismissNotice: () => set({ notice: null }),

  reset: () => {
    inflight?.abort();
    inflight = null;
    closed.clear();
    set({ ...INITIAL });
  },
}));
