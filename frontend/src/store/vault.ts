"use client";

/**
 * Sovereign Vault store: the client's balance, holdings and reserve proof,
 * kept current by REST loads and by the real-time channel.
 *
 * Sources of truth:
 *  1. The WebSocket stream, once it has delivered a `vault.snapshot`. From
 *     then on every message is applied at most once and in `seq` order:
 *     replays are ignored, and a gap marks the state `stale` so the socket
 *     layer can ask the server for a fresh snapshot.
 *  2. REST (`load`): the first paint, and the fallback while the socket is
 *     down. It is ignored while a live stream holds the baseline, so a slow
 *     response can never overwrite newer real-time state.
 *
 * Money is never computed here: every message carries the authoritative
 * totals after the change (see `VaultRealtimeMessage`).
 *
 * Client-only. The store is a module singleton, so on the server it would
 * be shared by every request: never `load`, `hydrate` or `receive` during
 * server rendering, and call `reset()` on sign-out. Nothing is persisted to
 * browser storage.
 *
 * Wiring a socket (sketch). The backend (`GET /api/v1/vault/ws`) takes the
 * session token as a subprotocol entry, since a browser cannot set headers
 * on a WebSocket, and confirms only `sc.vault.v1`:
 *
 *   const socket = new WebSocket(url, ["sc.vault.v1", `bearer.${token}`]);
 *   const vault = useVaultStore.getState();
 *   vault.setRealtimeStatus("connecting");
 *   socket.onopen = () => vault.setRealtimeStatus("live");
 *   socket.onmessage = (event) => {
 *     if (vault.receive(event.data) === "gap") socket.send(JSON.stringify({ type: "resync" }));
 *   };
 *   socket.onclose = () => vault.setRealtimeStatus("reconnecting");
 */

import { create } from "zustand";

import { isZero } from "../lib/decimal.ts";
import { parseRealtimeMessage, parseVaultSnapshot } from "../lib/vault.ts";
import type {
  Holding,
  ReserveProof,
  SpeiTransfer,
  VaultAccount,
  VaultBalance,
  VaultRealtimeMessage,
  VaultSnapshot,
} from "../types/vault.ts";

/** How many pushed SPEI transfers the store keeps for an activity feed or toasts. */
export const MAX_RECENT_TRANSFERS = 20;

export type VaultLoadStatus = "idle" | "loading" | "ready" | "error";
export type RealtimeStatus = "offline" | "connecting" | "live" | "reconnecting";

/**
 * What happened to a real-time message:
 * - `applied`: next in sequence; the state is updated.
 * - `gap`: applied, but at least one earlier message never arrived. `stale`
 *   is now true: ask the server for a new snapshot.
 * - `duplicate`: a delta at or below the last applied `seq`, or a snapshot
 *   below it (a replay); ignored. A snapshot at the current `seq` is applied:
 *   it is how a resync answers, and how a new reserve proof arrives.
 * - `no-baseline`: a delta before the stream's first snapshot; ignored.
 * - `invalid`: failed validation; ignored.
 */
export type ApplyResult = "applied" | "gap" | "duplicate" | "no-baseline" | "invalid";

/**
 * The stream's state as the UI should name it: "live" only once a snapshot
 * has set the baseline. An open socket whose snapshot has not arrived, or was
 * refused, is still connecting: what is on screen came over REST.
 */
export function displayedRealtime(state: { realtime: RealtimeStatus; lastSeq: number | null }): RealtimeStatus {
  return state.realtime === "live" && state.lastSeq === null ? "connecting" : state.realtime;
}

/** Fetches the vault over REST and resolves with the parsed JSON body; the store validates it. */
export type VaultFetcher = (signal: AbortSignal) => Promise<unknown>;

export interface VaultFields {
  /** Owner and SPEI deposit details; null until the first snapshot. */
  account: VaultAccount | null;
  balance: VaultBalance | null;
  reserveProof: ReserveProof | null;
  holdings: Holding[];
  /** SPEI transfers pushed in real time, newest first, at most `MAX_RECENT_TRANSFERS`. */
  recentTransfers: SpeiTransfer[];

  /** `error` only while there is nothing to show; a failed refresh keeps `ready` and sets `error`. */
  status: VaultLoadStatus;
  error: Error | null;
  /** Epoch milliseconds of the last change applied, from REST or the socket. */
  updatedAt: number | null;

  realtime: RealtimeStatus;
  /** `seq` of the last applied message; null until the socket delivers a snapshot. */
  lastSeq: number | null;
  /** Messages were lost: what is shown may miss updates until the next snapshot. */
  stale: boolean;
}

export interface VaultActions {
  /** Loads over REST. Cancels an earlier load still in flight. */
  load: (fetchVault: VaultFetcher) => Promise<void>;
  /**
   * Sets an already-validated snapshot. Returns false, changing nothing, when
   * a live stream holds the baseline (its state is newer).
   */
  hydrate: (snapshot: VaultSnapshot) => boolean;
  /** Any status but `live` drops the stream baseline: the next connection starts from a snapshot. */
  setRealtimeStatus: (status: RealtimeStatus) => void;
  /** Applies a validated message, enforcing the snapshot-first, in-order protocol. */
  applyMessage: (message: VaultRealtimeMessage) => ApplyResult;
  /** Validates and applies a raw WebSocket payload: pass `event.data` straight in. */
  receive: (raw: unknown) => ApplyResult;
  /** Clears everything (sign-out, user switch) and cancels a load in flight. */
  reset: () => void;
}

