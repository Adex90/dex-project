/**
 * Where a guarded page sends a visitor whose session is not usable, kept out
 * of the React hooks so `node --test` can check it.
 *
 * A session can end three ways: the JWT reaches its local expiry, the API
 * answers 401 before that (secret rotated, sessions revoked, backend restarted
 * with a new secret), or the person signs out. The first two must land on the
 * sign-in page with the "Your session expired" notice; a sign-out, or no
 * session at all, on the plain sign-in page. Every redirect after a 401 and the
 * layout guard compute the same path, so they never race to different URLs.
 */

export const LOGIN_PATH = "/auth/login";
export const EXPIRED_LOGIN_PATH = "/auth/login?expired=1";

export interface SessionSnapshot {
  token: string | null;
  expiresAt: string | null;
  /** The last session ended by expiry or by a 401 (not by signing out). */
  expired: boolean;
}

/** A token whose local expiry is still ahead. */
export function isSessionUsable(s: Pick<SessionSnapshot, "token" | "expiresAt">, now = Date.now()): boolean {
  return Boolean(s.token && s.expiresAt && new Date(s.expiresAt).getTime() > now);
}

/** The sign-in URL for a session that cannot be used, or null while it can. */
export function guardRedirect(s: SessionSnapshot, now = Date.now()): string | null {
  if (isSessionUsable(s, now)) return null;
  return s.expired || s.token ? EXPIRED_LOGIN_PATH : LOGIN_PATH;
}
