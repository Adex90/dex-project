"use client";

import { useCallback } from "react";
import { useRouter } from "next/navigation";

import { ApiError } from "@/lib/api/client";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";

/**
 * For a request's `.catch`: a 401 ends the session (to the login page, with
 * the "session expired" notice), as on every dashboard page; any error is
 * rethrown for the caller to handle.
 */
export function useSignOutOn401(): (error: unknown) => never {
  const router = useRouter();
  return useCallback(
    (error: unknown): never => {
      if (error instanceof ApiError && error.isUnauthorized) {
        useAuthStore.getState().expire();
        router.replace(EXPIRED_LOGIN_PATH);
      }
      throw error;
    },
    [router],
  );
}
