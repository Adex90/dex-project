"use client";

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { api } from "@/lib/api/endpoints";
import type { RegistroInput, RegistroRespuesta, Rol, Sesion } from "@/lib/api/types";

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
  login: (email: string, password: string) => Promise<Sesion>;
  register: (input: RegistroInput) => Promise<RegistroRespuesta>;
  logout: () => void;
}

const EMPTY = { token: null, expiresAt: null, clientId: null, role: null, email: null } as const;

export const useAuthStore = create<AuthState>()(
  persist(
    (set) => ({
      ...EMPTY,

      login: async (email, password) => {
        const session = await api.login(email.trim(), password);
        set({
          token: session.token,
          expiresAt: session.expira_en,
          clientId: session.cliente_id,
          role: session.rol,
          email: email.trim().toLowerCase(),
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
        });
        return result;
      },

      logout: () => set({ ...EMPTY }),
    }),
    {
      name: "synthetic-capital-session",
      storage: createJSONStorage(() => sessionStorage),
      partialize: ({ token, expiresAt, clientId, role, email }) => ({ token, expiresAt, clientId, role, email }),
    },
  ),
);

export function isSessionValid(state: Pick<AuthState, "token" | "expiresAt">, now = Date.now()): boolean {
  return Boolean(state.token && state.expiresAt && new Date(state.expiresAt).getTime() > now);
}
