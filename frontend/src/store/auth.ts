"use client";

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";

import { api } from "@/lib/api/endpoints";
import type { ConfiguracionMfa, RegistroInput, RegistroRespuesta, ResultadoLogin, Rol, Sesion } from "@/lib/api/types";
import { isSessionUsable } from "@/lib/session";

/**
 * A sign-in that still needs its second step: the authenticator code
 * ("verify"), or setting up two-step verification first ("enroll", staff
 * only). Kept in memory, never in sessionStorage: a reload starts over.
 */
export interface PendingSignIn {
  kind: "verify" | "enroll";
  token: string;
  expiresAt: string;
  email: string;
}

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
  /** ...by the inactivity timeout in particular. Not persisted. */
  idle: boolean;
  /** ...by a lockout after too many failed security checks (the server closed it). Not persisted. */
  locked: boolean;
  /** Not persisted. */
  pending: PendingSignIn | null;
  /** First step: a session, or the second step still to complete (see `pending`). */
  login: (email: string, password: string) => Promise<Sesion | PendingSignIn>;
  /** Second step: the code from the authenticator app. */
  verifyMfa: (code: string) => Promise<Sesion>;
  /** Generates the secret for the authenticator app (with the enrollment token, or the session). */
  setupMfa: () => Promise<ConfiguracionMfa>;
  /** Confirms the first code: two-step verification is on and the session starts. */
  enableMfa: (code: string) => Promise<Sesion>;
  clearPending: () => void;
  register: (input: RegistroInput) => Promise<RegistroRespuesta>;
  /** Signing out: this tab forgets the session and the server ends it (its token stops working). */
  logout: () => void;
  /** The session expired or the API answered 401: same as logout, but the sign-in page says why. */
  expire: () => void;
  /** No activity for the client's inactivity timeout: ends the session on the server too, with the notice. */
  expireIdle: () => void;
  /** A security check locked the account and the server already closed this session: the notice says so. */
  expireLocked: () => void;
}

/** Ends a session on the server; best effort: a session the server already ended needs nothing more. */
function endOnServer(token: string | null): void {
  if (token) void api.logout(token).catch(() => undefined);
}

const EMPTY = { token: null, expiresAt: null, clientId: null, role: null, email: null } as const;

function sessionOf(result: ResultadoLogin): Sesion {
  if (result.estado !== "OK") throw new Error(`Unexpected sign-in state: ${result.estado}`);
  const { token, expira_en, cliente_id, rol } = result;
  return { token, expira_en, cliente_id, rol };
}

export const useAuthStore = create<AuthState>()(
  persist(
    (set, get) => {
      const start = (session: Sesion, email: string) =>
        set({
          token: session.token,
          expiresAt: session.expira_en,
          clientId: session.cliente_id,
          role: session.rol,
          email,
          expired: false,
          idle: false,
          locked: false,
          pending: null,
        });
      /** Token for the enrollment endpoints: the pending enrollment, or the current session. */
      const enrollmentToken = (): string => {
        const { pending, token } = get();
        const t = pending?.kind === "enroll" ? pending.token : token;
        if (!t) throw new Error("Sign in again to set up two-step verification.");
        return t;
      };

      return {
        ...EMPTY,
        expired: false,
        idle: false,
        locked: false,
        pending: null,

        login: async (email, password) => {
          const normalized = email.trim().toLowerCase();
          const result = await api.login(email.trim(), password);
          if (result.estado === "OK") {
            const session = sessionOf(result);
            start(session, normalized);
            return session;
          }
          const pending: PendingSignIn =
            result.estado === "2FA_REQUIRED"
              ? { kind: "verify", token: result.token_mfa, expiresAt: result.expira_en, email: normalized }
              : { kind: "enroll", token: result.token_inscripcion, expiresAt: result.expira_en, email: normalized };
          set({ ...EMPTY, expired: false, idle: false, locked: false, pending });
          return pending;
        },

        verifyMfa: async (code) => {
          const pending = get().pending;
          if (pending?.kind !== "verify") throw new Error("Sign in again to enter your code.");
          const session = sessionOf(await api.verifyMfa(pending.token, code));
          start(session, pending.email);
          return session;
        },

        setupMfa: () => api.mfaSetup(enrollmentToken()),

        enableMfa: async (code) => {
          const email = get().pending?.email ?? get().email ?? "";
          const session = sessionOf(await api.mfaEnable(enrollmentToken(), code));
          start(session, email);
          return session;
        },

        clearPending: () => set({ pending: null }),

        register: async (input) => {
          const result = await api.register(input);
          set({
            token: result.sesion.token,
            expiresAt: result.sesion.expira_en,
            clientId: result.sesion.cliente_id,
            role: result.sesion.rol,
            email: result.cliente.email,
            expired: false,
            idle: false,
            locked: false,
            pending: null,
          });
          return result;
        },

        logout: () => {
          const token = get().token;
          set({ ...EMPTY, expired: false, idle: false, locked: false, pending: null });
          endOnServer(token);
        },
        expire: () => set({ ...EMPTY, expired: true, idle: false, locked: false, pending: null }),
        expireIdle: () => {
          const token = get().token;
          set({ ...EMPTY, expired: true, idle: true, locked: false, pending: null });
          endOnServer(token);
        },
        expireLocked: () => set({ ...EMPTY, expired: true, idle: false, locked: true, pending: null }),
      };
    },
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

/** A pending second step whose token has not expired yet. */
export function isPendingUsable(pending: PendingSignIn | null, now = Date.now()): pending is PendingSignIn {
  return Boolean(pending && new Date(pending.expiresAt).getTime() > now);
}
