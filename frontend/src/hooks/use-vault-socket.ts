"use client";

import { useEffect, useRef } from "react";

import { API_URL } from "@/lib/config";
import { isSessionUsable } from "@/lib/session";
import { VaultSocket, vaultSocketUrl } from "@/lib/vault-socket";
import { useAuthStore } from "@/store/auth";
import { useVaultStore } from "@/store/vault";

/** Advances that land within this window reload the dashboard once. */
const ADVANCE_DEBOUNCE_MS = 250;

export interface VaultSocketCallbacks {
  /** The vault reached a newer version (a deposit, an investment): reload what the stream does not carry. */
  onAdvance: () => void;
  /** The server ended the session (close 4001) or the token expired: sign in again. */
  onSessionEnded: () => void;
}

/**
 * Keeps the Sovereign Vault live while the dashboard is open: one WebSocket
 * to `/api/v1/vault/ws` (src/lib/vault-socket.ts) feeding `useVaultStore`.
 *
 * - Every frame goes straight to the store's `receive`, which enforces the
 *   snapshot-first, in-order protocol; a gap asks the server for a resync.
 * - Drops and deploys reconnect with jittered backoff; coming back online or
 *   to the tab reconnects at once.
 * - Close 4001 (session expired or revoked) ends the session instead.
 * - Opens once per token: switching dashboard tabs keeps the same socket.
 */
export function useVaultSocket(token: string | null, callbacks: VaultSocketCallbacks) {
  // The latest callbacks, without reconnecting when their identity changes.
  const latest = useRef(callbacks);
  useEffect(() => {
    latest.current = callbacks;
  });

  useEffect(() => {
    if (!token) return;
    let debounce: ReturnType<typeof setTimeout> | undefined;
    const store = useVaultStore.getState;
    const channel = new VaultSocket({
      url: vaultSocketUrl(API_URL),
      token,
      receive: (data) => store().receive(data),
      currentSeq: () => store().lastSeq,
      setStatus: (status) => store().setRealtimeStatus(status),
      onAdvance: () => {
        clearTimeout(debounce);
        debounce = setTimeout(() => latest.current.onAdvance(), ADVANCE_DEBOUNCE_MS);
      },
      onSessionEnded: () => latest.current.onSessionEnded(),
      isTokenExpired: () => !isSessionUsable(useAuthStore.getState()),
    });
    channel.start();

    const wake = () => {
      if (document.visibilityState === "visible") channel.wake();
    };
    window.addEventListener("online", wake);
    document.addEventListener("visibilitychange", wake);
    return () => {
      window.removeEventListener("online", wake);
      document.removeEventListener("visibilitychange", wake);
      clearTimeout(debounce);
      channel.stop();
    };
  }, [token]);
}