export type VaultState = VaultFields & VaultActions;

const INITIAL: VaultFields = {
  account: null,
  balance: null,
  reserveProof: null,
  holdings: [],
  recentTransfers: [],
  status: "idle",
  error: null,
  updatedAt: null,
  realtime: "offline",
  lastSeq: null,
  stale: false,
};

type VaultDelta = Exclude<VaultRealtimeMessage, { type: "vault.snapshot" }>;

function fromSnapshot(snapshot: VaultSnapshot): Pick<VaultFields, "account" | "balance" | "reserveProof" | "holdings"> {
  const { user_id, clabe, bank_name, balance, reserve_proof, holdings } = snapshot;
  return { account: { user_id, clabe, bank_name }, balance, reserveProof: reserve_proof, holdings };
}

function transferKey(transfer: SpeiTransfer): string {
  return `${transfer.direction}:${transfer.tracking_key}`;
}

/** Newest first, one entry per transfer (a later status replaces the earlier one), bounded. */
function withTransfer(transfers: SpeiTransfer[], transfer: SpeiTransfer): SpeiTransfer[] {
  const key = transferKey(transfer);
  return [transfer, ...transfers.filter((t) => transferKey(t) !== key)].slice(0, MAX_RECENT_TRANSFERS);
}

/** Replaces a position in place, appends a new one, or drops a closed one (`shares` "0"). */
function withHolding(holdings: Holding[], holding: Holding): Holding[] {
  const index = holdings.findIndex((h) => h.ticker === holding.ticker);
  if (isZero(holding.shares)) return index === -1 ? holdings : holdings.filter((_, i) => i !== index);
  if (index === -1) return [...holdings, holding];
  return holdings.map((h, i) => (i === index ? holding : h));
}

/** The state a delta changes. Totals come from the message, never from client arithmetic. */
function applyDelta(state: VaultFields, message: VaultDelta): Partial<VaultFields> {
  switch (message.type) {
    case "spei.transfer":
      return { balance: message.balance, recentTransfers: withTransfer(state.recentTransfers, message.transfer) };
    case "holding.updated":
      return { balance: message.balance, holdings: withHolding(state.holdings, message.holding) };
    case "reserve.updated":
      return { reserveProof: message.reserve_proof };
  }
}

let inflight: AbortController | null = null;

export const useVaultStore = create<VaultState>()((set, get) => ({
  ...INITIAL,

  load: async (fetchVault) => {
    inflight?.abort();
    const controller = new AbortController();
    inflight = controller;
    // Keep what is on screen during a refresh: the loading state is for the first load only.
    if (get().balance === null) set({ status: "loading", error: null });
    try {
      const body = await fetchVault(controller.signal);
      if (controller.signal.aborted) return;
      const snapshot = parseVaultSnapshot(body);
      if (!snapshot) throw new Error("The vault response does not match the expected format.");
      get().hydrate(snapshot);
    } catch (cause) {
      if (controller.signal.aborted) return;
      const error = cause instanceof Error ? cause : new Error(String(cause));
      set({ status: get().balance === null ? "error" : "ready", error });
    } finally {
      if (inflight === controller) inflight = null;
    }
  },

  hydrate: (snapshot) => {
    if (get().lastSeq !== null) return false;
    set({ ...fromSnapshot(snapshot), status: "ready", error: null, stale: false, updatedAt: Date.now() });
    return true;
  },

  setRealtimeStatus: (status) => set(status === "live" ? { realtime: status } : { realtime: status, lastSeq: null }),

  applyMessage: (message) => {
    const { lastSeq } = get();

    if (message.type === "vault.snapshot") {
      // Same version, same balances: a snapshot at the current seq is sent on
      // purpose (a resync after a gap, a new reserve proof) and must apply,
      // or `stale` would never clear. Only an older one is a replay.
      if (lastSeq !== null && message.seq < lastSeq) return "duplicate";
      set({
        ...fromSnapshot(message.vault),
        lastSeq: message.seq,
        stale: false,
        status: "ready",
        error: null,
        updatedAt: Date.now(),
      });
      return "applied";
    }

    if (lastSeq === null) return "no-baseline";
    if (message.seq <= lastSeq) return "duplicate";
    const gap = message.seq !== lastSeq + 1;
    set((state) => ({
      ...applyDelta(state, message),
      lastSeq: message.seq,
      stale: state.stale || gap,
      updatedAt: Date.now(),
    }));
    return gap ? "gap" : "applied";
  },

  receive: (raw) => {
    const message = parseRealtimeMessage(raw);
    return message ? get().applyMessage(message) : "invalid";
  },

  reset: () => {
    inflight?.abort();
    inflight = null;
    set({ ...INITIAL });
  },
}));
