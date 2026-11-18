/**
 * The Security Center (roadmap items 20 and 21, migration 0019), as the
 * screens use it. `lib/security.ts` builds these from the API's answers
 * (`/api/v1/auth/sessions`, `/auth/security/events`, `/auth/security/preferences`)
 * after validating them.
 */

/** Minutes without activity after which a session signs out. */
export type InactivityMinutes = 5 | 15 | 30 | 60;

/** One signed-in device. */
export interface DeviceSession {
  id: string;
  /** "Chrome on macOS": summarized by the server from the browser's User-Agent. */
  device: string;
  /** Where it signed in from; null when unknown. */
  ip: string | null;
  signedInAt: string;
  /** Accurate to about a minute. */
  lastActiveAt: string;
  /** The session ends then at the latest, even if active. */
  expiresAt: string;
  /** It verified the authenticator code. */
  twoStep: boolean;
  /** The session of this browser tab. */
  current: boolean;
}

export interface DeviceSessions {
  /** The current one first, then by most recent activity. */
  sessions: DeviceSession[];
  inactivityMinutes: InactivityMinutes;
}

/** One entry of the security log (immutable on the server). */
export interface SecurityEvent {
  id: number;
  /** The server's code: INICIO_SESION, CONTRASENA_CAMBIADA... */
  kind: string;
  at: string;
  ip: string | null;
  device: string | null;
  /** How many sessions were signed out, or the inactivity minutes chosen. */
  detail: number | null;
  /** Done from this browser tab's session. */
  fromThisSession: boolean;
}

export interface SecurityPreferences {
  inactivityMinutes: InactivityMinutes;
  options: InactivityMinutes[];
  /** Two-step verification (authenticator app) is on. */
  twoStepEnabled: boolean;
}

/** POST /api/v1/auth/password/change. */
export interface PasswordChange {
  current: string;
  next: string;
  /** The authenticator code, when two-step verification is on. */
  code?: string;
}
