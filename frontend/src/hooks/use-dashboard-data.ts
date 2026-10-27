"use client";

import { useCallback, useEffect } from "react";
import { useRouter } from "next/navigation";

import { ApiError } from "@/lib/api/client";
import { POLL_INTERVAL_MS } from "@/lib/config";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useDashboardStore } from "@/store/dashboard";

/**
 * Loads the dashboard and keeps it live: polls while the tab is visible,
 * refreshes when it becomes visible again, and signs out on a 401.
 */
export function useDashboardData(token: string | null) {
  const router = useRouter();
  const refreshStore = useDashboardStore((s) => s.refresh);
  const loadRates = useDashboardStore((s) => s.loadRates);
  const reset = useDashboardStore((s) => s.reset);

  const refresh = useCallback(async () => {
    if (!token) return;
    try {
      await refreshStore(token);
    } catch (error) {
      if (error instanceof ApiError && error.isUnauthorized) {
        useAuthStore.getState().expire();
        router.replace(EXPIRED_LOGIN_PATH);
      }
    }
  }, [token, refreshStore, router]);

  useEffect(() => {
    if (!token) return;
    void refresh();
    loadRates().catch(() => undefined);
    const tick = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    const id = setInterval(tick, POLL_INTERVAL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [token, refresh, loadRates]);

  useEffect(() => () => reset(), [reset]);

  return { refresh };
}
