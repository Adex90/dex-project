"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";

import { EXPIRED_LOGIN_PATH, guardRedirect } from "@/lib/session";
import { isSessionValid, useAuthStore } from "@/store/auth";

/**
 * True once the persisted session has been read from sessionStorage. Starts
 * false on every first render (server and client alike) so the client's
 * first render matches the server HTML, then flips in an effect.
 */
function useHydrated(): boolean {
  const [hydrated, setHydrated] = useState(false);
  useEffect(() => {
    setHydrated(useAuthStore.persist.hasHydrated());
    return useAuthStore.persist.onFinishHydration(() => setHydrated(true));
  }, []);
  return hydrated;
}

/**
 * Current session. Ends it automatically when the JWT expires and, if
 * `redirect` is set, sends visitors without a usable session to the login
 * page: with the "session expired" notice when it expired or the API answered
 * 401 (`guardRedirect`), so this guard and a 401 handler that already
 * navigated never race to different URLs.
 */
export function useSession({ redirect = false }: { redirect?: boolean } = {}) {
  const router = useRouter();
  const token = useAuthStore((s) => s.token);
  const expiresAt = useAuthStore((s) => s.expiresAt);
  const expired = useAuthStore((s) => s.expired);
  const role = useAuthStore((s) => s.role);
  const email = useAuthStore((s) => s.email);
  const logout = useAuthStore((s) => s.logout);
  const expire = useAuthStore((s) => s.expire);
  const hydrated = useHydrated();

  const valid = hydrated && isSessionValid({ token, expiresAt });

  useEffect(() => {
    if (!hydrated) return;
    if (token && !isSessionValid({ token, expiresAt })) {
      expire();
      if (redirect) router.replace(EXPIRED_LOGIN_PATH);
      return;
    }
    if (!token) {
      const target = guardRedirect({ token, expiresAt, expired });
      if (redirect && target) router.replace(target);
      return;
    }
    const remaining = new Date(expiresAt ?? 0).getTime() - Date.now();
    const timer = setTimeout(() => {
      expire();
      router.replace(EXPIRED_LOGIN_PATH);
    }, Math.max(0, Math.min(remaining, 2 ** 31 - 1)));
    return () => clearTimeout(timer);
  }, [hydrated, token, expiresAt, expired, redirect, expire, router]);

  return { hydrated, valid, token: valid ? token : null, role, email, logout, expire };
}
