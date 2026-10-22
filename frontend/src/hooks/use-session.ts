"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";

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
 * Current session. Logs out automatically when the JWT expires and, if
 * `redirect` is set, sends unauthenticated visitors to the login page.
 */
export function useSession({ redirect = false }: { redirect?: boolean } = {}) {
  const router = useRouter();
  const token = useAuthStore((s) => s.token);
  const expiresAt = useAuthStore((s) => s.expiresAt);
  const role = useAuthStore((s) => s.role);
  const email = useAuthStore((s) => s.email);
  const logout = useAuthStore((s) => s.logout);
  const hydrated = useHydrated();

  const valid = hydrated && isSessionValid({ token, expiresAt });

  useEffect(() => {
    if (!hydrated) return;
    if (token && !isSessionValid({ token, expiresAt })) {
      logout();
      if (redirect) router.replace("/auth/login?expired=1");
      return;
    }
    if (!token) {
      if (redirect) router.replace("/auth/login");
      return;
    }
    const remaining = new Date(expiresAt ?? 0).getTime() - Date.now();
    const timer = setTimeout(() => {
      logout();
      router.replace("/auth/login?expired=1");
    }, Math.max(0, Math.min(remaining, 2 ** 31 - 1)));
    return () => clearTimeout(timer);
  }, [hydrated, token, expiresAt, redirect, logout, router]);

  return { hydrated, valid, token: valid ? token : null, role, email, logout };
}
