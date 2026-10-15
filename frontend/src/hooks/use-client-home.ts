"use client";

import { useCallback, useEffect, useRef } from "react";
import { useRouter } from "next/navigation";

import { useSession } from "@/hooks/use-session";
import { request } from "@/lib/api/client";
import type { Cliente, EstatusKyc } from "@/lib/api/types";

export const DASHBOARD_PATH = "/dashboard";
export const ONBOARDING_PATH = "/auth/onboarding";

/**
 * Where an account holder belongs: the dashboard once their identity is
 * VERIFICADO, the onboarding wizard otherwise (pending, in review, rejected
 * or suspended: the wizard shows the state of their KYC file).
 */
export function homeForKycStatus(status: EstatusKyc): string {
  return status === "VERIFICADO" ? DASHBOARD_PATH : ONBOARDING_PATH;
}

/**
 * Reads the client's current KYC status (GET /api/v1/yo) and returns their
 * home. If the status cannot be read, the dashboard: it reads the status
 * again on its own and keeps money movements locked until it is VERIFICADO.
 */
export async function resolveClientHome(token: string, signal?: AbortSignal): Promise<string> {
  try {
    const me = await request<Cliente>("/api/v1/yo", { token, signal });
    return homeForKycStatus(me.estatus_kyc);
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    return DASHBOARD_PATH;
  }
}

/**
 * For the sign-in and sign-up pages: sends a visitor who already holds a
 * valid client session to their home. A form that signs in calls `claim()`
 * before it starts and navigates by itself afterwards, so the form and this
 * redirect never race; `release()` hands control back if signing in fails.
 */
export function useClientHomeRedirect() {
  const router = useRouter();
  const { valid, role, token } = useSession();
  const claimed = useRef(false);

  useEffect(() => {
    if (claimed.current || !valid || role !== "CLIENTE" || !token) return;
    const controller = new AbortController();
    resolveClientHome(token, controller.signal).then(
      (path) => {
        if (!controller.signal.aborted && !claimed.current) router.replace(path);
      },
      () => undefined,
    );
    return () => controller.abort();
  }, [valid, role, token, router]);

  const claim = useCallback(() => {
    claimed.current = true;
  }, []);
  const release = useCallback(() => {
    claimed.current = false;
  }, []);

  return { claim, release };
}
