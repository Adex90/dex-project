"use client";

/**
 * Savings plans: the client's active and paused plans
 * (`GET /api/v1/market/savings-plans`), for the Vault's Savings plans panel
 * (components/markets/savings-plans-list.tsx).
 *
 * - `load` validates the list whole before anything renders it. A failed
 *   refresh keeps what is on screen and sets `error`. The list reloads
 *   whenever the vault moves (the WebSocket's new version): every purchase,
 *   pause, resume and cancellation bumps it, so a plan created on another
 *   device appears and each purchase shows up as it happens.
 * - `act` (pause, resume, cancel) is optimistic: the row changes at once
 *   (`changing` overlays it: paused, resumed, or gone when cancelled) and the
 *   action goes to the server, whose answer replaces the row (a cancelled plan
 *   leaves the list). A plan the server says is not there any more (404, or
 *   409 `PLAN_CANCELADO` when another session cancelled it) leaves too; any
 *   other failure brings the row back as it was. Each outcome leaves a
 *   `notice`. A load that started before an action settled is dropped, so it
 *   cannot bring back the row as it was.
 *
 * Client-only, like the vault store: never load during server rendering, and
 * `reset()` on sign-out. The fetchers are injected, so `node --test` runs it.
 */

import { create } from "zustand";

import { applyAction, parseSavingsPlan, parseSavingsPlans } from "../lib/savings-plans.ts";
import type { PlanAction, SavingsPlan } from "../types/market.ts";

/** Fetches the plans and resolves with the parsed JSON body; the store validates it. */
export type PlansFetcher = (signal: AbortSignal) => Promise<unknown>;
/** Pauses, resumes or cancels one plan and resolves with the parsed JSON body (the plan as it stands). */
export type PlanChanger = (planId: string, action: PlanAction) => Promise<unknown>;

export type PlansStatus = "idle" | "loading" | "ready" | "error";

/** What the last action did, for the panel to say. */
export type PlansNotice =
  /** `plan`: as the server answered (its next date, once resumed). */
  | { kind: "done"; action: PlanAction; plan: SavingsPlan }
  /** The plan is not there any more (cancelled elsewhere); `plan` as it was. */
  | { kind: "gone"; plan: SavingsPlan }
  | { kind: "failed"; action: PlanAction; plan: SavingsPlan; error: unknown };

export interface PlansFields {
  /** The active and paused plans, newest first, as the server last answered them. */
  plans: SavingsPlan[];
  /** `error` only while there is nothing to show; a failed refresh keeps `ready` and sets `error`. */
  status: PlansStatus;
  error: Error | null;
  /** Actions in flight, by plan: the row shows their outcome until the server answers. */
  changing: Record<string, PlanAction>;
  notice: PlansNotice | null;
}

export interface PlansActions {
  /** Loads the list. Cancels an earlier load still in flight. */
  load: (fetchPlans: PlansFetcher) => Promise<void>;
  /** Pauses, resumes or cancels a listed plan, optimistically. Does nothing for one unknown or already changing. */
  act: (planId: string, action: PlanAction, changePlan: PlanChanger) => Promise<void>;
  dismissNotice: () => void;
  /** Clears everything (sign-out, user switch) and cancels a load in flight. */
  reset: () => void;
}

export type PlansState = PlansFields & PlansActions;

const INITIAL: PlansFields = { plans: [], status: "idle", error: null, changing: {}, notice: null };

/** The rows to show: each plan as its action in flight leaves it, without those being cancelled. */
export function visiblePlans(state: Pick<PlansFields, "plans" | "changing">): SavingsPlan[] {
  return state.plans.flatMap((plan) => {
    const action = state.changing[plan.plan_id];
    if (action === undefined) return [plan];
    return action === "cancel" ? [] : [applyAction(plan, action)];
  });
}

/** The server says the plan is gone: cancelled elsewhere (409), or not there (404). */
function isGone(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const code = (error as { code?: unknown }).code;
  return code === "PLAN_CANCELADO" || code === "NO_ENCONTRADO";
}

let inflight: AbortController | null = null;
/** Bumped each time an action settles: a load that started before it is out of date. */
let epoch = 0;
/** Plans this session cancelled (or learned were cancelled): no load brings them back. */
const closed = new Set<string>();

export const useSavingsPlansStore = create<PlansState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchPlans) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    const started = epoch;
    // Keep what is on screen during a refresh: the loading state is for the first load only.
    if (get().status !== "ready") set({ status: "loading", error: null });
    try {
      const body = await fetchPlans(controller.signal);
      if (controller.signal.aborted || started !== epoch) return;
      const parsed = parseSavingsPlans(body);
      if (!parsed) throw new Error("The savings plans could not be read.");
      const plans = parsed.plans.filter((plan) => plan.status !== "cancelled" && !closed.has(plan.plan_id));
      set({ plans, status: "ready", error: null });
    } catch (cause) {
      if (controller.signal.aborted || started !== epoch) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().status === "ready" ? "ready" : "error", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  act: async (planId, action, changePlan) => {
    const plan = get().plans.find((p) => p.plan_id === planId);
    if (!plan || get().changing[planId] !== undefined) return;
    set((state) => ({ changing: { ...state.changing, [planId]: action }, notice: null }));
    const settle = (next: SavingsPlan | null, notice: PlansNotice) => {
      epoch += 1;
      if (next === null) closed.add(planId);
      set((state) => {
        const changing = { ...state.changing };
        delete changing[planId];
        const plans =
          next === null
            ? state.plans.filter((p) => p.plan_id !== planId)
            : state.plans.map((p) => (p.plan_id === planId ? next : p));
        return { plans, changing, notice };
      });
    };
    try {
      const body = await changePlan(planId, action);
      // A 2xx means it was done (a repeat answers the same plan). An answer
      // that cannot be read still is one: the row shows the action's outcome
      // until the next load brings the plan as it stands.
      const answer = parseSavingsPlan(body);
      const next = answer && answer.plan_id === planId ? answer : applyAction(plan, action);
      settle(next.status === "cancelled" ? null : next, { kind: "done", action, plan: next });
    } catch (error) {
      if (isGone(error)) settle(null, { kind: "gone", plan });
      else settle(plan, { kind: "failed", action, plan, error });
    }
  },

  dismissNotice: () => set({ notice: null }),

  reset: () => {
    inflight?.abort();
    inflight = null;
    epoch += 1;
    closed.clear();
    set({ ...INITIAL, changing: {} });
  },
}));
