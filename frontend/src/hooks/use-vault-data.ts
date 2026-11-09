"use client";

import { useCallback, useEffect } from "react";
import { useRouter } from "next/navigation";

import { ApiError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { POLL_INTERVAL_MS } from "@/lib/config";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useVaultStore } from "@/store/vault";

/**
 * Loads the Sovereign Vault (GET /api/v1/vault) into `useVaultStore`: the
 * first paint, and the fallback whenever the WebSocket (`useVaultSocket`) is
 * not live. It polls while the tab is visible and the stream is down,
 * refreshes when the tab becomes visible again, and signs out on a 401. While
 * the stream is live a poll would change nothing (the store keeps the
 * stream's newer state), so none is sent. The store is cleared when the
 * dashboard unmounts (sign-out), so the next person in this tab never sees
 * the previous one's vault.
 */
export function useVaultData(token: string | null) {
  const router = useRouter();
  const load = useVaultStore((s) => s.load);
  const reset = useVaultStore((s) => s.reset);

  const refresh = useCallback(async () => {
    if (!token) return;
    await load((signal) => api.vault(token, signal));
    const { error } = useVaultStore.getState();
    if (error instanceof ApiError && error.isUnauthorized) {
      useAuthStore.getState().expire();
      router.replace(EXPIRED_LOGIN_PATH);
    }
  }, [token, load, router]);

  useEffect(() => {
    if (!token) return;
    void refresh();
    const tick = () => {
      if (document.visibilityState === "visible" && useVaultStore.getState().realtime !== "live") void refresh();
    };
    const id = setInterval(tick, POLL_INTERVAL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [token, refresh]);

  useEffect(() => () => reset(), [reset]);

  return { refresh };
}
