"use client";

/**
 * Price alerts (`GET /api/v1/market/alerts`): the active ones and those
 * reached in the last 30 days, for an asset's card
 * (components/markets/price-alerts.tsx), the Vault's Watchlist and the
 * dashboard's notice when a target is reached.
 *
 * - `load` validates the list whole; a failed refresh keeps what is shown.
 *   It reloads whenever the vault moves: setting, cancelling and reaching a
 *   target bump the vault's version.
 * - `create` resolves with the new alert (or rejects with the error for the
 *   card to show); `cancel` is optimistic: the row goes at once, a failure
 *   brings it back and leaves a `notice`; one already reached (409) shows as
 *   reached on the next load.
 *
 * Client-only; `reset()` on sign-out. The fetchers are injected, so
 * `node --test` runs it.
 */

import { create } from "zustand";

import { parsePriceAlert, parsePriceAlerts } from "../lib/price-alerts.ts";
import type { PriceAlert } from "../types/market.ts";

export type AlertsFetcher = (signal: AbortSignal) => Promise<unknown>;
export type AlertCreator = () => Promise<unknown>;
export type AlertCanceller = (alertId: string) => Promise<unknown>;

export type AlertsStatus = "idle" | "loading" | "ready" | "error";

export interface AlertsFields {
  alerts: PriceAlert[];
  status: AlertsStatus;
  error: Error | null;
  /** Cancellations in flight: hidden until the server answers. */
  cancelling: string[];
  /** The last cancellation that failed. */
  notice: { alert: PriceAlert; error: unknown } | null;
}

export interface AlertsActions {
  load: (fetchAlerts: AlertsFetcher) => Promise<void>;
  /** Sends a new alert; resolves with it, or rejects with the error to show. */
  create: (createAlert: AlertCreator) => Promise<PriceAlert>;
  cancel: (alertId: string, cancelAlert: AlertCanceller) => Promise<void>;
  dismissNotice: () => void;
  reset: () => void;
}

export type AlertsState = AlertsFields & AlertsActions;

const INITIAL: AlertsFields = { alerts: [], status: "idle", error: null, cancelling: [], notice: null };

/** The alerts to show: all but those being cancelled. */
export function visibleAlerts(state: Pick<AlertsFields, "alerts" | "cancelling">): PriceAlert[] {
  return state.cancelling.length === 0
    ? state.alerts
    : state.alerts.filter((alert) => !state.cancelling.includes(alert.alert_id));
}

let inflight: AbortController | null = null;
let epoch = 0;

export const usePriceAlertsStore = create<AlertsState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchAlerts) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    const started = epoch;
    if (get().status !== "ready") set({ status: "loading", error: null });
    try {
      const body = await fetchAlerts(controller.signal);
      if (controller.signal.aborted || started !== epoch) return;
      const parsed = parsePriceAlerts(body);
      if (!parsed) throw new Error("The price alerts could not be read.");
      set({ alerts: parsed.alerts.filter((a) => a.status !== "cancelled"), status: "ready", error: null });
    } catch (cause) {
      if (controller.signal.aborted || started !== epoch) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().status === "ready" ? "ready" : "error", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  create: async (createAlert) => {
    const body = await createAlert();
    const alert = parsePriceAlert(body);
    if (!alert) throw new Error("The price target was set, but the answer could not be read. Reload to see it.");
    epoch += 1;
    set((state) => ({ alerts: [alert, ...state.alerts.filter((a) => a.alert_id !== alert.alert_id)] }));
    return alert;
  },

  cancel: async (alertId, cancelAlert) => {
    const alert = get().alerts.find((a) => a.alert_id === alertId);
    if (!alert || get().cancelling.includes(alertId)) return;
    set((state) => ({ cancelling: [...state.cancelling, alertId], notice: null }));
    const settle = (fields: Partial<AlertsFields>, remove: boolean) => {
      epoch += 1;
      set((state) => ({
        ...fields,
        alerts: remove ? state.alerts.filter((a) => a.alert_id !== alertId) : state.alerts,
        cancelling: state.cancelling.filter((id) => id !== alertId),
      }));
    };
    try {
      await cancelAlert(alertId);
      settle({}, true);
    } catch (error) {
      settle({ notice: { alert, error } }, false);
    }
  },

  dismissNotice: () => set({ notice: null }),

  reset: () => {
    inflight?.abort();
    inflight = null;
    epoch += 1;
    set({ ...INITIAL, cancelling: [] });
  },
}));
