"use client";

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { api } from "@/lib/api/endpoints";
import type { RegistroInput, RegistroRespuesta, Rol, Sesion } from "@/lib/api/types";
import { isSessionUsable } from "@/lib/session";

/**
 * Session state. The JWT is kept in sessionStorage (cleared when the tab
 * closes) rather than localStorage, and is discarded as soon as it expires.
 */
interface AuthState {
  token: string | null;
  expiresAt: string | null;
  clientId: string | null;
  role: Rol | null;
  email: string | null;
  /** The last session ended by expiry or by a 401, not by signing out (see `guardRedirect`). Not persisted. */
  expired: boolean;
  login: (email: string, password: string) => Promise<Sesion>;
  register: (input: RegistroInput) => Promise<RegistroRespuesta>;
  /** Signing out. */
  logout: () => void;
  /** The session expired or the API answered 401: same as logout, but the sign-in page says why. */
  expire: () => void;
}

const EMPTY = { token: null, expiresAt: null, clientId: null, role: null, email: null } as const;

export const useAuthStore = create<AuthState>()(
  persist(
    (set) => ({
      ...EMPTY,
      expired: false,

      login: async (email, password) => {
        const session = await api.login(email.trim(), password);
        set({
          token: session.token,
          expiresAt: session.expira_en,
          clientId: session.cliente_id,
          role: session.rol,
          email: email.trim().toLowerCase(),
          expired: false,
        });
        return session;
      },

      register: async (input) => {
        const result = await api.register(input);
        set({
          token: result.sesion.token,
          expiresAt: result.sesion.expira_en,
          clientId: result.sesion.cliente_id,
          role: result.sesion.rol,
          email: result.cliente.email,
          expired: false,
        });
        return result;
      },

      logout: () => set({ ...EMPTY, expired: false }),
      expire: () => set({ ...EMPTY, expired: true }),
    }),
    {
      name: "synthetic-capital-session",
      storage: createJSONStorage(() => sessionStorage),
      partialize: ({ token, expiresAt, clientId, role, email }) => ({ token, expiresAt, clientId, role, email }),
    },
  ),
);

export function isSessionValid(state: Pick<AuthState, "token" | "expiresAt">, now = Date.now()): boolean {
  return isSessionUsable(state, now);
}
